//! Hosted (no-VM) validation evidence and keyless promotion checks.
//!
//! Hosted evidence binds a GitHub-hosted regression run to exact candidate
//! bytes and states what it did not exercise: it never claims a VM, Docker
//! or physical end-to-end run. Promotion accepts only evidence from the same
//! workflow run and attempt, for the same candidate bytes, with exactly
//! those capability claims.
use super::checkout::{Checkout, empty_output_directory, existing_directory, is_positive_decimal, required, variable};
use super::files::{canonical_json, read_json, set_mode, sha256_file, write_new};
use super::syntax::{candidate_tag_version, is_artifact_name, is_hex};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// Checks hosted evidence records as passed.
pub const HOSTED_PASSED: [&str; 4] = ["testLocalMacOS", "artifactHashes", "archiveSafety", "guestImageContract"];
/// Capabilities hosted evidence must record as not exercised.
pub const HOSTED_NOT_EXERCISED: [&str; 3] = ["vmLifecycle", "dockerE2E", "colimaCoexistence"];

/// `hosted-validation`: binds a hosted regression run to exact candidate
/// bytes. Inputs (environment): `RELEASE_REF` (must name the checked-out
/// commit), `RELEASE_TAG` (`vX.Y.Z-rc.N`), `CANDIDATE_DIR`, `OUTPUT_DIR`
/// (created; must be empty) and `GITHUB_RUN_ID`/`GITHUB_RUN_ATTEMPT`
/// (positive decimals, or both unset for `local`). Every artifact must
/// match `SHA256SUMS` before OUTPUT_DIR receives
/// `hosted-validation-evidence.json` (mode 0644), its only effect.
pub fn hosted_validation(args: &[String]) -> Result<(), String> {
    if !args.is_empty() {
        return Err("usage: hamn-dev release hosted-validation (inputs are environment variables)".into());
    }
    validate_hosted().map_err(|error| format!("hosted validation: {error}"))
}

fn validate_hosted() -> Result<(), String> {
    let [reference, tag, candidate_dir, output_dir] = required(
        ["RELEASE_REF", "RELEASE_TAG", "CANDIDATE_DIR", "OUTPUT_DIR"],
        "RELEASE_REF, RELEASE_TAG, CANDIDATE_DIR, and OUTPUT_DIR are required",
    )?;
    if candidate_tag_version(&tag).is_none() {
        return Err("RELEASE_TAG must be a release candidate tag".into());
    }
    let local = |name: &str| variable(name).map(|value| if value.is_empty() { "local".to_owned() } else { value });
    let (run, attempt) = (local("GITHUB_RUN_ID")?, local("GITHUB_RUN_ATTEMPT")?);
    let recorded = is_positive_decimal(&run) && is_positive_decimal(&attempt);
    if !recorded && !(run == "local" && attempt == "local") {
        return Err("workflow run and attempt must be positive decimals".into());
    }
    let candidate_dir = existing_directory(&candidate_dir, "CANDIDATE_DIR is unsafe")?;
    let checkout = Checkout::current()?;
    let commit = checkout.commit(&reference).map_err(|_| "RELEASE_REF is not a commit")?;
    if commit != checkout.head()? {
        return Err("RELEASE_REF does not match the checked-out commit".into());
    }
    let tree = checkout.tree(&commit).map_err(|_| "cannot resolve source tree")?;
    let output_dir = empty_output_directory(&output_dir)?;
    let regular =
        |name: &str| fs::symlink_metadata(candidate_dir.join(name)).is_ok_and(|info| info.file_type().is_file());
    if !regular("candidate.json") || !regular("SHA256SUMS") {
        return Err("candidate metadata is missing".into());
    }
    if !checksums_match(candidate_dir) {
        return Err("candidate artifact hashes do not match".into());
    }
    let evidence = evidence_for(candidate_dir, &tag, &commit, &tree, &run, &attempt)?;
    let path = output_dir.join("hosted-validation-evidence.json");
    write_new(&path, canonical_json(&evidence).as_bytes())?;
    set_mode(&path, 0o644)?;
    println!("bound hosted validation to exact candidate {tag}");
    Ok(())
}

