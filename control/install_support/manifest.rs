//! Strict bounded v2/v3 metadata. Typed decoding rejects duplicate/unknown keys.
//! The legacy installer keeps its five-line interface; upgrades validate URLs.
use super::{Result, download, files, require};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::json;
use std::path::Path;

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
    #[serde(
        default,
        deserialize_with = "some",
        skip_serializing_if = "Option::is_none"
    )]
    repository: Option<String>,
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
    #[serde(
        default,
        deserialize_with = "some",
        skip_serializing_if = "Option::is_none"
    )]
    pub size: Option<u64>,
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
fn decode(data: &[u8], urls: bool) -> Result<Manifest> {
    require(
        data.len() as u64 <= download::MANIFEST_LIMIT,
        "manifest exceeds 256 KiB",
    )?;
    let mut value: Manifest = serde_json::from_slice(data)?;
    require(
        matches!(value.schema_version, 2 | 3) && value.channel == "stable",
        "manifest is not a stable schema v2 or v3 release",
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
    if let Some(repository) = value.repository.take() {
        let parts: Vec<_> = repository.split('/').collect();
        require(
            value.schema_version == 2
                && parts.len() == 2
                && parts.iter().all(|p| {
                    !p.is_empty()
                        && p.bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
                }),
            "invalid release repository",
        )?;
    }
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
        if value.schema_version == 3 {
            require(
                artifact
                    .size
                    .is_some_and(|size| (1..=limit).contains(&size)),
                "artifact size outside permitted range",
            )?;
        } else {
            require(artifact.size.is_none(), "unexpected v2 artifact size")?;
        }
        if value.schema_version == 3 && name == "guestImage" {
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
pub(super) fn fields(path: &str, os: &str, architecture: &str) -> Result<()> {
    let value = decode(
        &download::read_file(Path::new(path), download::MANIFEST_LIMIT, false)?,
        false,
    )?;
    compatible(&value, os, architecture)?;
    value.print_fields();
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
        "artifacts":{"host":{"url":format!("file://{host}"),"sha256":host_hash}, "guestImage":{"url":guest,"sha256":guest_hash}}});
    files::publish(Path::new(path), format!("{value}\n").as_bytes())
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
    pub(crate) fn fixture(schema: u32) -> serde_json::Value {
        let mut value = json!({"schemaVersion":schema,"channel":"stable","version":"v1.2.3","commit":"a".repeat(40),"validationMode":"physical-apple-silicon","compatibility":{"os":"darwin","architecture":"arm64","minimumMacOS":"13.0"},"artifacts":{"host":{"url":"https://example.test/host","sha256":"b".repeat(64)},"guestImage":{"url":"https://example.test/guest","sha256":"c".repeat(64)}}});
        if schema == 3 {
            value["artifacts"]["host"]["size"] = 1.into();
            let guest = value["artifacts"]["guestImage"].as_object_mut().unwrap();
            guest.extend([
                ("size".into(), 1.into()),
                ("format".into(), "qcow2".into()),
                ("compression".into(), "zlib".into()),
                ("virtualSize".into(), (8_u64 * 1024 * 1024 * 1024).into()),
            ]);
        }
        value
    }
    #[test]
    fn schemas_reject_duplicates_nulls_extra_keys_and_overflow() {
        for schema in [2, 3] {
            assert!(
                parse(
                    &serde_json::to_vec(&fixture(schema)).unwrap(),
                    "13",
                    "arm64e"
                )
                .is_ok()
            );
        }
        let source = fixture(3).to_string();
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
        ] {
            assert!(parse(bad.as_bytes(), "13", "arm64").is_err(), "{bad}");
        }
        let mut old = fixture(2);
        old["repository"] = "legacy/hamn".into();
        assert!(
            serde_json::to_value(parse(&serde_json::to_vec(&old).unwrap(), "13", "arm64").unwrap())
                .unwrap()
                .get("repository")
                .is_none()
        );
        old["schemaVersion"] = 3.into();
        assert!(parse(&serde_json::to_vec(&old).unwrap(), "13", "arm64").is_err());
        assert!(parse(source.as_bytes(), "12.9", "arm64").is_err());
        assert!(parse(source.as_bytes(), "13", "x86_64").is_err());
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
        let v3 = fixture(3);
        let parsed = parse(&serde_json::to_vec(&v3).unwrap(), "13", "arm64").unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), v3);
        let mut boundary = fixture(3);
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
            let mut item = fixture(3);
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
            let mut item = fixture(3);
            item["artifacts"][name][key] = value.clone();
            cases.push((format!("{name}.{key}={value}"), item));
        }
        let mut repository = fixture(2);
        repository["repository"] = "bad/extra/name".into();
        cases.push(("v2 repository=bad/extra/name".into(), repository));
        for (label, item) in cases {
            assert!(
                parse(&serde_json::to_vec(&item).unwrap(), "13.0", "arm64").is_err(),
                "{label}"
            );
        }

        assert!(parse(b"NaN", "13", "arm64").is_err());
        let mut exact = serde_json::to_vec(&fixture(3)).unwrap();
        exact.resize(download::MANIFEST_LIMIT as usize, b' ');
        assert!(parse(&exact, "13", "arm64").is_ok());
        exact.push(b' ');
        assert!(parse(&exact, "13", "arm64").is_err());
    }

    #[test]
    fn generated_v2_and_v3_manifests_name_identical_artifacts() {
        // Dual publication, seed 20260922: 16 publisher-shaped releases. V3
        // only adds sizes and image metadata; both must select the same bytes.
        let mut next = generator(20260922);
        let mut hex = |length: usize| -> String {
            (0..length)
                .map(|_| char::from_digit((next() >> 60) as u32, 16).unwrap())
                .collect()
        };
        for case in 0..16 {
            let mut v3 = fixture(3);
            let version = format!(
                "v{}.{}.{}",
                u32::from_str_radix(&hex(8), 16).unwrap(),
                u32::from_str_radix(&hex(8), 16).unwrap(),
                u32::from_str_radix(&hex(8), 16).unwrap()
            );
            v3["version"] = version.clone().into();
            v3["commit"] = hex(40).into();
            for name in ["host", "guestImage"] {
                let artifact = &mut v3["artifacts"][name];
                artifact["url"] = format!("https://fixture.test/{version}/{name}-{case}").into();
                artifact["sha256"] = hex(64).into();
                artifact["size"] = (1 + u64::from_str_radix(&hex(4), 16).unwrap()).into();
            }
            let mut v2 = v3.clone();
            v2["schemaVersion"] = 2.into();
            for name in ["host", "guestImage"] {
                let artifact = v2["artifacts"][name].as_object_mut().unwrap();
                artifact.retain(|key, _| key == "url" || key == "sha256");
            }
            let old = parse(&serde_json::to_vec(&v2).unwrap(), "13.0", "arm64").unwrap();
            let new = parse(&serde_json::to_vec(&v3).unwrap(), "13.0", "arm64").unwrap();
            assert_eq!(
                (&old.version, &new.version),
                (&version, &version),
                "case {case}"
            );
            for (before, after) in [
                (&old.artifacts.host, &new.artifacts.host),
                (&old.artifacts.guest_image, &new.artifacts.guest_image),
            ] {
                assert_eq!((&before.url, &before.sha256), (&after.url, &after.sha256));
                assert!(before.size.is_none() && after.size.is_some(), "case {case}");
            }
        }
    }
}
