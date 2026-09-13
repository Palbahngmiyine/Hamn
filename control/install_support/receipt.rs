//! Version-1 receipts remain byte-compatible with the original Python tree hash.
//! A failed check is advisory (normal verified install); write failures abort the
//! transaction. The caller owns the install locks and generation lifetime.
use super::{Result, files, require};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::MetadataExt, path::Path};

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
        entries.push(json!([name, m.mode() & 0o7777, files::digest(path)?]));
    }
    Ok(())
}
fn installed_digest(generation: &Path) -> Result<String> {
    files::owned(generation, true, None)?;
    let mut entries = Vec::new();
    visit(&generation.join("bin"), "bin", &mut entries)?;
    for name in ["scripts", "packaging"] {
        visit(
            &generation.join("share/hamn/src").join(name),
            name,
            &mut entries,
        )?;
    }
    Ok(files::hash(
        canonical_json(&Value::Array(entries))?.as_bytes(),
    ))
}

pub(super) fn run(
    mode: &str,
    target: &str,
    version: &str,
    host_hash: &str,
    guest_hash: &str,
    cache: &str,
) -> Result<()> {
    let generation = files::parent(files::parent(Path::new(target))?)?;
    let path = generation.join(".hamn-release.json");
    if mode == "check" {
        let info = files::owned(&path, false, Some(0o600))?;
        require(info.len() <= 4096, "invalid receipt size")?;
        let r: Receipt = serde_json::from_slice(&fs::read(&path)?)?;
        require(
            r.schema_version == 1
                && r.version == version
                && r.host == host_hash
                && r.guest == guest_hash,
            "different release",
        )?;
        require(
            r.installed == installed_digest(generation)?,
            "installed files changed",
        )?;
        let cache = Path::new(cache);
        let selection = cache.join("guest-image.json");
        require(
            files::owned(&selection, false, None)?.len() <= 4096,
            "invalid selection size",
        )?;
        let name = format!("hamn-guest-{guest_hash}.img");
        let selection: Value = serde_json::from_slice(&fs::read(selection)?)?;
        require(
            selection == json!({"schemaVersion":1,"file":name,"sha256":guest_hash}),
            "different image selection",
        )?;
        let marker = cache.join(format!("{name}.verified"));
        require(
            files::owned(&marker, false, None)?.len() <= 128
                && files::text(&marker)?.trim() == guest_hash,
            "invalid image verification marker",
        )?;
        files::owned(&cache.join(&name), false, None)?;
        require(
            files::digest(&cache.join(name))? == guest_hash,
            "cached image changed",
        )?;
    } else if mode == "write" {
        let r = Receipt {
            schema_version: 1,
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
    #[test]
    fn python_compatible_unicode_and_surrogate_encoding() {
        assert_eq!(
            canonical_json(&json!(["한😀\n", 493, null])).unwrap(),
            r#"["\ud55c\ud83d\ude00\n",493,null]"#
        );
    }
}
