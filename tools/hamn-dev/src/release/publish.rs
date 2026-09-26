//! `publish STABLE_TAG RC_TAG COMMIT INPUT_DIR OUTPUT_DIR`: promotes exact
//! GitHub-hosted candidate bytes without rebuilding them or using a
//! long-lived release key. The release workflow verifies the GitHub
//! attestations of these inputs separately, before this driver runs.
//!
//! Arguments: the stable tag `vX.Y.Z` and its candidate tag `vX.Y.Z-rc.N`
//! (each number `[0-9]+`); `COMMIT`, a ref naming the release commit in the
//! checkout of the working directory; `INPUT_DIR`, holding `hamn-candidate/`
//! (exactly the candidate files) and `hamn-evidence/` (the hosted evidence
//! and `guest-image-size-report.json`); and `OUTPUT_DIR`, an existing empty
//! directory. Environment:
//! - `HAMN_RELEASE_PROVENANCE`: `workflow` (unset or empty) requires the run
//!   that recorded the hosted evidence, as positive decimal
//!   `HAMN_EXPECTED_WORKFLOW_RUN` and `HAMN_EXPECTED_WORKFLOW_ATTEMPT`;
//!   `solo-local` takes neither, expects evidence recorded as `local`, and
//!   is refused when `GITHUB_ACTIONS=true`.
//! - `HAMN_RELEASE_REPOSITORY` (`owner/repository`) makes the artifact URLs
//!   that repository's GitHub Release downloads for the stable tag; without
//!   it, `HAMN_RELEASE_BASE_URL` (HTTPS) is their base.
//! - `HAMN_TEST_RELEASE_SIZE_BUDGET` replaces the reviewed
//!   `guest/image/release-size-budget.json`, for tests only: it requires
//!   `HAMN_RELEASE_ALLOW_LOCAL=1` and an empty `GITHUB_ACTIONS`.
//!
//! Effects: nothing is written to OUTPUT_DIR until every input is checked:
//! the candidate files are owned, single-link regular files matching
//! `SHA256SUMS`; the hosted evidence binds them to COMMIT and the expected
//! run ([`verify_hosted`]); and `guest/build/hamn-image-tool
//! verify-release-size`, which `make -C guest image-tool` builds into the
//! checkout, accepts the guest image, its size report and the budget for
//! COMMIT. OUTPUT_DIR then receives the schema 3 update manifest, which the
//! candidate's own `hamn` must accept, then copies of candidate.json,
//! SHA256SUMS and the hosted evidence and `promoted-from-rc` (the candidate
//! tag), each mode 0644. When the candidate client rejects the manifest, the
//! manifest stays in OUTPUT_DIR and nothing else is written; the release
//! workflow publishes nothing from a failed run. Every child runs under a
//! deadline with our environment plus `LC_ALL=C` (see [`super::checkout`]).
use super::checkout::{Checkout, environment, existing_directory, is_positive_decimal, variable};
use super::files::{Workspace, copy_new, owned_regular, set_mode, write_new};
use super::hosted::{Promotion, checksums_match, verify_hosted, write_manifest};
use super::process::{self, Spec};
use super::syntax::{is_artifact_name, is_digits, is_repository};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const USAGE: &str = "usage: hamn-dev release publish vX.Y.Z vX.Y.Z-rc.N COMMIT INPUT_DIR OUTPUT_DIR";
/// Building the small C image tool from source.
const MAKE_TIMEOUT: Duration = Duration::from_secs(600);
/// Hashing and checking a guest image below the 2 GiB release asset limit.
const SIZE_TIMEOUT: Duration = Duration::from_secs(1800);
const TAR_TIMEOUT: Duration = Duration::from_secs(600);
/// The candidate client only parses the manifest.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(120);

/// Reads one environment variable; unset reads as empty.
type Lookup<'a> = &'a dyn Fn(&str) -> Result<String, String>;

/// `publish`; see the module documentation.
pub fn publish(args: &[String]) -> Result<(), String> {
    promote(args, &variable).map_err(|error| format!("hamn publish: {error}"))
}

/// The arguments and environment, checked before any file is read.
#[derive(Debug, PartialEq, Eq)]
struct Request<'a> {
    stable_tag: &'a str,
    candidate_tag: &'a str,
    reference: &'a str,
    input_dir: &'a str,
    output_dir: &'a str,
    /// The workflow run and attempt the hosted evidence must name.
    run: String,
    attempt: String,
    base_url: String,
}

