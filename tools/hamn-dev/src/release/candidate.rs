//! `build-candidate`: builds the exact bytes that hosted and physical
//! validation will test, from the clean checked-out commit.
//!
//! Inputs (environment): `RELEASE_REF` (must name the checked-out commit),
//! `RELEASE_TAG` (`vX.Y.Z-rc.N`), `OUTPUT_DIR` (created; must be empty),
//! `HAMN_GUEST_IMAGE` (an owned, single-link regular file), the release
//! repository `GITHUB_REPOSITORY` or else `HAMN_RELEASE_REPOSITORY`
//! (`owner/repository`), and `HAMN_RELEASE_MANIFEST_URL` (required without a
//! repository). `HAMN_RELEASE_ALLOW_DIRTY=1` accepts a dirty tree and
//! `HAMN_RELEASE_ALLOW_LOCAL=1` local `file://` URLs; both are for tests.
//!
//! Effects: every input is checked before anything is built or written.
//! `make host VERSION=X.Y.Z` then replaces the checkout's build/hamn, and
//! OUTPUT_DIR receives exactly the host archive, the guest image,
//! `install.sh`, the SPDX SBOM, `candidate.json` and `SHA256SUMS`. The
//! archive is staged in a private workspace inside OUTPUT_DIR that is
//! removed on success and failure; a failure can leave some of the six
//! files behind, which a later validation rejects as an incomplete set.
//! Each metadata file is canonical JSON (or shell) derived only from its
//! inputs, so equal inputs give equal bytes.
use super::checkout::{Checkout, empty_output_directory, environment, git_in, machine, required, variable};
use super::files::{Workspace, canonical_json, copy_new, owned_regular, set_mode, sha256_file, write_new};
use super::process::{self, Spec};
use super::syntax::{candidate_tag_version, is_digits, is_repository, shell_quote};
use serde_json::{Value, json};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::time::Duration;

/// A cold release build of the host executable fits well within this.
const BUILD_TIMEOUT: Duration = Duration::from_secs(3600);
const TAR_TIMEOUT: Duration = Duration::from_secs(600);
const GIT_TIMEOUT: Duration = Duration::from_secs(120);

/// `build-candidate`; see the module documentation.
pub fn build_candidate(args: &[String]) -> Result<(), String> {
    if !args.is_empty() {
        return Err("usage: hamn-dev release build-candidate (inputs are environment variables)".into());
    }
    build().map_err(|error| format!("release candidate: {error}"))
}

/// The update manifest URL a candidate embeds. With a release repository
/// it is that repository's latest-release v3 manifest, which a configured
/// URL must equal; otherwise the configured URL, which must use HTTPS or,
/// only with `allow_local`, be a `file://` URL or an absolute path.
pub fn manifest_url(repository: &str, configured: &str, allow_local: bool) -> Result<String, String> {
    let url = if repository.is_empty() {
        configured.to_owned()
    } else {
        if !is_repository(repository) {
            return Err("GITHUB_REPOSITORY is invalid".into());
        }
        let canonical =
            format!("https://github.com/{repository}/releases/latest/download/hamn-update-manifest-v3.json");
        if !configured.is_empty() && configured != canonical {
            return Err("HAMN_RELEASE_MANIFEST_URL must match the canonical GitHub Release manifest URL".into());
        }
        canonical
    };
    if url.is_empty() {
        return Err("HAMN_RELEASE_MANIFEST_URL is required outside GitHub Actions".into());
    }
    let local = url.starts_with("file://") || url.starts_with('/');
    if url.starts_with("https://") || (local && allow_local) {
        Ok(url)
    } else {
        Err("release manifest URL must use HTTPS".into())
    }
}

