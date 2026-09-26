//! Strict bounded schema v3 release metadata. Typed decoding rejects
//! duplicate, unknown and null keys; every artifact names its exact byte size.
//! Other schema versions (including the retired v2) are refused by version
//! before field decoding, so the error names the unsupported schema.
use super::{Result, download, files, require};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::json;
use std::path::Path;

/// The only release manifest schema this client reads.
const SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Manifest {
    schema_version: u32,
    channel: String,
    pub version: String,
    commit: String,
    validation_mode: String,
    compatibility: Compatibility,
    pub artifacts: Artifacts,
}
fn some<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> std::result::Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Compatibility {
    os: String,
    architecture: String,
    #[serde(rename = "minimumMacOS")]
    minimum_mac_os: String,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Artifacts {
    pub host: Artifact,
    pub guest_image: Artifact,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Artifact {
    pub url: String,
    pub sha256: String,
    /// Exact byte size, 1..=the artifact kind's limit.
    pub size: u64,
    #[serde(
        default,
        deserialize_with = "some",
        skip_serializing_if = "Option::is_none"
    )]
    format: Option<String>,
    #[serde(
        default,
        deserialize_with = "some",
        skip_serializing_if = "Option::is_none"
    )]
    compression: Option<String>,
    #[serde(
        default,
        deserialize_with = "some",
        skip_serializing_if = "Option::is_none"
    )]
    virtual_size: Option<u64>,
}
impl Artifact {
    pub fn acquisition(&self) -> download::Artifact {
        download::Artifact {
            url: self.url.clone(),
            sha256: self.sha256.clone(),
            size: self.size,
        }
    }
}
impl Manifest {
    pub fn artifact(&self, name: &str) -> Result<&Artifact> {
        match name {
            "host" => Ok(&self.artifacts.host),
            "guestImage" => Ok(&self.artifacts.guest_image),
            _ => Err("unknown release artifact".into()),
        }
    }
    pub fn print_fields(&self) {
        println!(
            "{}\n{}\n{}\n{}\n{}",
            self.version,
            self.artifacts.host.url,
            self.artifacts.host.sha256,
            self.artifacts.guest_image.url,
            self.artifacts.guest_image.sha256
        );
    }
}
pub(super) fn hexadecimal(value: &str, size: usize) -> bool {
    value.len() == size
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
pub(super) fn stable_version(value: &str) -> Result<[u32; 3]> {
    let parts: Vec<_> = value
        .strip_prefix('v')
        .unwrap_or(value)
        .split('.')
        .collect();
    require(parts.len() == 3, "version must be canonical stable X.Y.Z")?;
    let mut result = [0; 3];
    for (index, part) in parts.iter().enumerate() {
        require(
            !part.is_empty()
                && (part.len() == 1 || !part.starts_with('0'))
                && part.bytes().all(|c| c.is_ascii_digit()),
            "version must be canonical stable X.Y.Z",
        )?;
        result[index] = part.parse().map_err(|_| "version component overflow")?;
    }
    Ok(result)
}
fn system_version(value: &str) -> Result<[u64; 3]> {
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
/// Only the schema version, read before strict field decoding so that a
/// manifest of another schema is refused by its version, not by a field.
#[derive(Deserialize)]
struct Schema {
    #[serde(rename = "schemaVersion")]
    schema_version: u32,
}
fn decode(data: &[u8], urls: bool) -> Result<Manifest> {
    require(
        data.len() as u64 <= download::MANIFEST_LIMIT,
        "manifest exceeds 256 KiB",
    )?;
    let schema: Schema = serde_json::from_slice(data)?;
    if schema.schema_version != SCHEMA_VERSION {
        return Err(format!(
            "manifest schema v{} is not supported; this Hamn reads only schema v{SCHEMA_VERSION}",
            schema.schema_version
        )
        .into());
    }
    let mut value: Manifest = serde_json::from_slice(data)?;
    require(
        value.schema_version == SCHEMA_VERSION && value.channel == "stable",
        "manifest is not a stable schema v3 release",
    )?;
    stable_version(&value.version)?;
    value.version = format!(
        "v{}",
        value.version.strip_prefix('v').unwrap_or(&value.version)
    );
    require(hexadecimal(&value.commit, 40), "invalid release commit")?;
    require(
        matches!(
            value.validation_mode.as_str(),
            "github-hosted-no-vm" | "physical-apple-silicon"
        ),
        "invalid release validation mode",
    )?;
    require(
        value.compatibility.os == "darwin" && value.compatibility.architecture == "arm64",
        "manifest is not compatible with Apple Silicon macOS",
    )?;
    system_version(&value.compatibility.minimum_mac_os)?;
    for (name, artifact, limit) in [
        ("host", &value.artifacts.host, download::HOST_LIMIT),
        (
            "guestImage",
            &value.artifacts.guest_image,
            download::GUEST_LIMIT,
        ),
    ] {
        require(
            !artifact.url.is_empty() && artifact.url.bytes().all(|c| (33..=126).contains(&c)),
            "invalid artifact URL",
        )?;
        if urls {
            download::validate_url(&artifact.url)?;
        }
        require(
            hexadecimal(&artifact.sha256, 64),
            "invalid artifact SHA-256",
        )?;
        require(
            (1..=limit).contains(&artifact.size),
            "artifact size outside permitted range",
        )?;
        if name == "guestImage" {
            require(
                artifact.format.as_deref() == Some("qcow2")
                    && artifact.compression.as_deref() == Some("zlib")
                    && artifact.virtual_size == Some(8 * 1024 * 1024 * 1024),
                "unsupported guest image format",
            )?;
        } else {
            require(
                artifact.format.is_none()
                    && artifact.compression.is_none()
                    && artifact.virtual_size.is_none(),
                "unexpected artifact image metadata",
            )?;
        }
    }
    Ok(value)
}
pub(super) fn parse(data: &[u8], os: &str, architecture: &str) -> Result<Manifest> {
    let value = decode(data, true)?;
    compatible(&value, os, architecture)?;
    Ok(value)
}
fn compatible(value: &Manifest, os: &str, architecture: &str) -> Result<()> {
    require(
        matches!(architecture, "arm64" | "arm64e"),
        "manifest is not compatible with Apple Silicon macOS",
    )?;
    require(
        system_version(os)? >= system_version(&value.compatibility.minimum_mac_os)?,
        "macOS is below the release minimum",
    )
}
pub(super) fn load(path: &Path) -> Result<Manifest> {
    decode(
        &download::read_file(path, download::MANIFEST_LIMIT, false)?,
        true,
    )
}
pub(super) fn bootstrap_v3(
    path: &str,
    version: &str,
    commit: &str,
    host: &str,
    host_hash: &str,
    guest: &str,
    guest_hash: &str,
    host_size: u64,
    guest_size: u64,
) -> Result<()> {
    let value = json!({"schemaVersion":3,"channel":"stable","version":version,"commit":commit,
        "validationMode":"github-hosted-no-vm", "compatibility":{"os":"darwin","architecture":"arm64","minimumMacOS":"13.0"},
        "artifacts":{"host":{"url":format!("file://{host}"),"sha256":host_hash,"size":host_size},
            "guestImage":{"url":guest,"sha256":guest_hash,"size":guest_size,"format":"qcow2","compression":"zlib","virtualSize":8_u64*1024*1024*1024}}});
    let value = decode(&serde_json::to_vec(&value)?, false)?;
    files::publish(
        Path::new(path),
        format!("{}\n", serde_json::to_string(&value)?).as_bytes(),
    )
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    /// A valid stable schema v3 release with 1-byte host and guest artifacts.
    pub(crate) fn fixture() -> serde_json::Value {
        json!({"schemaVersion":3,"channel":"stable","version":"v1.2.3","commit":"a".repeat(40),
            "validationMode":"physical-apple-silicon",
            "compatibility":{"os":"darwin","architecture":"arm64","minimumMacOS":"13.0"},
            "artifacts":{"host":{"url":"https://example.test/host","sha256":"b".repeat(64),"size":1},
                "guestImage":{"url":"https://example.test/guest","sha256":"c".repeat(64),"size":1,
                    "format":"qcow2","compression":"zlib","virtualSize":8_u64*1024*1024*1024}}})
    }
    #[test]
    fn schemas_reject_duplicates_nulls_extra_keys_and_overflow() {
        assert!(parse(&serde_json::to_vec(&fixture()).unwrap(), "13", "arm64e").is_ok());
        let source = fixture().to_string();
        for bad in [
            source.replacen(
                "\"schemaVersion\":3",
                "\"schemaVersion\":3,\"schemaVersion\":3",
                1,
            ),
            source.replacen(
                "\"os\":\"darwin\"",
                "\"os\":\"darwin\",\"os\":\"darwin\"",
                1,
            ),
            source.replacen("\"size\":1", "\"size\":null", 1),
            source.replacen("\"size\":1", "\"size\":true", 1),
            source.replacen("\"size\":1", "\"size\":18446744073709551616", 1),
            source.replacen('{', "{\"repository\":null,", 1),
            // The retired v2 `repository` extension is an unknown key in v3.
            source.replacen('{', "{\"repository\":\"owner/hamn\",", 1),
        ] {
            assert!(parse(bad.as_bytes(), "13", "arm64").is_err(), "{bad}");
        }
        assert!(parse(source.as_bytes(), "12.9", "arm64").is_err());
        assert!(parse(source.as_bytes(), "13", "x86_64").is_err());
    }
    #[test]
    fn retired_schema_v2_and_other_versions_are_refused_by_version() {
        // Schema v2 (no sizes, optional `repository`) is no longer read. The
        // refusal names the schema instead of a missing size field.
        let mut v2 = fixture();
        v2["schemaVersion"] = 2.into();
        v2["repository"] = "owner/hamn".into();
        for name in ["host", "guestImage"] {
            let artifact = v2["artifacts"][name].as_object_mut().unwrap();
            artifact.retain(|key, _| key == "url" || key == "sha256");
        }
        let with_schema = |schema: u32| {
            let mut value = fixture();
            value["schemaVersion"] = schema.into();
            value
        };
        for (schema, value) in [
            (2, v2),
            (2, with_schema(2)),
            (0, with_schema(0)),
            (1, with_schema(1)),
            (4, with_schema(4)),
        ] {
            let error = parse(&serde_json::to_vec(&value).unwrap(), "13", "arm64")
                .unwrap_err()
                .to_string();
            assert_eq!(
                error,
                format!("manifest schema v{schema} is not supported; this Hamn reads only schema v3"),
                "{value}"
            );
        }
        // A v3 manifest without an artifact size is rejected, never defaulted.
        for name in ["host", "guestImage"] {
            let mut missing = fixture();
            let artifact = missing["artifacts"][name].as_object_mut().unwrap();
            artifact.remove("size");
            let error = parse(&serde_json::to_vec(&missing).unwrap(), "13", "arm64")
                .unwrap_err()
                .to_string();
            assert!(error.contains("missing field `size`"), "{name}: {error}");
        }
        for bad in [
            &b"{}"[..],
            b"[]",
            b"{\"schemaVersion\":true}",
            b"{\"schemaVersion\":null}",
            b"{\"schemaVersion\":3,\"schemaVersion\":2}",
        ] {
            assert!(parse(bad, "13", "arm64").is_err());
        }
    }
    #[test]
    fn stable_versions_are_canonical_numeric_and_bounded() {
        assert_eq!(stable_version("v4294967295.2.3").unwrap(), [u32::MAX, 2, 3]);
        assert!(stable_version("1.10.0").unwrap() > stable_version("1.9.99").unwrap());
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "1.2.3-rc.1",
            "1.2.3+build",
            "4294967296.0.0",
            " 1.2.3",
            "1.2.3\n",
            "V1.2.3",
            "vv1.2.3",
            "1..3",
        ] {
            assert!(stable_version(bad).is_err(), "{bad}");
        }
        assert_eq!(
            system_version("13").unwrap(),
            system_version("13.0.0").unwrap()
        );
        for bad in [
            "",
            "13.",
            "13.0.1.2",
            "13.-1",
            "999999999999999999999999999",
        ] {
            assert!(system_version(bad).is_err());
        }
    }

    /// Deterministic LCG shared by the generated cases below (no RNG crate).
    fn generator(seed: u64) -> impl FnMut() -> u64 {
        let mut state = seed;
        move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            state
        }
    }

    #[test]
    fn generated_stable_versions_order_numerically_with_optional_prefix() {
        // Property 1 (stable semantic version ordering), seed 20260921:
        // 200 pairs. Odd cases draw small components so equal leading parts
        // force comparison of later components; even cases span all of u32.
        let mut next = generator(20260921);
        let text = |v: [u32; 3]| format!("{}.{}.{}", v[0], v[1], v[2]);
        for case in 0..200 {
            let mut component = || {
                let value = (next() >> 32) as u32;
                if case % 2 == 1 { value % 3 } else { value }
            };
            let left = [component(), component(), component()];
            let right = [component(), component(), component()];
            let parsed_left = stable_version(&format!("v{}", text(left))).unwrap();
            let parsed_right = stable_version(&text(right)).unwrap();
            assert_eq!((parsed_left, parsed_right), (left, right), "case {case}");
            assert_eq!(parsed_left < parsed_right, left < right, "case {case}");
            assert_eq!(parsed_left == parsed_right, left == right, "case {case}");
        }
    }

    #[test]
    fn v3_round_trips_and_field_violations_are_rejected() {
        let v3 = fixture();
        let parsed = parse(&serde_json::to_vec(&v3).unwrap(), "13", "arm64").unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), v3);
        let mut boundary = fixture();
        boundary["artifacts"]["host"]["size"] = download::HOST_LIMIT.into();
        boundary["artifacts"]["guestImage"]["size"] = download::GUEST_LIMIT.into();
        assert!(parse(&serde_json::to_vec(&boundary).unwrap(), "13", "arm64").is_ok());

        let mut cases = Vec::new();
        for (key, value) in [
            ("schemaVersion", json!(true)),
            ("schemaVersion", json!(4)),
            ("channel", json!("beta")),
            ("unexpected", json!(1)),
            ("version", json!("v01.2.3")),
            ("version", json!(true)),
            ("commit", json!("A".repeat(40))),
            ("validationMode", json!("unverified")),
        ] {
            let mut item = fixture();
            item[key] = value.clone();
            cases.push((format!("{key}={value}"), item));
        }
        for (name, key, value) in [
            ("host", "size", json!(0)),
            ("host", "size", json!(download::HOST_LIMIT + 1)),
            ("guestImage", "size", json!(download::GUEST_LIMIT + 1)),
            ("host", "url", json!("http://example.test/host")),
            ("host", "url", json!("https://user@example.test/host")),
            ("host", "sha256", json!("A".repeat(64))),
            ("host", "sha256", json!("b".repeat(63))),
            ("guestImage", "format", json!("raw")),
            ("guestImage", "virtualSize", json!(1)),
        ] {
            let mut item = fixture();
            item["artifacts"][name][key] = value.clone();
            cases.push((format!("{name}.{key}={value}"), item));
        }
        for (label, item) in cases {
            assert!(
                parse(&serde_json::to_vec(&item).unwrap(), "13.0", "arm64").is_err(),
                "{label}"
            );
        }

        assert!(parse(b"NaN", "13", "arm64").is_err());
        let mut exact = serde_json::to_vec(&fixture()).unwrap();
        exact.resize(download::MANIFEST_LIMIT as usize, b' ');
        assert!(parse(&exact, "13", "arm64").is_ok());
        exact.push(b' ');
        assert!(parse(&exact, "13", "arm64").is_err());
    }

    #[test]
    fn generated_manifests_round_trip_their_artifact_identities() {
        // Seed 20260922: 16 publisher-shaped releases. Parsing keeps each
        // artifact's URL, digest and size exactly (the bytes to acquire).
        let mut next = generator(20260922);
        let mut hex = |length: usize| -> String {
            (0..length)
                .map(|_| char::from_digit((next() >> 60) as u32, 16).unwrap())
                .collect()
        };
        for case in 0..16 {
            let mut value = fixture();
            let version = format!(
                "v{}.{}.{}",
                u32::from_str_radix(&hex(8), 16).unwrap(),
                u32::from_str_radix(&hex(8), 16).unwrap(),
                u32::from_str_radix(&hex(8), 16).unwrap()
            );
            value["version"] = version.clone().into();
            value["commit"] = hex(40).into();
            let mut expected = Vec::new();
            for name in ["host", "guestImage"] {
                let artifact = &mut value["artifacts"][name];
                let url = format!("https://fixture.test/{version}/{name}-{case}");
                let (sha256, size) = (hex(64), 1 + u64::from_str_radix(&hex(4), 16).unwrap());
                artifact["url"] = url.clone().into();
                artifact["sha256"] = sha256.clone().into();
                artifact["size"] = size.into();
                expected.push((url, sha256, size));
            }
            let parsed = parse(&serde_json::to_vec(&value).unwrap(), "13.0", "arm64").unwrap();
            assert_eq!(parsed.version, version, "case {case}");
            let actual: Vec<_> = [&parsed.artifacts.host, &parsed.artifacts.guest_image]
                .into_iter()
                .map(|artifact| (artifact.url.clone(), artifact.sha256.clone(), artifact.size))
                .collect();
            assert_eq!(actual, expected, "case {case}");
        }
    }

    #[test]
    fn bootstrap_writes_a_strict_v3_manifest_that_load_reads_back() {
        let temp = super::super::test_support::Temp::new();
        let path = temp.0.join("manifest.json");
        let (host, guest) = ("b".repeat(64), "c".repeat(64));
        let write = |path: &Path, host_size: u64| {
            bootstrap_v3(
                path.to_str().unwrap(),
                "v1.2.3",
                &"a".repeat(40),
                "/private/host.tar.gz",
                &host,
                "https://example.test/guest",
                &guest,
                host_size,
                9,
            )
        };
        write(&path, 7).unwrap();
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["schemaVersion"], 3);
        assert_eq!(
            written["artifacts"]["host"],
            json!({"url": "file:///private/host.tar.gz", "sha256": host, "size": 7})
        );
        assert_eq!(written["artifacts"]["guestImage"]["size"], 9);
        // A size outside the artifact limit is refused before publication.
        let refused = temp.0.join("refused.json");
        assert!(write(&refused, 0).is_err());
        assert!(write(&refused, download::HOST_LIMIT + 1).is_err());
        assert!(!refused.exists());
    }
}
