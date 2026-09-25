//! The release version recorded in the Release Please manifest and its
//! copies in version.txt, the Makefile and flake.nix.
//!
//! - `resolve-version`: turns a manifest increase between two commits into
//!   one release run (GitHub step outputs).
//! - `current-version`: the checked-out manifest version, for recovering an
//!   unpublished release.
//! - `check-version-state`: the repository's release configuration and
//!   version copies agree.
use super::process::{self, Spec};
use super::syntax::Version;
use serde_json::Value;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::Duration;

/// Values of `VERSION ?= value` lines (Python:
/// `^VERSION[ \t]+\?=[ \t]+([^ \t#\r\n]+)[ \t]*$`, multiline).
pub fn makefile_versions(text: &str) -> Vec<&str> {
    fn blank(text: &str) -> &str {
        text.trim_start_matches([' ', '\t'])
    }
    text.split('\n')
        .filter_map(|line| {
            let rest = line.strip_prefix("VERSION")?;
            let rest = Some(blank(rest)).filter(|trimmed| trimmed.len() < rest.len())?.strip_prefix("?=")?;
            let value = Some(blank(rest)).filter(|trimmed| trimmed.len() < rest.len())?;
            let end = value.find([' ', '\t', '#', '\r']).unwrap_or(value.len());
            let (value, after) = value.split_at(end);
            (!value.is_empty() && blank(after).is_empty()).then_some(value)
        })
        .collect()
}

/// Values of `hamnVersion = "value"; # x-release-please-version` lines with
/// the semantics of Python's `re.findall` on
/// `^\s*hamnVersion = "([^"]+)";\s*# x-release-please-version\s*$`
/// (multiline; `\s` also matches newlines).
pub fn flake_versions(text: &str) -> Vec<&str> {
    const PREFIX: &str = "hamnVersion = \"";
    const MARKER: &str = "# x-release-please-version";
    // `\s*`: every whitespace character, newlines included.
    let skip = |index: usize| index + text[index..].len() - text[index..].trim_start().len();
    // One match attempt at a line start: (value, end of match).
    let attempt = |position: usize| -> Option<(&str, usize)> {
        let value_start = skip(position);
        let value_start = text[value_start..].starts_with(PREFIX).then_some(value_start + PREFIX.len())?;
        let value_end = value_start + text[value_start..].find('"').filter(|length| *length > 0)?;
        let marker = skip(text[value_end..].starts_with("\";").then_some(value_end + 2)?);
        let after_marker = text[marker..].starts_with(MARKER).then_some(marker + MARKER.len())?;
        let end = skip(after_marker);
        // `\s*$` ends at the text's end or backtracks to the last newline
        // it consumed.
        let end = if end == text.len() {
            end
        } else {
            text[after_marker..end].rfind('\n').map(|offset| after_marker + offset)?
        };
        Some((&text[value_start..value_end], end))
    };
    let mut found = Vec::new();
    let mut position = 0;
    while position <= text.len() {
        let at_line_start = position == 0 || text.as_bytes()[position - 1] == b'\n';
        match at_line_start.then(|| attempt(position)).flatten() {
            Some((value, end)) => {
                found.push(value);
                position = end.max(position + 1);
            }
            None => position += 1,
        }
        while position < text.len() && !text.is_char_boundary(position) {
            position += 1;
        }
    }
    found
}

/// The root package's entry of a manifest that has no other package.
fn root_entry(manifest: &Value) -> Result<&Value, String> {
    match manifest.as_object() {
        Some(object) if object.len() == 1 && object.contains_key(".") => Ok(&object["."]),
        _ => Err("release manifest must contain only the root package".into()),
    }
}

fn append_outputs(output: &Path, lines: &[String]) -> Result<(), String> {
    let mut file =
        OpenOptions::new().append(true).open(output).map_err(|error| format!("{}: {error}", output.display()))?;
    file.write_all(lines.iter().map(|line| format!("{line}\n")).collect::<String>().as_bytes())
        .map_err(|error| format!("{}: {error}", output.display()))
}