/// `shasum -a 256 -c SHA256SUMS` in `directory`: at least one
/// `SHA256  NAME` (or binary-mode `SHA256 *NAME`) line, each naming a plain
/// file in `directory` with that digest. Anything else is a mismatch.
pub fn checksums_match(directory: &Path) -> bool {
    let Ok(listed) = fs::read_to_string(directory.join("SHA256SUMS")) else {
        return false;
    };
    let mut lines = listed.lines().filter(|line| !line.is_empty()).peekable();
    lines.peek().is_some()
        && lines.all(|line| {
            let (digest, rest) = line.split_at_checked(64).unwrap_or(("", ""));
            let name = rest.strip_prefix("  ").or_else(|| rest.strip_prefix(" *"));
            name.is_some_and(|name| {
                is_hex(digest, 64)
                    && is_artifact_name(name)
                    && sha256_file(&directory.join(name)).is_ok_and(|actual| actual == digest)
            })
        })
}

pub fn evidence_for(
    directory: &Path,
    tag: &str,
    commit: &str,
    tree: &str,
    run: &str,
    attempt: &str,
) -> Result<Value, String> {
    let candidate_path = directory.join("candidate.json");
    let checksums_path = directory.join("SHA256SUMS");
    let value = read_json(&candidate_path)?;
    let candidate = value.as_object().ok_or("candidate schema is invalid")?;
    let schema = BTreeSet::from(["artifacts", "commit", "kind", "schemaVersion", "sourceTree", "tag", "version"]);
    if candidate.keys().map(String::as_str).collect::<BTreeSet<_>>() != schema {
        return Err("candidate schema is invalid".into());
    }
    if candidate["schemaVersion"] != json!(1)
        || candidate["kind"] != "hamn-release-candidate"
        || candidate["tag"] != tag
        || candidate["commit"] != commit
        || candidate["sourceTree"] != tree
    {
        return Err("candidate identity does not match hosted validation".into());
    }
    if candidate_tag_version(tag).is_none_or(|version| candidate["version"] != version) {
        return Err("candidate version does not match release tag".into());
    }
    let entries = candidate["artifacts"]
        .as_array()
        .filter(|entries| entries.len() == 4)
        .ok_or("candidate artifact list is invalid")?;
    let mut artifacts = Map::new();
    for entry in entries {
        let entry = entry.as_object().ok_or("candidate artifact entry is invalid")?;
        let (name, digest) = (entry.get("name").and_then(Value::as_str), entry.get("sha256").and_then(Value::as_str));
        let (Some(name), Some(digest)) = (name, digest) else {
            return Err("candidate artifact entry is invalid".into());
        };
        if entry.len() != 2 || !is_artifact_name(name) || !is_hex(digest, 64) || artifacts.contains_key(name) {
            return Err("candidate artifact entry is invalid".into());
        }
        artifacts.insert(name.to_owned(), json!(digest));
    }
    let listed =
        fs::read_to_string(&checksums_path).map_err(|error| format!("{}: {error}", checksums_path.display()))?;
    let mut names = BTreeSet::new();
    for line in listed.lines().filter(|line| !line.trim().is_empty()) {
        let (_, name) =
            line.trim_start().split_once(char::is_whitespace).ok_or("candidate checksum set is incomplete")?;
        names.insert(name.trim().to_owned());
    }
    let mut expected: BTreeSet<String> = artifacts.keys().cloned().collect();
    expected.insert("candidate.json".into());
    if names != expected {
        return Err("candidate checksum set is incomplete".into());
    }
    let mut checks: Map<String, Value> = HOSTED_PASSED.iter().map(|name| (name.to_string(), json!(true))).collect();
    checks.extend(HOSTED_NOT_EXERCISED.iter().map(|name| (name.to_string(), json!(false))));
    Ok(json!({
        "schemaVersion": 1,
        "kind": "hamn-hosted-validation-evidence",
        "validationMode": "github-hosted-no-vm",
        "physicalE2E": false,
        "tag": tag,
        "commit": commit,
        "sourceTree": tree,
        "workflow": {"run": run, "attempt": attempt},
        "candidate": {
            "candidateJsonSha256": sha256_file(&candidate_path)?,
            "checksumsSha256": sha256_file(&checksums_path)?,
            "artifacts": artifacts,
        },
        "checks": checks,
    }))
}

/// Names in a promotion: the stable tag, the candidate tag it came from, the
/// source it was built from and the workflow run that validated it.
pub struct Promotion<'a> {
    pub stable_tag: &'a str,
    pub candidate_tag: &'a str,
    pub commit: &'a str,
    pub tree: &'a str,
    pub run: &'a str,
    pub attempt: &'a str,
    /// Host archive, guest image, SBOM and installer file names.
    pub artifacts: [&'a str; 4],
}

