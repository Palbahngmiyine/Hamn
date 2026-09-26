//! The release candidate and physical validation evidence contracts: what a
//! candidate directory must contain before a physical gate runs, and what
//! physical evidence must prove before it counts for promotion.
//!
//! Physical evidence schema 3 dropped the legacy K3s retirement checks and
//! the `legacy` section; schema 2 evidence is rejected.
use super::files::{read_json_limited, sha256_file};
use super::syntax::{candidate_tag_version, is_artifact_name, is_hex};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

pub const PHYSICAL_SCHEMA_VERSION: u64 = 3;

/// Every check a physical run must pass.
pub const CHECKS: [&str; 11] = [
    "singleBinary",
    "vmLifecycle",
    "multipleProfiles",
    "dockerWithoutCli",
    "dockerApi",
    "externalDockerSocket",
    "externalKubernetes",
    "tuiTerminalRestore",
    "vmSurvivesTuiExit",
    "kubeconfigUnchanged",
    "cleanup",
];

fn keys(value: &Map<String, Value>) -> BTreeSet<&str> {
    value.keys().map(String::as_str).collect()
}

fn object<'a>(value: &'a Value, error: &str) -> Result<&'a Map<String, Value>, String> {
    value.as_object().ok_or_else(|| error.to_owned())
}

/// Validates a candidate directory against its tag and the checked-out
/// source: exact schema, source identity, the four artifacts plus
/// `candidate.json` and `SHA256SUMS` and nothing else, checksums bound to
/// the manifest and each artifact's bytes.
pub fn validate_candidate(directory: &Path, tag: &str, commit: &str, tree: &str) -> Result<Value, String> {
    let value = read_json_limited(&directory.join("candidate.json"))?;
    let candidate = object(&value, "candidate schema is invalid")?;
    let schema = ["artifacts", "commit", "kind", "schemaVersion", "sourceTree", "tag", "version"];
    if keys(candidate) != BTreeSet::from(schema)
        || candidate["schemaVersion"] != json!(1)
        || candidate["kind"] != "hamn-release-candidate"
    {
        return Err("candidate schema is invalid".into());
    }
    let Some(version) = candidate_tag_version(tag) else {
        return Err("candidate source identity mismatch".into());
    };
    if candidate["tag"] != tag
        || candidate["version"] != version
        || candidate["commit"] != commit
        || candidate["sourceTree"] != tree
    {
        return Err("candidate source identity mismatch".into());
    }
    let expected: BTreeSet<String> = ["-darwin-arm64.tar.gz", "-ubuntu-24.04-arm64.img", ".spdx.json"]
        .iter()
        .map(|suffix| format!("hamn-{version}{suffix}"))
        .chain(["install.sh".to_owned()])
        .collect();
    let artifacts = artifact_entries(&candidate["artifacts"]).ok_or("candidate artifact schema is invalid")?;
    let mut listed: BTreeSet<String> = expected.clone();
    listed.extend(["candidate.json".to_owned(), "SHA256SUMS".to_owned()]);
    let present = fs::read_dir(directory)
        .map_err(|error| format!("{}: {error}", directory.display()))?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<Result<BTreeSet<String>, _>>()
        .map_err(|error| error.to_string())?;
    if artifacts.keys().cloned().collect::<BTreeSet<_>>() != expected || present != listed {
        return Err("candidate artifact set is invalid".into());
    }
    let sums = directory.join("SHA256SUMS");
    let sums_link = fs::symlink_metadata(&sums).map_err(|error| error.to_string())?.file_type().is_symlink();
    if sums_link || fs::metadata(&sums).map_err(|error| error.to_string())?.len() > 4096 {
        return Err("unsafe checksums file".into());
    }
    let mut checksums = BTreeMap::new();
    for line in fs::read_to_string(&sums).map_err(|error| error.to_string())?.lines() {
        let (digest, name) = line.split_once("  ").ok_or("invalid candidate checksums")?;
        if !is_hex(digest, 64)
            || !is_artifact_name(name)
            || checksums.insert(name.to_owned(), digest.to_owned()).is_some()
        {
            return Err("invalid candidate checksums".into());
        }
    }
    let mut bound = artifacts.clone();
    bound.insert("candidate.json".into(), sha256_file(&directory.join("candidate.json"))?);
    if checksums != bound {
        return Err("candidate checksums binding mismatch".into());
    }
    for (name, digest) in &artifacts {
        let path = directory.join(name);
        let regular = fs::symlink_metadata(&path).is_ok_and(|info| info.file_type().is_file());
        if !regular || sha256_file(&path)? != *digest {
            return Err("candidate artifact digest mismatch".into());
        }
    }
    Ok(value)
}