fn git_show(root: &Path, reference: &str, path: &str) -> Result<String, String> {
    process::run(
        OsStr::new("git"),
        &[OsStr::new("-C"), root.as_os_str(), OsStr::new("show"), OsStr::new(&format!("{reference}:{path}"))],
        &Spec::default(),
        Duration::from_secs(60),
    )
}

/// `resolve-version ROOT PREVIOUS_REF COMMIT RUN_ID OUTPUT`
pub fn resolve_version(args: &[String]) -> Result<(), String> {
    let [root, previous, commit, run, output] = args else {
        return Err("usage: hamn-dev release resolve-version ROOT PREVIOUS_REF COMMIT RUN_ID OUTPUT".into());
    };
    let root = Path::new(root);
    let manifest = |reference: &str| -> Result<(String, Version), String> {
        let value = git_show(root, reference, ".release-please-manifest.json")
            .and_then(|text| serde_json::from_str::<Value>(&text).map_err(|error| error.to_string()))
            .map_err(|error| format!("release manifest is invalid: {error}"))?;
        let text = root_entry(&value)?.as_str().ok_or("release version is not a string")?;
        let version = Version::parse(text).ok_or("release version is not canonical SemVer")?;
        Ok((text.to_owned(), version))
    };
    let (_, old) = manifest(previous)?;
    let (text, new) = manifest(commit)?;
    if new <= old {
        return Err("release version did not increase".into());
    }
    if git_show(root, commit, "version.txt")? != format!("{text}\n") {
        return Err("version.txt does not match the release manifest".into());
    }
    if makefile_versions(&git_show(root, commit, "Makefile")?).first() != Some(&text.as_str()) {
        return Err("Makefile does not match the release manifest".into());
    }
    if flake_versions(&git_show(root, commit, "flake.nix")?) != [text.as_str()] {
        return Err("flake.nix does not match the release manifest".into());
    }
    append_outputs(
        Path::new(output),
        &[
            "should_release=true".into(),
            format!("version={text}"),
            format!("stable_tag=v{text}"),
            format!("candidate_tag=v{text}-rc.{run}"),
            format!("commit={commit}"),
        ],
    )
}

/// `current-version ROOT`: prints the checked-out release version after
/// checking that every version copy agrees.
pub fn current_version(args: &[String]) -> Result<(), String> {
    let [root] = args else {
        return Err("usage: hamn-dev release current-version ROOT".into());
    };
    let root = Path::new(root);
    let read = |name: &str| fs::read_to_string(root.join(name)).map_err(|error| format!("{name}: {error}"));
    let manifest: Value =
        serde_json::from_str(&read(".release-please-manifest.json")?).map_err(|error| error.to_string())?;
    let Some(version) = root_entry(&manifest)?.as_str().filter(|version| Version::parse(version).is_some()) else {
        return Err("release version is not canonical SemVer".into());
    };
    if read("version.txt")? != format!("{version}\n") {
        return Err("version.txt does not match the release manifest".into());
    }
    if makefile_versions(&read("Makefile")?) != [version] {
        return Err("Makefile does not match the release manifest".into());
    }
    if flake_versions(&read("flake.nix")?) != [version] {
        return Err("flake.nix does not match the release manifest".into());
    }
    println!("{version}");
    Ok(())
}