fn request<'a>(args: &'a [String], lookup: Lookup) -> Result<Request<'a>, String> {
    let [stable_tag, candidate_tag, reference, input_dir, output_dir] = args else {
        return Err(USAGE.into());
    };
    if args.iter().any(String::is_empty) {
        return Err(USAGE.into());
    }
    if !is_release_tag(stable_tag) {
        return Err("stable tag is invalid".into());
    }
    if !is_candidate_of(candidate_tag, stable_tag) {
        return Err("RC tag does not correspond to the stable tag".into());
    }
    let (run, attempt) = expected_run(lookup)?;
    let base_url = base_url(stable_tag, lookup)?;
    Ok(Request { stable_tag, candidate_tag, reference, input_dir, output_dir, run, attempt, base_url })
}

/// `v[0-9]+.[0-9]+.[0-9]+`; numbers need not be canonical here (the
/// candidate client rejects a manifest whose version is not).
fn is_release_tag(tag: &str) -> bool {
    let numbers: Vec<&str> = tag.strip_prefix('v').map_or_else(Vec::new, |version| version.split('.').collect());
    numbers.len() == 3 && numbers.iter().all(|number| is_digits(number))
}

/// `STABLE_TAG-rc.[0-9]+`, with STABLE_TAG taken literally.
fn is_candidate_of(candidate_tag: &str, stable_tag: &str) -> bool {
    candidate_tag.strip_prefix(stable_tag).and_then(|rest| rest.strip_prefix("-rc.")).is_some_and(is_digits)
}

/// The workflow run and attempt that recorded the hosted evidence: the
/// expected positive decimals, or `local` for solo-local provenance.
fn expected_run(lookup: Lookup) -> Result<(String, String), String> {
    let run = lookup("HAMN_EXPECTED_WORKFLOW_RUN")?;
    let attempt = lookup("HAMN_EXPECTED_WORKFLOW_ATTEMPT")?;
    match lookup("HAMN_RELEASE_PROVENANCE")?.as_str() {
        "" | "workflow" => {
            if !is_positive_decimal(&run) {
                return Err("HAMN_EXPECTED_WORKFLOW_RUN must be a positive decimal run ID".into());
            }
            if !is_positive_decimal(&attempt) {
                return Err("HAMN_EXPECTED_WORKFLOW_ATTEMPT must be a positive decimal attempt".into());
            }
            Ok((run, attempt))
        }
        "solo-local" => {
            if lookup("GITHUB_ACTIONS")? == "true" {
                return Err("solo-local provenance is unavailable inside GitHub Actions".into());
            }
            if !run.is_empty() || !attempt.is_empty() {
                return Err("solo-local provenance must not accept workflow run inputs".into());
            }
            Ok(("local".into(), "local".into()))
        }
        _ => Err("HAMN_RELEASE_PROVENANCE must be workflow or solo-local".into()),
    }
}

/// The HTTPS base of the published artifact URLs: the repository's GitHub
/// Release downloads for `stable_tag`, which no configured base may
/// override, or else the configured base.
fn base_url(stable_tag: &str, lookup: Lookup) -> Result<String, String> {
    let repository = lookup("HAMN_RELEASE_REPOSITORY")?;
    let configured = lookup("HAMN_RELEASE_BASE_URL")?;
    let url = if repository.is_empty() {
        if configured.is_empty() {
            return Err("HAMN_RELEASE_BASE_URL is required outside GitHub Actions".into());
        }
        configured
    } else {
        if !is_repository(&repository) {
            return Err("HAMN_RELEASE_REPOSITORY is invalid".into());
        }
        if !configured.is_empty() {
            return Err("HAMN_RELEASE_BASE_URL must not override the canonical GitHub Release base".into());
        }
        format!("https://github.com/{repository}/releases/download/{stable_tag}")
    };
    if !url.starts_with("https://") {
        return Err("release base URL must use HTTPS".into());
    }
    Ok(url)
}

/// The reviewed budget, or the test budget where it is allowed.
fn size_budget(root: &Path, lookup: Lookup) -> Result<PathBuf, String> {
    let test_budget = lookup("HAMN_TEST_RELEASE_SIZE_BUDGET")?;
    if test_budget.is_empty() {
        return Ok(root.join("guest/image/release-size-budget.json"));
    }
    if lookup("HAMN_RELEASE_ALLOW_LOCAL")? != "1" || !lookup("GITHUB_ACTIONS")?.is_empty() {
        return Err("test size budget is forbidden in release workflows".into());
    }
    Ok(PathBuf::from(test_budget))
}