/// The `name -> sha256` map of exactly four `{name, sha256}` string entries
/// with distinct names, or `None`.
fn artifact_entries(value: &Value) -> Option<BTreeMap<String, String>> {
    let entries = value.as_array().filter(|entries| entries.len() == 4)?;
    let mut artifacts = BTreeMap::new();
    for entry in entries {
        let entry = entry.as_object().filter(|entry| keys(entry) == BTreeSet::from(["name", "sha256"]))?;
        let (name, digest) = (entry["name"].as_str()?, entry["sha256"].as_str()?);
        if artifacts.insert(name.to_owned(), digest.to_owned()).is_some() {
            return None;
        }
    }
    Some(artifacts)
}

/// The candidate's artifacts as a JSON object `name -> sha256`, as evidence
/// binds them.
pub fn artifact_map(candidate: &Value) -> Result<Map<String, Value>, String> {
    let entries = candidate["artifacts"].as_array().ok_or("candidate artifacts are invalid")?;
    entries
        .iter()
        .map(|entry| match (entry["name"].as_str(), entry.get("sha256")) {
            (Some(name), Some(digest)) => Ok((name.to_owned(), digest.clone())),
            _ => Err("candidate artifacts are invalid".to_owned()),
        })
        .collect()
}

/// Physical evidence for `candidate` from a run in which every check passed.
pub fn physical_evidence(
    candidate: &Value,
    candidate_sha256: &str,
    checksums_sha256: &str,
    run: &str,
    attempt: &str,
    kubernetes: Value,
) -> Result<Value, String> {
    Ok(json!({
        "schemaVersion": PHYSICAL_SCHEMA_VERSION,
        "kind": "hamn-physical-validation-evidence",
        "validationMode": "physical-apple-silicon",
        "tag": candidate["tag"],
        "commit": candidate["commit"],
        "sourceTree": candidate["sourceTree"],
        "workflow": {"run": run, "attempt": attempt},
        "candidate": {
            "candidateJsonSha256": candidate_sha256,
            "checksumsSha256": checksums_sha256,
            "artifacts": artifact_map(candidate)?,
        },
        "checks": CHECKS.iter().map(|check| (check.to_string(), Value::Bool(true))).collect::<Map<_, _>>(),
        "kubernetes": kubernetes,
    }))
}

/// Validates physical evidence against the exact candidate files and the
/// workflow run that is promoting it.
pub fn validate_physical(
    candidate_path: &Path,
    checksums_path: &Path,
    evidence_path: &Path,
    run: &str,
    attempt: &str,
) -> Result<Value, String> {
    let candidate = read_json_limited(candidate_path)?;
    let value = read_json_limited(evidence_path)?;
    let evidence = object(&value, "physical validation evidence schema is invalid")?;
    let schema = [
        "candidate",
        "checks",
        "commit",
        "kind",
        "kubernetes",
        "schemaVersion",
        "sourceTree",
        "tag",
        "validationMode",
        "workflow",
    ];
    if keys(evidence) != BTreeSet::from(schema) {
        return Err("physical validation evidence schema is invalid".into());
    }
    if evidence["schemaVersion"] != json!(PHYSICAL_SCHEMA_VERSION)
        || evidence["kind"] != "hamn-physical-validation-evidence"
        || evidence["validationMode"] != "physical-apple-silicon"
    {
        return Err("physical validation identity is invalid".into());
    }
    for key in ["tag", "commit", "sourceTree"] {
        if candidate.get(key).is_none_or(|expected| evidence[key] != *expected) {
            return Err("physical validation provenance mismatch".into());
        }
    }
    if evidence["workflow"] != json!({"run": run, "attempt": attempt}) {
        return Err("physical validation workflow mismatch".into());
    }
    let bound = json!({
        "candidateJsonSha256": sha256_file(candidate_path)?,
        "checksumsSha256": sha256_file(checksums_path)?,
        "artifacts": artifact_map(&candidate)?,
    });
    if evidence["candidate"] != bound {
        return Err("physical validation candidate binding mismatch".into());
    }
    let checks = evidence["checks"].as_object();
    if checks.is_none_or(|checks| {
        keys(checks) != BTreeSet::from(CHECKS) || checks.values().any(|value| *value != json!(true))
    }) {
        return Err("physical validation checks are incomplete".into());
    }
    let kubernetes = &evidence["kubernetes"];
    if !kubernetes.is_object()
        || kubernetes.get("passed") != Some(&json!(true))
        || kubernetes.get("namespaceRemoved") != Some(&json!(true))
        || kubernetes.get("kubeconfigUnchanged") != Some(&json!(true))
        || kubernetes.get("kind") != Some(&json!("hamn-external-kubernetes-e2e"))
    {
        return Err("external Kubernetes evidence is incomplete".into());
    }
    Ok(value)
}