/// Accepts hosted evidence at `evidence_path` only when `directory` holds
/// exactly the promotion's artifacts, candidate.json and SHA256SUMS, and
/// the candidate and the evidence both name the promotion's tags, source
/// and workflow run and bind those exact bytes with no more capability
/// claims than a hosted run makes.
pub fn verify_hosted(directory: &Path, evidence_path: &Path, promotion: &Promotion) -> Result<(), String> {
    let mut expected: BTreeSet<&str> = promotion.artifacts.into_iter().collect();
    expected.extend(["candidate.json", "SHA256SUMS"]);
    let mut actual = BTreeSet::new();
    for entry in fs::read_dir(directory).map_err(|error| format!("{}: {error}", directory.display()))? {
        let entry = entry.map_err(|error| error.to_string())?;
        if !entry.file_type().map_err(|error| error.to_string())?.is_file() {
            return Err("candidate artifact directory contains an unsafe entry".into());
        }
        actual.insert(entry.file_name().to_string_lossy().into_owned());
    }
    if actual.iter().map(String::as_str).collect::<BTreeSet<_>>() != expected {
        return Err("candidate artifact directory contains unexpected entries".into());
    }
    let candidate_path = directory.join("candidate.json");
    let candidate = read_json(&candidate_path)?;
    let evidence = read_json(evidence_path)?;
    if !candidate.is_object()
        || candidate["tag"] != promotion.candidate_tag
        || candidate["version"] != promotion.stable_tag
        || candidate["commit"] != promotion.commit
        || candidate["sourceTree"] != promotion.tree
    {
        return Err("candidate provenance mismatch".into());
    }
    let entries = candidate["artifacts"].as_array().ok_or("candidate artifacts are invalid")?;
    // Entries that are not objects name nothing; a repeated name keeps its
    // last digest, and the evidence must bind that same map.
    let mut artifact_map = Map::new();
    for entry in entries.iter().filter_map(Value::as_object) {
        let Some(name) = entry.get("name").and_then(Value::as_str) else {
            return Err("candidate artifact names are invalid".into());
        };
        artifact_map.insert(name.to_owned(), entry.get("sha256").cloned().unwrap_or(Value::Null));
    }
    if artifact_map.keys().map(String::as_str).collect::<BTreeSet<_>>() != promotion.artifacts.into_iter().collect() {
        return Err("candidate artifact names are invalid".into());
    }
    let evidence_object = evidence.as_object().ok_or("hosted validation evidence schema is invalid")?;
    let schema = BTreeSet::from([
        "candidate",
        "checks",
        "commit",
        "kind",
        "physicalE2E",
        "schemaVersion",
        "sourceTree",
        "tag",
        "validationMode",
        "workflow",
    ]);
    if evidence_object.keys().map(String::as_str).collect::<BTreeSet<_>>() != schema {
        return Err("hosted validation evidence schema is invalid".into());
    }
    if evidence["schemaVersion"] != json!(1)
        || evidence["kind"] != "hamn-hosted-validation-evidence"
        || evidence["validationMode"] != "github-hosted-no-vm"
        || evidence["physicalE2E"] != json!(false)
        || evidence["tag"] != promotion.candidate_tag
        || evidence["commit"] != promotion.commit
        || evidence["sourceTree"] != promotion.tree
    {
        return Err("hosted validation identity mismatch".into());
    }
    if evidence["workflow"] != json!({"run": promotion.run, "attempt": promotion.attempt}) {
        return Err("hosted validation workflow provenance mismatch".into());
    }
    let bound = &evidence["candidate"];
    if !bound.is_object()
        || bound["candidateJsonSha256"] != sha256_file(&candidate_path)?
        || bound["checksumsSha256"] != sha256_file(&directory.join("SHA256SUMS"))?
        || bound["artifacts"] != Value::Object(artifact_map)
    {
        return Err("hosted validation candidate binding mismatch".into());
    }
    let checks = &evidence["checks"];
    if !checks.is_object()
        || HOSTED_PASSED.iter().any(|name| checks[name] != json!(true))
        || HOSTED_NOT_EXERCISED.iter().any(|name| checks[name] != json!(false))
    {
        return Err("hosted validation capabilities are invalid".into());
    }
    Ok(())
}