fn build() -> Result<(), String> {
    let [reference, tag, output_dir] = required(
        ["RELEASE_REF", "RELEASE_TAG", "OUTPUT_DIR"],
        "RELEASE_REF, RELEASE_TAG, and OUTPUT_DIR are required",
    )?;
    let version = candidate_tag_version(&tag).ok_or("RELEASE_TAG must be a vX.Y.Z-rc.N tag")?.to_owned();
    if machine()? != "arm64" {
        return Err("release candidate must build on Apple Silicon arm64".into());
    }
    let checkout = Checkout::current()?;
    let commit = checkout.commit(&reference).map_err(|_| "RELEASE_REF is not a commit")?;
    if commit != checkout.head()? {
        return Err("RELEASE_REF does not match the checked-out commit".into());
    }
    let tree = checkout.tree(&commit).map_err(|_| "cannot resolve the checked-out source tree")?;
    let epoch = checkout
        .git_text(&["show", "-s", "--format=%ct", &commit])
        .map_err(|_| "cannot resolve the checked-out commit timestamp")?;
    let epoch: u64 = Some(epoch.as_str())
        .filter(|epoch| is_digits(epoch))
        .and_then(|epoch| epoch.parse().ok())
        .ok_or("checked-out commit timestamp is invalid")?;
    if variable("HAMN_RELEASE_ALLOW_DIRTY")? != "1" && checkout.is_dirty()? {
        return Err("release source tree is dirty".into());
    }
    let guest_image = variable("HAMN_GUEST_IMAGE")?;
    owned_regular(Path::new(&guest_image)).map_err(|_| "HAMN_GUEST_IMAGE must name one owned regular guest image")?;
    let allow_local = variable("HAMN_RELEASE_ALLOW_LOCAL")? == "1";
    let repository = match variable("GITHUB_REPOSITORY")? {
        github if github.is_empty() => variable("HAMN_RELEASE_REPOSITORY")?,
        github => github,
    };
    let manifest_url = manifest_url(&repository, &variable("HAMN_RELEASE_MANIFEST_URL")?, allow_local)?;
    // Artifact URLs are the repository's release downloads, or local files.
    if repository.is_empty() && !allow_local {
        return Err("HAMN_RELEASE_REPOSITORY is required".into());
    }
    let output_dir_text = output_dir;
    let output_dir = empty_output_directory(&output_dir_text)?;
    let work = Workspace::create(output_dir, ".hamn-candidate.")
        .map_err(|error| format!("cannot create candidate workspace: {error}"))?;

    let plain_version = &version[1..];
    let binary = build_host(&checkout.root, plain_version)?;
    let host_name = format!("hamn-{version}-darwin-arm64");
    let artifact_root = work.path().join(&host_name);
    make_directory(&artifact_root)?;
    make_directory(&artifact_root.join("bin"))?;
    copy_new(&binary, &artifact_root.join("bin/hamn"), 0o755)?;
    stage_sources(&checkout.root, work.path(), &artifact_root)?;
    let url_file = artifact_root.join("packaging/release/update-manifest-url");
    write_new(&url_file, format!("{manifest_url}\n").as_bytes())?;
    set_mode(&url_file, 0o644)?;

    let host_file = format!("{host_name}.tar.gz");
    let host_artifact = output_dir.join(&host_file);
    // Without COPYFILE_DISABLE, macOS tar adds AppleDouble `._` members.
    let archive_environment = environment(&[("COPYFILE_DISABLE", "1")])?;
    let archive_args: [&OsStr; 5] =
        ["-C".as_ref(), work.path().as_os_str(), "-czf".as_ref(), host_artifact.as_os_str(), host_name.as_ref()];
    let spec = Spec { environment: Some(&archive_environment), ..Spec::default() };
    process::run(OsStr::new("tar"), &archive_args, &spec, TAR_TIMEOUT)?;
    let guest_file = format!("hamn-{version}-ubuntu-24.04-arm64.img");
    let guest_artifact = output_dir.join(&guest_file);
    copy_new(Path::new(&guest_image), &guest_artifact, 0o644)?;

    let host_sha256 = sha256_file(&host_artifact)?;
    let guest_sha256 = sha256_file(&guest_artifact)?;
    let (host_url, guest_url) = if repository.is_empty() {
        (format!("file://{}", host_artifact.display()), format!("file://{}", guest_artifact.display()))
    } else {
        let base = format!("https://github.com/{repository}/releases/download/{version}");
        (format!("{base}/{host_file}"), format!("{base}/{guest_file}"))
    };
    let template_path = checkout.root.join("packaging/release/install.sh.in");
    let template =
        fs::read_to_string(&template_path).map_err(|error| format!("{}: {error}", template_path.display()))?;
    let size = |path: &Path| {
        fs::metadata(path).map(|info| info.len().to_string()).map_err(|error| format!("{}: {error}", path.display()))
    };
    let installer_text = render(
        &template,
        &[
            ("__HAMN_VERSION__", version.clone()),
            ("__HAMN_COMMIT__", commit.clone()),
            ("__HAMN_HOST_URL__", host_url),
            ("__HAMN_HOST_SHA256__", host_sha256.clone()),
            ("__HAMN_GUEST_URL__", guest_url),
            ("__HAMN_GUEST_SHA256__", guest_sha256.clone()),
            ("__HAMN_HOST_SIZE__", size(&host_artifact)?),
            ("__HAMN_GUEST_SIZE__", size(&guest_artifact)?),
        ],
    )?;
    let installer = output_dir.join("install.sh");
    write_new(&installer, installer_text.as_bytes())?;
    set_mode(&installer, 0o755)?;
    let sbom_file = format!("hamn-{version}.spdx.json");
    let sbom_path = output_dir.join(&sbom_file);
    let document = sbom(&version, &commit, &tree, epoch, (&host_file, &host_sha256), (&guest_file, &guest_sha256))?;
    write_new(&sbom_path, canonical_json(&document).as_bytes())?;
    set_mode(&sbom_path, 0o644)?;

    let artifacts = [
        (host_file, host_sha256),
        (guest_file, guest_sha256),
        ("install.sh".to_owned(), sha256_file(&installer)?),
        (sbom_file, sha256_file(&sbom_path)?),
    ];
    let candidate_path = output_dir.join("candidate.json");
    write_new(&candidate_path, canonical_json(&candidate(&tag, &version, &commit, &tree, &artifacts)).as_bytes())?;
    set_mode(&candidate_path, 0o644)?;
    let mut checksums: String = artifacts.iter().map(|(name, digest)| format!("{digest}  {name}\n")).collect();
    checksums.push_str(&format!("{}  candidate.json\n", sha256_file(&candidate_path)?));
    let checksums_path = output_dir.join("SHA256SUMS");
    write_new(&checksums_path, checksums.as_bytes())?;
    set_mode(&checksums_path, 0o644)?;
    work.remove()?;
    println!("built candidate {tag} ({commit}) in {output_dir_text}");
    Ok(())
}