fn promote(args: &[String], lookup: Lookup) -> Result<(), String> {
    let request = request(args, lookup)?;
    let input_dir = existing_directory(request.input_dir, "INPUT_DIR is unsafe")?;
    let output_dir = existing_directory(request.output_dir, "OUTPUT_DIR is unsafe")?;
    let mut entries = fs::read_dir(output_dir).map_err(|error| format!("{}: {error}", output_dir.display()))?;
    if entries.next().is_some() {
        return Err("OUTPUT_DIR must be empty".into());
    }
    let candidate_dir = input_dir.join("hamn-candidate");
    let evidence_dir = input_dir.join("hamn-evidence");
    let is_directory = |path: &Path| fs::symlink_metadata(path).is_ok_and(|info| info.file_type().is_dir());
    if !is_directory(&candidate_dir) || !is_directory(&evidence_dir) {
        return Err("candidate or hosted evidence directory is missing".into());
    }
    let stable_tag = request.stable_tag;
    let host = format!("hamn-{stable_tag}-darwin-arm64.tar.gz");
    let guest = format!("hamn-{stable_tag}-ubuntu-24.04-arm64.img");
    let sbom = format!("hamn-{stable_tag}.spdx.json");
    let installer = "install.sh";
    let candidate = candidate_dir.join("candidate.json");
    let checksums = candidate_dir.join("SHA256SUMS");
    let evidence = evidence_dir.join("hosted-validation-evidence.json");
    let inputs = [
        candidate.clone(),
        checksums.clone(),
        candidate_dir.join(&host),
        candidate_dir.join(&guest),
        candidate_dir.join(&sbom),
        candidate_dir.join(installer),
        evidence.clone(),
    ];
    for path in &inputs {
        owned_regular(path).map_err(|_| format!("unsafe release input: {}", path.display()))?;
    }
    if !checksums_match(&candidate_dir) {
        return Err("candidate artifact hashes do not match".into());
    }

    let checkout = Checkout::current()?;
    let commit = checkout.commit(request.reference).map_err(|_| "RELEASE_REF is not a commit")?;
    let tree = checkout.tree(&commit).map_err(|_| "cannot resolve release source tree")?;
    let promotion = Promotion {
        stable_tag,
        candidate_tag: request.candidate_tag,
        commit: &commit,
        tree: &tree,
        run: &request.run,
        attempt: &request.attempt,
        artifacts: [&host, &guest, &sbom, installer],
    };
    verify_hosted(&candidate_dir, &evidence, &promotion)?;
    let budget = size_budget(&checkout.root, lookup)?;
    let report = evidence_dir.join("guest-image-size-report.json");
    verify_size(&checkout.root, &candidate_dir.join(&guest), &report, &budget, &commit).map_err(|error| {
        format!("guest image size evidence or reviewed release budget is missing or invalid: {error}")
    })?;

    // Only the schema v3 manifest is published; clients up to v0.1.2 read
    // the removed v2 manifest and must reinstall with install.sh.
    let manifest = output_dir.join("hamn-update-manifest-v3.json");
    write_manifest(&manifest, stable_tag, &commit, &request.base_url, &candidate_dir, &host, &guest)?;
    validate_manifest(&candidate_dir.join(&host), &manifest)?;
    set_mode(&manifest, 0o644)?;
    copy_new(&candidate, &output_dir.join("candidate.json"), 0o644)?;
    copy_new(&checksums, &output_dir.join("SHA256SUMS"), 0o644)?;
    copy_new(&evidence, &output_dir.join("hosted-validation-evidence.json"), 0o644)?;
    let promoted = output_dir.join("promoted-from-rc");
    write_new(&promoted, format!("{}\n", request.candidate_tag).as_bytes())?;
    set_mode(&promoted, 0o644)?;
    println!("verified hosted candidate {}; publish exact bytes without rebuilding", request.candidate_tag);
    Ok(())
}