/// Writes the schema 3 stable update manifest to the new file `output` (an
/// existing path is never replaced), binding the host archive `host` and
/// guest image `guest` in `directory` by their `BASE_URL/NAME` URL, SHA-256
/// and size, and the guest image's format. (The schema 2 manifest for
/// v0.0.1-v0.1.2 clients is no longer published; those clients reinstall
/// with install.sh.)
pub fn write_manifest(
    output: &Path,
    stable_tag: &str,
    commit: &str,
    base_url: &str,
    directory: &Path,
    host: &str,
    guest: &str,
) -> Result<(), String> {
    let artifact = |name: &str| -> Result<Value, String> {
        let path = directory.join(name);
        let size = fs::metadata(&path).map_err(|error| format!("{}: {error}", path.display()))?.len();
        Ok(json!({"url": format!("{base_url}/{name}"), "sha256": sha256_file(&path)?, "size": size}))
    };
    let mut guest_image = artifact(guest)?;
    guest_image["format"] = json!("qcow2");
    guest_image["compression"] = json!("zlib");
    guest_image["virtualSize"] = json!(8u64 * 1024 * 1024 * 1024);
    let value = json!({
        "schemaVersion": 3,
        "channel": "stable",
        "version": stable_tag,
        "commit": commit,
        "validationMode": "github-hosted-no-vm",
        "compatibility": {"os": "darwin", "architecture": "arm64", "minimumMacOS": "13.0"},
        "artifacts": {"host": artifact(host)?, "guestImage": guest_image},
    });
    write_new(output, canonical_json(&value).as_bytes())
}