/// `make host VERSION=VERSION` in the checkout, then the version the built
/// executable reports; returns build/hamn.
fn build_host(root: &Path, version: &str) -> Result<std::path::PathBuf, String> {
    let environment = environment(&[])?;
    let spec = Spec { environment: Some(&environment), ..Spec::default() };
    let assignment = format!("VERSION={version}");
    let args: [&OsStr; 4] = ["-C".as_ref(), root.as_os_str(), "host".as_ref(), assignment.as_ref()];
    // The build's progress and warnings stay visible; its stdout is not.
    process::run_passing_stderr(OsStr::new("make"), &args, &spec, BUILD_TIMEOUT)?;
    let binary = root.join("build/hamn");
    let mismatch = |detail: String| format!("candidate binary version does not match tag: {detail}");
    let reported =
        process::run(binary.as_os_str(), &["--version"], &spec, Duration::from_secs(120)).map_err(mismatch)?;
    if !reported.lines().any(|line| line == format!("hamn {version}")) {
        return Err(mismatch(reported.trim_end().to_owned()));
    }
    Ok(binary)
}

/// `mkdir -m 0755`: a new directory with exactly that mode.
fn make_directory(path: &Path) -> Result<(), String> {
    fs::DirBuilder::new().mode(0o755).create(path).map_err(|error| format!("{}: {error}", path.display()))?;
    set_mode(path, 0o755)
}

/// Copies the checkout's tracked scripts/ and packaging/ files (committed
/// or not; untracked files never) into `destination` with tar, so modes
/// and links are kept.
fn stage_sources(root: &Path, work: &Path, destination: &Path) -> Result<(), String> {
    let listed = git_in(root, &["ls-files", "-z", "--", "scripts", "packaging"], &[], GIT_TIMEOUT)?;
    if !listed.status.success() {
        return Err(format!("git ls-files failed: {}", listed.stderr_lossy().trim()));
    }
    let environment = environment(&[])?;
    // Stage through a file: a tar reading a pipe stops at the end-of-archive
    // marker, so a writer still sending the final record's padding can fail
    // with EPIPE ("tar: Write error", CI run 36145944094).
    let staged = work.join("sources.tar");
    let create: [&OsStr; 7] = [
        "-C".as_ref(),
        root.as_os_str(),
        "--null".as_ref(),
        "-T".as_ref(),
        "-".as_ref(),
        "-cf".as_ref(),
        staged.as_os_str(),
    ];
    let spec = Spec { environment: Some(&environment), input: Some(&listed.stdout) };
    process::run(OsStr::new("tar"), &create, &spec, TAR_TIMEOUT)?;
    let extract: [OsString; 4] = ["-C".into(), destination.into(), "-xf".into(), staged.clone().into()];
    let spec = Spec { environment: Some(&environment), ..Spec::default() };
    process::run(OsStr::new("tar"), &extract, &spec, TAR_TIMEOUT)?;
    fs::remove_file(&staged).map_err(|error| format!("{}: {error}", staged.display()))
}

