use super::{Result, files, require};
use serde::{Deserialize, Deserializer};
use serde_json::json;
use std::{fs, path::Path};

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Manifest {
    schema_version: u32,
    channel: String,
    version: String,
    commit: String,
    validation_mode: String,
    compatibility: Compatibility,
    artifacts: Artifacts,
    #[serde(default, deserialize_with = "repository")]
    repository: Option<String>,
}
fn repository<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Option<String>, D::Error> {
    String::deserialize(d).map(Some)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Compatibility {
    os: String,
    architecture: String,
    #[serde(rename = "minimumMacOS")]
    minimum_mac_os: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Artifacts {
    host: Artifact,
    guest_image: Artifact,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    url: String,
    sha256: String,
}

pub(super) fn hexadecimal(value: &str, size: usize) -> bool {
    value.len() == size
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn version(value: &str) -> Result<[u64; 3]> {
    let parts: Vec<_> = value.split('.').collect();
    require((1..=3).contains(&parts.len()), "invalid macOS version")?;
    let mut result = [0; 3];
    for (index, part) in parts.iter().enumerate() {
        require(
            !part.is_empty() && part.bytes().all(|c| c.is_ascii_digit()),
            "invalid macOS version",
        )?;
        result[index] = part.parse()?;
    }
    Ok(result)
}
fn release(value: &str) -> bool {
    value
        .strip_prefix('v')
        .is_some_and(|v| v.split('.').count() == 3 && version(v).is_ok())
}
fn artifact(value: &Artifact) -> Result<()> {
    require(
        !value.url.is_empty() && value.url.bytes().all(|c| (33..=126).contains(&c)),
        "invalid artifact URL",
    )?;
    require(hexadecimal(&value.sha256, 64), "invalid artifact SHA-256")
}

pub(super) fn fields(path: &str, os: &str, architecture: &str) -> Result<()> {
    // Direct typed deserialization rejects duplicate fields at every level,
    // unknown keys, non-string identities, NaN, and mismatched containers.
    let m: Manifest = serde_json::from_slice(&fs::read(path)?)?;
    require(
        m.schema_version == 2 && m.channel == "stable",
        "manifest is not a stable schema v2 release",
    )?;
    require(
        release(&m.version) && hexadecimal(&m.commit, 40),
        "invalid release identity",
    )?;
    require(
        matches!(
            m.validation_mode.as_str(),
            "github-hosted-no-vm" | "physical-apple-silicon"
        ),
        "invalid release validation mode",
    )?;
    if let Some(repository) = m.repository {
        let parts: Vec<_> = repository.split('/').collect();
        require(
            parts.len() == 2
                && parts.iter().all(|p| {
                    !p.is_empty()
                        && p.bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
                }),
            "invalid release repository",
        )?;
    }
    require(
        m.compatibility.os == "darwin"
            && m.compatibility.architecture == "arm64"
            && matches!(architecture, "arm64" | "arm64e"),
        "manifest is not compatible with Apple Silicon macOS",
    )?;
    require(
        version(os)? >= version(&m.compatibility.minimum_mac_os)?,
        "macOS is below the release minimum",
    )?;
    artifact(&m.artifacts.host)?;
    artifact(&m.artifacts.guest_image)?;
    println!(
        "{}\n{}\n{}\n{}\n{}",
        m.version,
        m.artifacts.host.url,
        m.artifacts.host.sha256,
        m.artifacts.guest_image.url,
        m.artifacts.guest_image.sha256
    );
    Ok(())
}

pub(super) fn bootstrap(
    path: &str,
    version: &str,
    commit: &str,
    host: &str,
    host_hash: &str,
    guest: &str,
    guest_hash: &str,
) -> Result<()> {
    let value = json!({"schemaVersion":2,"channel":"stable","version":version,"commit":commit,
        "validationMode":"github-hosted-no-vm", "compatibility":{"os":"darwin","architecture":"arm64","minimumMacOS":"13.0"},
        "artifacts":{"host":{"url":format!("file://{host}"),"sha256":host_hash},
        "guestImage":{"url":guest,"sha256":guest_hash}}});
    files::publish(Path::new(path), format!("{value}\n").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schema_rejects_duplicate_and_null_repository() {
        let valid = r#"{"schemaVersion":2,"channel":"stable","version":"v1.2.3","commit":"a","validationMode":"physical-apple-silicon","compatibility":{"os":"darwin","architecture":"arm64","minimumMacOS":"13.0"},"artifacts":{"host":{"url":"x","sha256":"x"},"guestImage":{"url":"x","sha256":"x"}}}"#;
        assert!(serde_json::from_str::<Manifest>(valid).is_ok());
        for bad in [
            valid.replacen(
                "\"schemaVersion\":2",
                "\"schemaVersion\":2,\"schemaVersion\":2",
                1,
            ),
            valid.replacen(
                "\"os\":\"darwin\"",
                "\"os\":\"darwin\",\"os\":\"darwin\"",
                1,
            ),
            valid.replacen('{', "{\"repository\":null,", 1),
        ] {
            assert!(serde_json::from_str::<Manifest>(&bad).is_err());
        }
    }
    #[test]
    fn version_boundaries() {
        assert_eq!(version("13").unwrap(), version("13.0.0").unwrap());
        for bad in [
            "",
            "13.",
            "13.0.1.2",
            "13.-1",
            "999999999999999999999999999",
        ] {
            assert!(version(bad).is_err());
        }
    }
}