/// `check-version-state ROOT`: Release Please policy (initial version 0.0.1,
/// pre-major bumps) and the manifest, version.txt, Makefile and flake.nix
/// versions agree. A 0.0.0 manifest is the bootstrap state, whose sources
/// carry the initial version.
pub fn check_version_state(args: &[String]) -> Result<(), String> {
    let [root] = args else {
        return Err("usage: hamn-dev release check-version-state ROOT".into());
    };
    let root = Path::new(root);
    let read = |relative: &str| {
        let path = root.join(relative);
        let regular = fs::symlink_metadata(&path).is_ok_and(|info| info.file_type().is_file());
        let unsafe_source = || format!("release version source is missing or unsafe: {relative}");
        if !regular {
            return Err(unsafe_source());
        }
        fs::read_to_string(&path).map_err(|_| unsafe_source())
    };
    let read_json = |relative: &str| -> Result<Value, String> {
        serde_json::from_str(&read(relative)?).map_err(|error| format!("release JSON is invalid: {relative}: {error}"))
    };
    let config = read_json("release-please-config.json")?;
    let config = config.as_object().ok_or("Release Please configuration must be an object")?;
    let initial = config.get("initial-version").and_then(Value::as_str);
    if initial != Some("0.0.1") {
        return Err("initial release version policy is not 0.0.1".into());
    }
    if config.get("bump-minor-pre-major") != Some(&Value::Bool(true))
        || config.get("bump-patch-for-minor-pre-major") != Some(&Value::Bool(true))
    {
        return Err("pre-major release policy is incomplete".into());
    }
    let manifest = read_json(".release-please-manifest.json")?;
    let manifest_version = root_entry(&manifest)?.as_str().filter(|version| Version::parse(version).is_some());
    let Some(manifest_version) = manifest_version else {
        return Err("manifest version is not canonical SemVer".into());
    };
    let source = read("version.txt")?;
    let source_version = source.strip_suffix('\n').filter(|version| Version::parse(version).is_some());
    let Some(source_version) = source_version else {
        return Err("version.txt is not one canonical SemVer line".into());
    };
    if makefile_versions(&read("Makefile")?) != [source_version] {
        return Err("Makefile version does not match version.txt".into());
    }
    if flake_versions(&read("flake.nix")?) != [source_version] {
        return Err("flake.nix version does not match version.txt".into());
    }
    if manifest_version == "0.0.0" {
        if Some(source_version) != initial {
            return Err("bootstrap source version does not match initial-version".into());
        }
    } else if source_version != manifest_version {
        return Err("released source version does not match the manifest".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makefile_versions_follow_the_assignment_grammar() {
        let text = "# x-release-please-start-version\nVERSION    ?= 0.1.2\n# x-release-please-end\n";
        assert_eq!(makefile_versions(text), ["0.1.2"]);
        assert_eq!(makefile_versions("VERSION ?= 0.1.0 \t\nVERSION\t?=\t0.2.0"), ["0.1.0", "0.2.0"]);
        for rejected in [
            "VERSION?= 0.1.0",
            "VERSION ?=0.1.0",
            "VERSION := 0.1.0",
            " VERSION ?= 0.1.0",
            "VERSION ?= 0.1.0 # note",
            "VERSION ?= 0.1.0\r",
            "XVERSION ?= 1",
        ] {
            assert!(makefile_versions(rejected).is_empty(), "{rejected:?}");
        }
    }

    #[test]
    fn flake_versions_follow_python_findall() {
        assert_eq!(flake_versions("      hamnVersion = \"0.1.2\"; # x-release-please-version\n"), ["0.1.2"]);
        assert_eq!(flake_versions("hamnVersion = \"0.1.0\"; # x-release-please-version"), ["0.1.0"]);
        // `\s*` spans lines on both sides of the marker.
        assert_eq!(
            flake_versions("\n\n  hamnVersion = \"1.0.0\";\n  # x-release-please-version\n\n  other\n"),
            ["1.0.0"]
        );
        assert_eq!(
            flake_versions(
                "a = 1;\nhamnVersion = \"0.1.0\"; # x-release-please-version\nhamnVersion = \"0.2.0\"; # x-release-please-version\n"
            ),
            ["0.1.0", "0.2.0"]
        );
        for rejected in [
            "hamnVersion = \"0.1.0\";",
            "hamnVersion = \"\"; # x-release-please-version",
            "x hamnVersion = \"0.1.0\"; # x-release-please-version",
            "hamnVersion = \"0.1.0\"; # x-release-please-version trailing",
            "hamnVersion  = \"0.1.0\"; # x-release-please-version",
        ] {
            assert!(flake_versions(rejected).is_empty(), "{rejected:?}");
        }
        assert_eq!(flake_versions("hamnVersion = \"0.1.0\"; # x-release-please-version \t\n"), ["0.1.0"]);
    }
}