/// `verify-draft-release RELEASE_JSON TAG COMMIT`: the draft created from
/// `gh release view --json tagName,targetCommitish,isDraft,isPrerelease,assets`
/// must be an unpublished, non-prerelease draft at the release commit whose
/// assets are exactly the keyless release files.
pub fn verify_draft_release(args: &[String]) -> Result<(), String> {
    let [path, tag, commit] = args else {
        return Err("usage: hamn-dev release verify-draft-release RELEASE_JSON TAG COMMIT".into());
    };
    let release = read_json(Path::new(path))?;
    let expected: BTreeSet<String> = [
        format!("hamn-{tag}-darwin-arm64.tar.gz"),
        format!("hamn-{tag}-ubuntu-24.04-arm64.img"),
        format!("hamn-{tag}.spdx.json"),
    ]
    .into_iter()
    .chain(
        [
            "install.sh",
            "hamn-update-manifest-v3.json",
            "hosted-validation-evidence.json",
            "candidate.json",
            "SHA256SUMS",
            "promoted-from-rc",
        ]
        .map(str::to_owned),
    )
    .collect();
    let assets = match release.get("assets") {
        None => Some(Vec::new()),
        Some(assets) => assets.as_array().map(|assets| assets.iter().collect()),
    };
    let names: Option<BTreeSet<String>> = assets.and_then(|assets| {
        assets.iter().map(|asset| asset.get("name").and_then(Value::as_str).map(str::to_owned)).collect()
    });
    if release.get("tagName") != Some(&json!(tag))
        || release.get("targetCommitish") != Some(&json!(commit))
        || release.get("isDraft") != Some(&json!(true))
        || release.get("isPrerelease") != Some(&json!(false))
        || names != Some(expected)
    {
        return Err("draft release does not bind every keyless artifact".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::tmp::TempDir;

    const COMMIT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const TREE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const NAMES: [&str; 4] = [
        "hamn-v0.0.1-darwin-arm64.tar.gz",
        "hamn-v0.0.1-ubuntu-24.04-arm64.img",
        "hamn-v0.0.1.spdx.json",
        "install.sh",
    ];

    /// A candidate directory for v0.0.1-rc.7 with bound checksums.
    fn candidate(root: &Path) {
        for name in NAMES {
            fs::write(root.join(name), name).unwrap();
        }
        let artifacts: Vec<Value> =
            NAMES.iter().map(|name| json!({"name": name, "sha256": sha256_file(&root.join(name)).unwrap()})).collect();
        let candidate = json!({"schemaVersion": 1, "kind": "hamn-release-candidate", "tag": "v0.0.1-rc.7",
            "version": "v0.0.1", "commit": COMMIT, "sourceTree": TREE, "artifacts": artifacts});
        fs::write(root.join("candidate.json"), canonical_json(&candidate)).unwrap();
        let sums: String = NAMES
            .iter()
            .chain(&["candidate.json"])
            .map(|name| format!("{}  {name}\n", sha256_file(&root.join(name)).unwrap()))
            .collect();
        fs::write(root.join("SHA256SUMS"), sums).unwrap();
    }

    fn promotion<'a>() -> Promotion<'a> {
        Promotion {
            stable_tag: "v0.0.1",
            candidate_tag: "v0.0.1-rc.7",
            commit: COMMIT,
            tree: TREE,
            run: "41",
            attempt: "2",
            artifacts: NAMES,
        }
    }

    /// `value` with each top-level field of `change` replaced, or merged
    /// into when both are objects.
    fn changed(value: &Value, change: &Value) -> Value {
        let mut value = value.clone();
        for (key, replacement) in change.as_object().unwrap() {
            match (value.get_mut(key), replacement) {
                (Some(Value::Object(section)), Value::Object(fields)) => section.extend(fields.clone()),
                _ => value[key] = replacement.clone(),
            }
        }
        value
    }

    #[test]
    fn hosted_evidence_claims_only_what_ran_and_promotion_binds_it() {
        let directory = TempDir::new("hamn-release-hosted-");
        let root = directory.path().join("candidate");
        fs::create_dir(&root).unwrap();
        candidate(&root);
        let evidence = evidence_for(&root, "v0.0.1-rc.7", COMMIT, TREE, "41", "2").unwrap();
        assert_eq!(evidence["physicalE2E"], json!(false));
        assert!(evidence["checks"].get("k3sE2E").is_none());
        let path = directory.path().join("evidence.json");
        let write_evidence = |value: &Value| fs::write(&path, canonical_json(value)).unwrap();
        write_evidence(&evidence);
        verify_hosted(&root, &path, &promotion()).unwrap();
        for (change, message) in [
            (json!({"physicalE2E": true}), "identity mismatch"),
            (json!({"tag": "v0.0.1-rc.8"}), "identity mismatch"),
            (json!({"workflow": {"run": "41", "attempt": "3"}}), "workflow provenance mismatch"),
            (json!({"checks": {"vmLifecycle": true}}), "capabilities are invalid"),
            (json!({"checks": {"testLocalMacOS": false}}), "capabilities are invalid"),
            (json!({"candidate": {"checksumsSha256": "0".repeat(64)}}), "binding mismatch"),
            (json!({"legacy": {}}), "schema is invalid"),
        ] {
            write_evidence(&changed(&evidence, &change));
            let error = verify_hosted(&root, &path, &promotion()).unwrap_err();
            assert!(error.contains(message), "{change}: {error}");
        }
        write_evidence(&evidence);
        // Nothing unbound or linked may sit beside the candidate files.
        fs::write(root.join("extra"), "unbound").unwrap();
        assert!(verify_hosted(&root, &path, &promotion()).unwrap_err().contains("unexpected entries"));
        fs::remove_file(root.join("extra")).unwrap();
        fs::rename(root.join("install.sh"), directory.path().join("install.sh")).unwrap();
        std::os::unix::fs::symlink(directory.path().join("install.sh"), root.join("install.sh")).unwrap();
        assert!(verify_hosted(&root, &path, &promotion()).unwrap_err().contains("unsafe entry"));
    }

    #[test]
    fn hosted_evidence_requires_the_candidate_identity_and_checksum_set() {
        let directory = TempDir::new("hamn-release-hosted-");
        let root = directory.path();
        candidate(root);
        assert!(evidence_for(root, "v0.0.1-rc.8", COMMIT, TREE, "local", "local").unwrap_err().contains("identity"));
        assert!(evidence_for(root, "v0.0.1-rc.7", TREE, TREE, "local", "local").unwrap_err().contains("identity"));
        let sums = fs::read_to_string(root.join("SHA256SUMS")).unwrap();
        let fewer: String = sums.lines().skip(1).map(|line| format!("{line}\n")).collect();
        fs::write(root.join("SHA256SUMS"), fewer).unwrap();
        assert!(evidence_for(root, "v0.0.1-rc.7", COMMIT, TREE, "local", "local").unwrap_err().contains("incomplete"));
    }

    #[test]
    fn checksum_verification_follows_shasum_check_for_plain_names() {
        let directory = TempDir::new("hamn-release-sums-");
        let root = directory.path();
        fs::write(root.join("a"), "first").unwrap();
        fs::write(root.join("b"), "second").unwrap();
        let (a, b) = (sha256_file(&root.join("a")).unwrap(), sha256_file(&root.join("b")).unwrap());
        let sums = |text: String| {
            fs::write(root.join("SHA256SUMS"), text).unwrap();
            checksums_match(root)
        };
        assert!(sums(format!("{a}  a\n{b}  b\n")));
        assert!(sums(format!("{a} *a\n{b}  b")), "binary-mode markers and a missing final newline are accepted");
        assert!(!sums(String::new()), "an empty list verifies nothing");
        assert!(!sums(format!("{b}  a\n")), "wrong digest");
        assert!(!sums(format!("{a}  a\n{b}  missing\n")), "missing file");
        assert!(!sums(format!("{a}  a\n{b} b\n")), "malformed separator");
        assert!(!sums(format!("{}  a\n", a.to_uppercase())), "digests are lowercase hex");
        assert!(!sums(format!("{a}  a\n{b}  ../b\n")), "only plain names in the candidate directory");
        fs::remove_file(root.join("SHA256SUMS")).unwrap();
        assert!(!checksums_match(root));
    }

    #[test]
    fn draft_release_must_bind_exactly_the_keyless_assets() {
        let directory = TempDir::new("hamn-release-draft-");
        let path = directory.path().join("release.json");
        let assets = [
            "hamn-v0.0.1-darwin-arm64.tar.gz",
            "hamn-v0.0.1-ubuntu-24.04-arm64.img",
            "hamn-v0.0.1.spdx.json",
            "install.sh",
            "hamn-update-manifest-v3.json",
            "hosted-validation-evidence.json",
            "candidate.json",
            "SHA256SUMS",
            "promoted-from-rc",
        ];
        let release = |names: &[&str], draft: bool| {
            json!({"tagName": "v0.0.1", "targetCommitish": COMMIT, "isDraft": draft, "isPrerelease": false,
                "assets": names.iter().map(|name| json!({"name": name})).collect::<Vec<_>>()})
        };
        let verify = |value: &Value| {
            fs::write(&path, value.to_string()).unwrap();
            verify_draft_release(&[path.to_string_lossy().into_owned(), "v0.0.1".into(), COMMIT.into()])
        };
        verify(&release(&assets, true)).unwrap();
        assert!(verify(&release(&assets, false)).is_err());
        assert!(verify(&release(&assets[1..], true)).is_err());
        let mut with_v2 = assets.to_vec();
        with_v2.push("hamn-update-manifest.json");
        assert!(verify(&release(&with_v2, true)).is_err(), "the removed v2 manifest must not be published");
        assert!(verify(&changed(&release(&assets, true), &json!({"targetCommitish": TREE}))).is_err());
        assert!(verify(&changed(&release(&assets, true), &json!({"isPrerelease": true}))).is_err());
    }

    #[test]
    fn manifest_binds_urls_digests_and_sizes_and_is_never_replaced() {
        let directory = TempDir::new("hamn-release-manifest-");
        let root = directory.path();
        fs::write(root.join("host.tar.gz"), "host bytes").unwrap();
        fs::write(root.join("guest.img"), "guest").unwrap();
        let output = root.join("manifest.json");
        let base = "https://example.invalid/download/v0.0.1";
        write_manifest(&output, "v0.0.1", COMMIT, base, root, "host.tar.gz", "guest.img").unwrap();
        // `shasum -a 256` of the literal bytes, independent of sha256_file.
        let host_sha256 = "43bcde663fb99877d335d8d1c06c25386bb8528a324218f1a9e8424f50d3cb03";
        let guest_sha256 = "84983c60f7daadc1cb8698621f802c0d9f9a3c3c295c810748fb048115c186ec";
        let expected = json!({
            "schemaVersion": 3, "channel": "stable", "version": "v0.0.1", "commit": COMMIT,
            "validationMode": "github-hosted-no-vm",
            "compatibility": {"os": "darwin", "architecture": "arm64", "minimumMacOS": "13.0"},
            "artifacts": {
                "host": {"url": format!("{base}/host.tar.gz"), "sha256": host_sha256, "size": 10},
                "guestImage": {"url": format!("{base}/guest.img"), "sha256": guest_sha256, "size": 5,
                    "format": "qcow2", "compression": "zlib", "virtualSize": 8_589_934_592u64},
            },
        });
        assert_eq!(fs::read_to_string(&output).unwrap(), canonical_json(&expected));
        let error = write_manifest(&output, "v0.0.2", COMMIT, base, root, "host.tar.gz", "guest.img").unwrap_err();
        assert!(error.contains("exists"), "{error}");
        assert_eq!(fs::read_to_string(&output).unwrap(), canonical_json(&expected));
        let missing = root.join("other.json");
        assert!(write_manifest(&missing, "v0.0.1", COMMIT, base, root, "absent", "guest.img").is_err());
        assert!(!missing.exists(), "a manifest was written for a missing artifact");
    }
}