/// Replaces each `__HAMN_*__` placeholder, which must occur exactly once,
/// with its shell-quoted value; no placeholder may remain.
fn render(template: &str, values: &[(&str, String)]) -> Result<String, String> {
    let mut rendered = template.to_owned();
    for (placeholder, value) in values {
        if rendered.matches(placeholder).count() != 1 {
            return Err(format!("installer template placeholder is malformed: {placeholder}"));
        }
        rendered = rendered.replace(placeholder, &shell_quote(value));
    }
    if rendered.contains("__HAMN_") {
        return Err("installer template has an unresolved placeholder".into());
    }
    Ok(rendered)
}

/// `YYYY-MM-DDTHH:MM:SSZ` for seconds since the Unix epoch (years up to 9999).
pub fn utc_timestamp(epoch: u64) -> Result<String, String> {
    if epoch > 253_402_300_799 {
        return Err(format!("timestamp {epoch} is after 9999-12-31"));
    }
    let (days, seconds) = ((epoch / 86_400) as i64, epoch % 86_400);
    // Howard Hinnant's days-to-civil conversion for the proleptic Gregorian calendar.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    Ok(format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", seconds / 3600, seconds / 60 % 60, seconds % 60))
}

/// The SPDX 2.3 SBOM of the host archive and guest image (`(name,
/// sha256)` each), created at the commit's timestamp.
fn sbom(
    version: &str,
    commit: &str,
    tree: &str,
    epoch: u64,
    host: (&str, &str),
    guest: (&str, &str),
) -> Result<Value, String> {
    let package = |id: &str, (name, digest): (&str, &str)| {
        json!({
            "SPDXID": id,
            "name": name,
            "versionInfo": version,
            "downloadLocation": "NOASSERTION",
            "filesAnalyzed": false,
            "checksums": [{"algorithm": "SHA256", "checksumValue": digest}],
        })
    };
    Ok(json!({
        "SPDXID": "SPDXRef-DOCUMENT",
        "spdxVersion": "SPDX-2.3",
        "name": format!("Hamn {version}"),
        "dataLicense": "CC0-1.0",
        "documentNamespace": format!("https://hamn.dev/spdx/{version}/{commit}"),
        "creationInfo": {
            "creators": ["Tool: hamn-release-candidate"],
            "created": utc_timestamp(epoch)?,
            "licenseListVersion": "3.23",
        },
        "packages": [package("SPDXRef-HamnHost", host), package("SPDXRef-HamnGuest", guest)],
        "annotations": [{
            "annotationType": "OTHER",
            "annotator": "Tool: hamn-release-candidate",
            "comment": format!("commit={commit} sourceTree={tree}"),
        }],
    }))
}

