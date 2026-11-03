//! Version-2 receipts hash a generation's installed payload as compact
//! canonical JSON: depth-first `[path, mode, sha256-or-null]` entries for its
//! `bin` and `share` trees (children sorted), with non-ASCII characters
//! escaped as UTF-16 `\uXXXX` units, so receipts written by every release
//! compare byte for byte. Version-1 receipts (the earlier layout's
//! `scripts`/`packaging` trees) never match.
//! A failed check is advisory (normal verified install); write failures abort the
//! transaction. The caller owns the install locks and generation lifetime.
use super::{Result, download, files, manifest::hexadecimal, require};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::MetadataExt, path::Path};

/// Receipts of the generation payload layout (`bin` and `share`).
const SCHEMA_VERSION: u32 = 2;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Receipt {
    schema_version: u32,
    version: String,
    #[serde(rename = "hostSHA256")]
    host: String,
    #[serde(rename = "guestSHA256")]
    guest: String,
    #[serde(rename = "installedSHA256")]
    installed: String,
}

fn canonical_json(value: &Value) -> Result<String> {
    let text = serde_json::to_string(value)?;
    let mut ascii = String::new();
    for ch in text.chars() {
        if ch.is_ascii() {
            ascii.push(ch);
        } else {
            for unit in ch.encode_utf16(&mut [0; 2]) {
                ascii.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    Ok(ascii)
}
fn visit(path: &Path, name: &str, entries: &mut Vec<Value>) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    files::owned(path, m.is_dir(), None)?;
    require(m.mode() & 0o022 == 0, "unsafe installed permissions")?;
    if m.is_dir() {
        entries.push(json!([name, m.mode() & 0o7777, null]));
        let mut children = fs::read_dir(path)?
            .map(|p| p.map(|p| p.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        children.sort();
        for child in children {
            let leaf = child
                .file_name()
                .and_then(|p| p.to_str())
                .ok_or("invalid receipt filename")?;
            visit(&child, &format!("{name}/{leaf}"), entries)?;
        }
    } else {
        entries.push(json!([name, m.mode() & 0o7777, download::digest(path)?]));
    }
    Ok(())
}
fn installed_digest(generation: &Path) -> Result<String> {
    let metadata = files::owned(generation, true, None)?;
    require(
        metadata.mode() & 0o022 == 0,
        "unsafe generation permissions",
    )?;
    let mut entries = Vec::new();
    for name in ["bin", "share"] {
        visit(&generation.join(name), name, &mut entries)?;
    }
    Ok(files::hash(
        canonical_json(&Value::Array(entries))?.as_bytes(),
    ))
}

fn read(path: &Path) -> Result<Receipt> {
    Ok(serde_json::from_slice(&download::read_file(
        path, 4096, true,
    )?)?)
}

/// The release artifacts that a receipt names, as SHA-256 digests.
pub(super) struct Digests {
    pub(super) host: String,
    pub(super) guest: String,
}

/// The digests that the receipt of `generation` (a generation directory)
/// records. An absent, unsafe, malformed, other-schema or non-digest receipt
/// is an error: it names no host archive or guest image.
pub(super) fn digests(generation: &Path) -> Result<Digests> {
    let r = read(&generation.join(".hamn-release.json"))?;
    require(
        r.schema_version == SCHEMA_VERSION && hexadecimal(&r.host, 64) && hexadecimal(&r.guest, 64),
        "release receipt is not a current receipt with host archive and guest image digests",
    )?;
    Ok(Digests {
        host: r.host,
        guest: r.guest,
    })
}

pub(super) fn run(
    mode: &str,
    target: &str,
    version: &str,
    host_hash: &str,
    guest_hash: &str,
) -> Result<()> {
    let generation = files::parent(files::parent(Path::new(target))?)?;
    let path = generation.join(".hamn-release.json");
    if mode == "host-check" {
        let r = read(&path)?;
        require(
            r.schema_version == SCHEMA_VERSION
                && r.version == version
                && r.host == host_hash
                && r.guest == guest_hash,
            "different release",
        )?;
        require(
            r.installed == installed_digest(generation)?,
            "installed files changed",
        )?;
    } else if mode == "write" {
        let r = Receipt {
            schema_version: SCHEMA_VERSION,
            version: version.into(),
            host: host_hash.into(),
            guest: guest_hash.into(),
            installed: installed_digest(generation)?,
        };
        files::publish(
            &path,
            format!("{}\n", serde_json::to_string(&r)?).as_bytes(),
        )?;
    } else {
        return Err("unknown receipt operation".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn host_check_rejects_changed_or_writable_host_and_unknown_modes() {
        let temp = super::super::test_support::Temp::new();
        let generation = temp.0.join("generation");
        for directory in ["bin", "share/hamn"] {
            fs::create_dir_all(generation.join(directory)).unwrap();
        }
        let pointer = generation.join("share/hamn/update-manifest-url");
        fs::write(&pointer, b"https://example.test/manifest\n").unwrap();
        let target = generation.join("bin/hamn");
        fs::write(&target, b"host bytes").unwrap();
        let target = target.to_str().unwrap();
        run("write", target, "v1.2.3", "host", "guest").unwrap();
        run("host-check", target, "v1.2.3", "host", "guest").unwrap();
        // The manifest pointer is part of the installed payload.
        fs::write(&pointer, b"https://example.test/other\n").unwrap();
        assert!(run("host-check", target, "v1.2.3", "host", "guest").is_err());
        fs::write(&pointer, b"https://example.test/manifest\n").unwrap();
        run("host-check", target, "v1.2.3", "host", "guest").unwrap();
        // An earlier (version 1) receipt with the same identities is stale.
        let path = generation.join(".hamn-release.json");
        let current = fs::read_to_string(&path).unwrap();
        fs::remove_file(&path).unwrap();
        fs::write(
            &path,
            current.replace("\"schemaVersion\":2", "\"schemaVersion\":1"),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(run("host-check", target, "v1.2.3", "host", "guest").is_err());
        fs::remove_file(&path).unwrap();
        run("write", target, "v1.2.3", "host", "guest").unwrap();
        assert!(run("host-check", target, "v1.2.4", "host", "guest").is_err());
        assert!(run("host-check", target, "v1.2.3", "host", "other").is_err());
        // The retired guest-checking `check` mode is not an alias.
        assert!(run("check", target, "v1.2.3", "host", "guest").is_err());
        fs::set_permissions(target, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(run("host-check", target, "v1.2.3", "host", "guest").is_err());
        fs::set_permissions(target, fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(target, b"changed host bytes").unwrap();
        assert!(run("host-check", target, "v1.2.3", "host", "guest").is_err());
    }

    #[test]
    fn digests_come_only_from_a_current_receipt_with_both_digests() {
        let temp = super::super::test_support::Temp::new();
        let generation = temp.0.join("generation");
        fs::create_dir_all(generation.join("bin")).unwrap();
        fs::create_dir_all(generation.join("share")).unwrap();
        let target = generation.join("bin/hamn");
        fs::write(&target, b"host bytes").unwrap();
        let target = target.to_str().unwrap();
        let path = generation.join(".hamn-release.json");
        assert!(digests(&generation).is_err(), "no receipt");
        let (host, guest) = ("a".repeat(64), "b".repeat(64));
        run("write", target, "v1.2.3", &host, &guest).unwrap();
        let read = digests(&generation).unwrap();
        assert_eq!((read.host.as_str(), read.guest.as_str()), (&*host, &*guest));
        let current = fs::read_to_string(&path).unwrap();
        for (label, damaged) in [
            (
                "older schema",
                current.replace("\"schemaVersion\":2", "\"schemaVersion\":1"),
            ),
            (
                "guest not a digest",
                current.replace(&guest, &"B".repeat(64)),
            ),
            ("guest short", current.replace(&guest, "bb")),
            ("host not a digest", current.replace(&host, &"A".repeat(64))),
            ("host short", current.replace(&host, "aa")),
        ] {
            fs::remove_file(&path).unwrap();
            fs::write(&path, damaged).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            assert!(digests(&generation).is_err(), "{label}");
        }
        fs::remove_file(&path).unwrap();
        fs::write(&path, &current).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(digests(&generation).unwrap().host, host);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            digests(&generation).is_err(),
            "a receipt with an unsafe mode"
        );
    }

    #[test]
    fn non_ascii_names_are_escaped_as_utf16_units() {
        assert_eq!(
            canonical_json(&json!(["한😀\n", 493, null])).unwrap(),
            r#"["\ud55c\ud83d\ude00\n",493,null]"#
        );
    }
}