/// Builds the guest image tool with `make -C ROOT/guest image-tool`, then
/// runs `guest/build/hamn-image-tool verify-release-size IMAGE REPORT BUDGET
/// COMMIT`, which accepts only non-review-only evidence of exactly these
/// image bytes, from COMMIT, within the budget. Both keep their diagnostics
/// on standard error; make's standard output is discarded.
fn verify_size(root: &Path, image: &Path, report: &Path, budget: &Path, commit: &str) -> Result<(), String> {
    let environment = environment(&[])?;
    let spec = Spec { environment: Some(&environment), ..Spec::default() };
    let guest = root.join("guest");
    let build: [&OsStr; 5] =
        ["-s".as_ref(), "--no-print-directory".as_ref(), "-C".as_ref(), guest.as_os_str(), "image-tool".as_ref()];
    process::run_passing_stderr(OsStr::new("make"), &build, &spec, MAKE_TIMEOUT)?;
    let tool = guest.join("build/hamn-image-tool");
    let verify: [&OsStr; 5] =
        ["verify-release-size".as_ref(), image.as_os_str(), report.as_os_str(), budget.as_os_str(), commit.as_ref()];
    print!("{}", process::run_passing_stderr(tool.as_os_str(), &verify, &spec, SIZE_TIMEOUT)?);
    Ok(())
}

/// Validates the manifest with the exact candidate client's parser (schema,
/// HTTPS URLs and compatibility) instead of a second implementation:
/// `hamn __install-support upgrade fields MANIFEST` must succeed. The
/// archive's digest was verified; only its executable is extracted, as
/// install.sh does, into a private directory that is always removed.
fn validate_manifest(host_archive: &Path, manifest: &Path) -> Result<(), String> {
    let validator = Workspace::create(&std::env::temp_dir(), "hamn-publish-validator.")
        .map_err(|error| format!("cannot create manifest validator workspace: {error}"))?;
    let environment = environment(&[])?;
    let spec = Spec { environment: Some(&environment), ..Spec::default() };
    let list: [&OsStr; 2] = ["-tzf".as_ref(), host_archive.as_os_str()];
    let listing = process::run(OsStr::new("tar"), &list, &spec, TAR_TIMEOUT)
        .map_err(|error| format!("cannot list the candidate host archive: {error}"))?;
    let member = executable_member(&listing)?;
    let extract: [&OsStr; 4] = ["-xzOf".as_ref(), host_archive.as_os_str(), "--".as_ref(), member.as_ref()];
    let unreadable = |detail: &str| format!("cannot read the candidate executable: {detail}");
    let executable =
        process::capture(OsStr::new("tar"), &extract, &spec, TAR_TIMEOUT).map_err(|error| unreadable(&error))?;
    if !executable.status.success() {
        return Err(unreadable(executable.stderr_lossy().trim()));
    }
    let client = validator.path().join("hamn");
    write_new(&client, &executable.stdout)?;
    set_mode(&client, 0o700)?;
    let fields: [&OsStr; 4] = ["__install-support".as_ref(), "upgrade".as_ref(), "fields".as_ref(), manifest.as_ref()];
    process::run_passing_stderr(client.as_os_str(), &fields, &spec, CLIENT_TIMEOUT)
        .map_err(|error| format!("the candidate client rejects the generated v3 manifest: {error}"))?;
    validator.remove()
}