/// `candidate.json`: the tag, version and source identity and the four
/// artifacts (host, guest, installer, SBOM) as `(name, sha256)`.
fn candidate(tag: &str, version: &str, commit: &str, tree: &str, artifacts: &[(String, String); 4]) -> Value {
    let artifacts: Vec<Value> =
        artifacts.iter().map(|(name, digest)| json!({"name": name, "sha256": digest})).collect();
    json!({
        "schemaVersion": 1,
        "kind": "hamn-release-candidate",
        "tag": tag,
        "version": version,
        "commit": commit,
        "sourceTree": tree,
        "artifacts": artifacts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_utc_iso_8601() {
        assert_eq!(utc_timestamp(0).unwrap(), "1970-01-01T00:00:00Z");
        assert_eq!(utc_timestamp(951_782_400).unwrap(), "2000-02-29T00:00:00Z");
        assert_eq!(utc_timestamp(1_700_000_000).unwrap(), "2023-11-14T22:13:20Z");
        assert_eq!(utc_timestamp(1_709_251_199).unwrap(), "2024-02-29T23:59:59Z");
        assert_eq!(utc_timestamp(253_402_300_799).unwrap(), "9999-12-31T23:59:59Z");
        assert!(utc_timestamp(253_402_300_800).is_err());
    }

    #[test]
    fn installer_placeholders_are_quoted_once_and_all_resolved() {
        let values = [("__HAMN_VERSION__", "v0.0.1".to_owned()), ("__HAMN_HOST_URL__", "file:///tmp/a b".to_owned())];
        assert_eq!(
            render("V=__HAMN_VERSION__\nU=__HAMN_HOST_URL__\n", &values).unwrap(),
            "V=v0.0.1\nU='file:///tmp/a b'\n"
        );
        let missing = render("V=__HAMN_VERSION__\n", &values).unwrap_err();
        assert!(missing.contains("malformed: __HAMN_HOST_URL__"), "{missing}");
        let twice = render("__HAMN_VERSION__ __HAMN_VERSION__ __HAMN_HOST_URL__", &values).unwrap_err();
        assert!(twice.contains("malformed: __HAMN_VERSION__"), "{twice}");
        let extra = render("__HAMN_VERSION__ __HAMN_HOST_URL__ __HAMN_OTHER__", &values).unwrap_err();
        assert!(extra.contains("unresolved placeholder"), "{extra}");
    }

    #[test]
    fn manifest_url_is_canonical_for_a_repository_and_https_unless_local() {
        let canonical = "https://github.com/example/hamn/releases/latest/download/hamn-update-manifest-v3.json";
        assert_eq!(manifest_url("example/hamn", "", false).unwrap(), canonical);
        assert_eq!(manifest_url("example/hamn", canonical, false).unwrap(), canonical);
        let other = manifest_url("example/hamn", "https://downloads.example.invalid/m.json", true).unwrap_err();
        assert_eq!(other, "HAMN_RELEASE_MANIFEST_URL must match the canonical GitHub Release manifest URL");
        for repository in ["example", "example/hamn/extra", "exa mple/hamn", "/hamn"] {
            assert_eq!(manifest_url(repository, "", true).unwrap_err(), "GITHUB_REPOSITORY is invalid", "{repository}");
        }
        assert_eq!(
            manifest_url("", "", true).unwrap_err(),
            "HAMN_RELEASE_MANIFEST_URL is required outside GitHub Actions"
        );
        assert_eq!(
            manifest_url("", "https://example.invalid/m.json", false).unwrap(),
            "https://example.invalid/m.json"
        );
        for local in ["file:///tmp/m.json", "/tmp/m.json"] {
            assert_eq!(manifest_url("", local, true).unwrap(), local);
            assert_eq!(manifest_url("", local, false).unwrap_err(), "release manifest URL must use HTTPS");
        }
        for insecure in ["http://example.invalid/m.json", "relative/m.json", "HTTPS://example.invalid/m.json"] {
            assert_eq!(manifest_url("", insecure, true).unwrap_err(), "release manifest URL must use HTTPS");
        }
    }

    #[test]
    fn metadata_documents_bind_the_source_and_artifacts() {
        let commit = "a".repeat(40);
        let tree = "b".repeat(40);
        let document = sbom("v0.0.1", &commit, &tree, 0, ("host.tar.gz", "1"), ("guest.img", "2")).unwrap();
        assert_eq!(document["creationInfo"]["created"], "1970-01-01T00:00:00Z");
        assert_eq!(document["packages"][1]["checksums"][0]["checksumValue"], "2");
        assert_eq!(document["annotations"][0]["comment"], format!("commit={commit} sourceTree={tree}"));
        assert!(sbom("v0.0.1", &commit, &tree, 253_402_300_800, ("h", "1"), ("g", "2")).is_err());
        let artifacts = ["h", "g", "install.sh", "s"].map(|name| (name.to_owned(), format!("{name}-digest")));
        let value = candidate("v0.0.1-rc.1", "v0.0.1", &commit, &tree, &artifacts);
        assert_eq!(value["artifacts"][2], json!({"name": "install.sh", "sha256": "install.sh-digest"}));
        assert_eq!((value["tag"].as_str(), value["version"].as_str()), (Some("v0.0.1-rc.1"), Some("v0.0.1")));
    }
}