/// The one `ROOT/bin/hamn` member, ROOT a plain name, of a `tar -t`
/// listing (one member per `\n`-terminated line).
fn executable_member(listing: &str) -> Result<&str, String> {
    let mut executables =
        listing.split('\n').filter(|member| member.strip_suffix("/bin/hamn").is_some_and(is_artifact_name));
    let member = executables.next().ok_or("candidate host archive has no executable")?;
    if executables.next().is_some() {
        return Err("candidate host archive has duplicate executables".into());
    }
    Ok(member)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const RC: &str = "v0.0.1-rc.7";

    fn arguments(stable: &str, candidate: &str) -> Vec<String> {
        [stable, candidate, "HEAD", "input", "output"].map(str::to_owned).to_vec()
    }

    /// `request` with the workflow run 41, attempt 2 and repository
    /// example/hamn unless `changes` replaces (or, with "", unsets) them.
    fn checked(args: &[String], changes: &[(&str, &str)]) -> Result<(String, String, String), String> {
        let mut values: BTreeMap<&str, &str> = BTreeMap::from([
            ("HAMN_EXPECTED_WORKFLOW_RUN", "41"),
            ("HAMN_EXPECTED_WORKFLOW_ATTEMPT", "2"),
            ("HAMN_RELEASE_REPOSITORY", "example/hamn"),
        ]);
        values.extend(changes.iter().copied());
        let lookup = |name: &str| Ok(values.get(name).copied().unwrap_or("").to_owned());
        request(args, &lookup).map(|request| (request.run, request.attempt, request.base_url))
    }

    #[test]
    fn arguments_must_be_five_non_empty_values() {
        let valid = arguments("v0.0.1", RC);
        let canonical = "https://github.com/example/hamn/releases/download/v0.0.1".to_owned();
        assert_eq!(checked(&valid, &[]), Ok(("41".into(), "2".into(), canonical)));
        for count in [0, 1, 4] {
            assert_eq!(checked(&valid[..count], &[]).unwrap_err(), USAGE);
        }
        let mut extra = valid.clone();
        extra.push("more".into());
        assert_eq!(checked(&extra, &[]).unwrap_err(), USAGE);
        for index in 0..valid.len() {
            let mut empty = valid.clone();
            empty[index].clear();
            assert_eq!(checked(&empty, &[]).unwrap_err(), USAGE, "argument {index}");
        }
    }

    #[test]
    fn tags_are_a_release_and_one_of_its_candidates() {
        for stable in ["v0.0.1", "v10.20.30", "v01.0.0"] {
            assert!(checked(&arguments(stable, &format!("{stable}-rc.1")), &[]).is_ok(), "{stable}");
        }
        for stable in ["0.0.1", "v0.0", "v0.0.1.1", "v0.0.1-rc.1", "v0.0.a", "V0.0.1", "v0.0.1 ", "v.0.1", "v0..1", "v"]
        {
            assert_eq!(checked(&arguments(stable, RC), &[]).unwrap_err(), "stable tag is invalid", "{stable}");
        }
        for candidate in [
            "v0.0.2-rc.7",
            "v0.0.1-rc.",
            "v0.0.1-rc.7a",
            "v0.0.1",
            "v0.0.1-beta.7",
            "v0x0x1-rc.7",
            "v0.0.1-rc.-7",
            "xv0.0.1-rc.7",
            "v0.0.1-rc.7 ",
        ] {
            let error = checked(&arguments("v0.0.1", candidate), &[]).unwrap_err();
            assert_eq!(error, "RC tag does not correspond to the stable tag", "{candidate}");
        }
        assert!(checked(&arguments("v0.0.1", "v0.0.1-rc.007"), &[]).is_ok());
    }

    #[test]
    fn provenance_is_a_positive_workflow_run_or_solo_local() {
        let args = arguments("v0.0.1", RC);
        for run in ["", "0", "01", "-1", "+1", "1.0", "abc", "local"] {
            let error = checked(&args, &[("HAMN_EXPECTED_WORKFLOW_RUN", run)]).unwrap_err();
            assert_eq!(error, "HAMN_EXPECTED_WORKFLOW_RUN must be a positive decimal run ID", "{run:?}");
            let error = checked(&args, &[("HAMN_EXPECTED_WORKFLOW_ATTEMPT", run)]).unwrap_err();
            assert_eq!(error, "HAMN_EXPECTED_WORKFLOW_ATTEMPT must be a positive decimal attempt", "{run:?}");
        }
        assert_eq!(checked(&args, &[("HAMN_RELEASE_PROVENANCE", "workflow")]).unwrap().0, "41");
        for provenance in ["local", "Workflow", "solo", "solo-local "] {
            let error = checked(&args, &[("HAMN_RELEASE_PROVENANCE", provenance)]).unwrap_err();
            assert_eq!(error, "HAMN_RELEASE_PROVENANCE must be workflow or solo-local", "{provenance:?}");
        }
        let solo = [("HAMN_RELEASE_PROVENANCE", "solo-local")];
        let without_run = [solo[0], ("HAMN_EXPECTED_WORKFLOW_RUN", ""), ("HAMN_EXPECTED_WORKFLOW_ATTEMPT", "")];
        let (run, attempt, _) = checked(&args, &without_run).unwrap();
        assert_eq!((run.as_str(), attempt.as_str()), ("local", "local"));
        for inputs in [&[("HAMN_EXPECTED_WORKFLOW_ATTEMPT", "")][..], &[("HAMN_EXPECTED_WORKFLOW_RUN", "")], &[]] {
            let changes: Vec<(&str, &str)> = solo.iter().chain(inputs).copied().collect();
            let error = checked(&args, &changes).unwrap_err();
            assert_eq!(error, "solo-local provenance must not accept workflow run inputs", "{inputs:?}");
        }
        let inside = [without_run.as_slice(), &[("GITHUB_ACTIONS", "true")]].concat();
        assert_eq!(checked(&args, &inside).unwrap_err(), "solo-local provenance is unavailable inside GitHub Actions");
        // Only GITHUB_ACTIONS=true, as GitHub sets it, marks a hosted run.
        let other = [without_run.as_slice(), &[("GITHUB_ACTIONS", "false")]].concat();
        assert!(checked(&args, &other).is_ok());
    }

    #[test]
    fn artifact_base_is_the_canonical_release_or_a_configured_https_base() {
        let args = arguments("v0.0.1", RC);
        for repository in ["example", "example/hamn/extra", "exa mple/hamn", "/hamn", "example/", "a/b c"] {
            let error = checked(&args, &[("HAMN_RELEASE_REPOSITORY", repository)]).unwrap_err();
            assert_eq!(error, "HAMN_RELEASE_REPOSITORY is invalid", "{repository}");
        }
        let canonical = "https://github.com/example/hamn/releases/download/v0.0.1";
        let error = checked(&args, &[("HAMN_RELEASE_BASE_URL", canonical)]).unwrap_err();
        assert_eq!(error, "HAMN_RELEASE_BASE_URL must not override the canonical GitHub Release base");
        let local = |base: &str| checked(&args, &[("HAMN_RELEASE_REPOSITORY", ""), ("HAMN_RELEASE_BASE_URL", base)]);
        assert_eq!(local("").unwrap_err(), "HAMN_RELEASE_BASE_URL is required outside GitHub Actions");
        for insecure in ["http://example.invalid/r", "file:///tmp/r", "HTTPS://example.invalid/r", "example.invalid/r"]
        {
            assert_eq!(local(insecure).unwrap_err(), "release base URL must use HTTPS", "{insecure}");
        }
        assert_eq!(local("https://example.invalid/r").unwrap().2, "https://example.invalid/r");
    }

    #[test]
    fn a_test_size_budget_needs_an_explicit_local_run() {
        let root = Path::new("/checkout");
        let reviewed = root.join("guest/image/release-size-budget.json");
        let budget = |values: &[(&str, &str)]| {
            let values: BTreeMap<&str, &str> = values.iter().copied().collect();
            size_budget(root, &|name: &str| Ok(values.get(name).copied().unwrap_or("").to_owned()))
        };
        assert_eq!(budget(&[]).unwrap(), reviewed);
        assert_eq!(budget(&[("GITHUB_ACTIONS", "true"), ("HAMN_TEST_RELEASE_SIZE_BUDGET", "")]).unwrap(), reviewed);
        let test = ("HAMN_TEST_RELEASE_SIZE_BUDGET", "/tmp/budget.json");
        assert_eq!(budget(&[test, ("HAMN_RELEASE_ALLOW_LOCAL", "1")]).unwrap(), Path::new("/tmp/budget.json"));
        for refused in [
            &[test][..],
            &[test, ("HAMN_RELEASE_ALLOW_LOCAL", "true")],
            &[test, ("HAMN_RELEASE_ALLOW_LOCAL", "0")],
            &[test, ("HAMN_RELEASE_ALLOW_LOCAL", "1"), ("GITHUB_ACTIONS", "true")],
            &[test, ("HAMN_RELEASE_ALLOW_LOCAL", "1"), ("GITHUB_ACTIONS", "false")],
        ] {
            assert_eq!(
                budget(refused).unwrap_err(),
                "test size budget is forbidden in release workflows",
                "{refused:?}"
            );
        }
    }

    #[test]
    fn the_host_archive_holds_exactly_one_root_executable() {
        let listing = "hamn-v0.0.1/\nhamn-v0.0.1/bin/\nhamn-v0.0.1/bin/hamn\nhamn-v0.0.1/scripts/x.sh\n";
        assert_eq!(executable_member(listing).unwrap(), "hamn-v0.0.1/bin/hamn");
        assert_eq!(executable_member("./bin/hamn").unwrap(), "./bin/hamn");
        for missing in
            ["", "bin/hamn\n", "a/b/bin/hamn\n", "a/bin/hamn2\n", "a/bin/hamn/\n", "a b/bin/hamn\n", "a/bin/hamn\r\n"]
        {
            assert_eq!(
                executable_member(missing).unwrap_err(),
                "candidate host archive has no executable",
                "{missing:?}"
            );
        }
        let duplicate = "a/bin/hamn\nb/bin/hamn\n";
        assert_eq!(executable_member(duplicate).unwrap_err(), "candidate host archive has duplicate executables");
    }
}
