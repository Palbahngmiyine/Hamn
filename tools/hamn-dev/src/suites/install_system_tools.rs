//! Install and upgrade in an isolated HOME with only macOS system commands
//! and the shipped `hamn` executable. A sandbox profile denies every other
//! executable (developer tools, interpreters, package managers) to the
//! installer and updater; the test itself only prepares fixtures outside it.
use crate::runner::{self, case};
use crate::support::tmp::TempDir;
use crate::support::upgrade::{self, Artifact, digest, file_digest, pack_release, release_payload, write_json};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "install-system-tools",
        "bootstrap, reinstall, native cleanup, unchanged upgrade and rejection with only OS tools + hamn; interpreters and developer tools denied",
        vec![case("install_and_upgrade_need_only_system_tools", install_and_upgrade_need_only_system_tools)],
        filters,
    )
}

/// The only commands the installer and updater may execute, besides the
/// release's own `hamn`: the stock shells and tools of install.sh and its
/// zsh acquisition program, and the updater's curl, sw_vers and lsof.
const TOOLS: &[&str] = &[
    "bash", "zsh", "env", "curl", "openssl", "tar", "bsdtar", "chmod", "mkdir", "rm", "mv", "cp", "sync", "cat", "stat",
    "mktemp", "id", "awk", "sw_vers", "lsof",
];
const SYSTEM_DIRECTORIES: [&str; 4] = ["/bin", "/usr/bin", "/sbin", "/usr/sbin"];

/// A JSON string literal as the receipt contract encodes names: compact,
/// with every non-ASCII character escaped as lowercase UTF-16 `\uXXXX` units.
fn ascii_json_string(text: &str) -> String {
    let mut result = String::from("\"");
    for character in text.chars() {
        match character {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            '\u{8}' => result.push_str("\\b"),
            '\u{c}' => result.push_str("\\f"),
            c if (c as u32) < 0x20 || !c.is_ascii() => {
                for unit in c.encode_utf16(&mut [0; 2]) {
                    result.push_str(&format!("\\u{unit:04x}"));
                }
            }
            c => result.push(c),
        }
    }
    result.push('"');
    result
}

/// The receipt's installed-tree digest, derived independently of Hamn from
/// the documented contract: depth-first `[name, mode, sha256-or-null]`
/// entries for the generation's bin and share trees with sorted children.
fn receipt_tree_digest(generation: &Path) -> String {
    fn visit(path: &Path, name: &str, entries: &mut Vec<String>) {
        let info = fs::symlink_metadata(path).unwrap();
        let content = if info.is_dir() { "null".to_owned() } else { format!("\"{}\"", file_digest(path)) };
        entries.push(format!("[{},{},{content}]", ascii_json_string(name), info.mode() & 0o7777));
        if info.is_dir() {
            let mut children: Vec<PathBuf> = fs::read_dir(path).unwrap().map(|entry| entry.unwrap().path()).collect();
            children.sort();
            for child in children {
                let leaf = child.file_name().unwrap().to_str().unwrap().to_owned();
                visit(&child, &format!("{name}/{leaf}"), entries);
            }
        }
    }
    let mut entries = Vec::new();
    for name in ["bin", "share"] {
        visit(&generation.join(name), name, &mut entries);
    }
    digest(format!("[{}]", entries.join(",")).as_bytes())
}

/// `^` + `text` as a literal regular expression.
fn literal_regex(text: &str) -> String {
    text.chars().map(|c| if c.is_ascii_alphanumeric() || c == '/' || c == '_' { c.to_string() } else { format!("\\{c}") }).collect()
}

fn install_and_upgrade_need_only_system_tools() {
    let hamn = crate::support::hamn();
    let directory = TempDir::new_in(&std::env::temp_dir(), "hamn-system-install-");
    let work = fs::canonicalize(directory.path()).unwrap();
    let (home, scratch) = (work.join("home"), work.join("tmp"));
    fs::create_dir(&home).unwrap();
    fs::create_dir(&scratch).unwrap();

    let release = work.join("release");
    release_payload(&release, &hamn, "https://example.invalid/manifest");
    let archive = work.join("host.tar.gz");
    pack_release(&release, &archive);
    let guest = work.join("guest.img");
    fs::write(&guest, "isolated managed image fixture\n").unwrap();
    let version = upgrade::run(Command::new(&hamn).arg("--version"), Duration::from_secs(10)).stdout();
    let version = version.trim().strip_prefix("hamn ").unwrap().to_owned();
    let (host_artifact, guest_artifact) = (Artifact::local(&archive), Artifact::local(&guest));
    let mut template = fs::read_to_string(upgrade::checkout().join("packaging/release/install.sh.in")).unwrap();
    for (key, value) in [
        ("VERSION", format!("v{version}")),
        ("COMMIT", "a".repeat(40)),
        ("HOST_URL", host_artifact.url.clone()),
        ("HOST_SHA256", host_artifact.sha256.clone()),
        ("HOST_SIZE", host_artifact.size.to_string()),
        ("GUEST_URL", guest_artifact.url.clone()),
        ("GUEST_SHA256", guest_artifact.sha256.clone()),
        ("GUEST_SIZE", guest_artifact.size.to_string()),
    ] {
        // Every value is shell-safe as written (a word of only these
        // characters needs no quoting).
        assert!(value.chars().all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c)), "{value}");
        template = template.replace(&format!("__HAMN_{key}__"), &value);
    }
    let installer = work.join("install.sh");
    fs::write(&installer, template).unwrap();

    // Enumerate only immutable OS executable locations; do not permit
    // /usr/bin as a whole (python3 and xcrun there can be developer-tool
    // installation shims).
    let mut allowed = BTreeSet::from([hamn.to_str().unwrap().to_owned()]);
    for tool in TOOLS {
        let mut found = false;
        for parent in SYSTEM_DIRECTORIES {
            let candidate = Path::new(parent).join(tool);
            if candidate.exists() {
                let resolved = fs::canonicalize(&candidate).unwrap();
                assert!(SYSTEM_DIRECTORIES.iter().any(|directory| resolved.starts_with(directory)), "{}", resolved.display());
                let info = fs::metadata(&resolved).unwrap();
                assert!(info.uid() == 0 && info.mode() & 0o022 == 0, "{}", resolved.display());
                allowed.insert(candidate.to_str().unwrap().to_owned());
                allowed.insert(resolved.to_str().unwrap().to_owned());
                found = true;
            }
        }
        assert!(found, "missing OS command: {tool}");
    }
    let quote = |text: &str| serde_json::to_string(text).unwrap();
    let work_text = work.to_str().unwrap();
    let mut profile_text = String::from(
        "(version 1)\n(allow default)\n(deny process-exec)\n\
         (deny file-read* (subpath \"/Library/Developer\") (subpath \"/Applications/Xcode.app\"))\n(allow process-exec\n",
    );
    for path in &allowed {
        profile_text.push_str(&format!(" (literal {})\n", quote(path)));
    }
    profile_text.push_str(&format!(" (regex {}))\n", quote(&format!("^{}/.*/(bin/hamn|hamn-support)$", literal_regex(work_text)))));
    let profile = work.join("system-only.sb");
    fs::write(&profile, profile_text).unwrap();
    let sandboxed = |program: &Path, args: &[&str], success: bool| {
        let mut command = Command::new("/usr/bin/sandbox-exec");
        command
            .arg("-f")
            .arg(&profile)
            .arg(program)
            .args(args)
            .env("LC_ALL", "C")
            .env("HOME", &home)
            .env("TMPDIR", &scratch)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HAMN_INSTALL_ALLOW_LOCAL_ARTIFACTS", "1")
            .env("HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS", "1");
        let result = upgrade::run(&mut command, Duration::from_secs(90));
        if success {
            assert_eq!(result.returncode, 0, "{program:?} {args:?}: {} {}", result.stdout(), result.stderr());
        }
        result
    };

    // Prove the enforcement boundary before claiming a dependency-free install.
    for forbidden in ["/usr/bin/python3", "/usr/bin/perl", "/usr/bin/ruby", "/usr/bin/xcrun", "/usr/bin/git"] {
        let result = sandboxed(Path::new(forbidden), &["--version"], false);
        assert_ne!(result.returncode, 0, "external/developer runtime was permitted: {forbidden}");
    }
    sandboxed(Path::new("/usr/bin/curl"), &["--version"], true);
    let command = home.join(".local/bin/hamn");
    for attempt in 0..3 {
        if attempt > 0 {
            let active = fs::canonicalize(&command).unwrap();
            fs::remove_file(active.parent().and_then(Path::parent).unwrap().join(".hamn-release.json")).unwrap();
        }
        sandboxed(Path::new("/bin/bash"), &[installer.to_str().unwrap()], true);
        assert_eq!(file_digest(&fs::canonicalize(&command).unwrap()), file_digest(&hamn));
    }
    let generations = fs::read_dir(home.join(".local/share/hamn/src/.hamn-generations"))
        .unwrap()
        .filter(|entry| entry.as_ref().unwrap().path().join(".hamn-generation").exists())
        .count();
    assert_eq!(generations, 2, "cleanup was skipped or lost retention during system-only install");
    let active = fs::canonicalize(&command).unwrap();
    let selection = home.join(".hamn/cache/guest-image.json");
    let before = fs::read(&selection).unwrap();

    // Rewrite the receipt with the independently derived tree digest: the
    // native updater must accept it without reinstalling (a no-op). A
    // non-ASCII name exercises the receipt's escaping contract.
    let generation = active.parent().and_then(Path::parent).unwrap();
    fs::write(generation.join("share/hamn/기록.txt"), "receipt Unicode fixture\n").unwrap();
    let receipt_path = generation.join(".hamn-release.json");
    let mut receipt: Value = serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
    receipt["installedSHA256"] = receipt_tree_digest(generation).into();
    write_json(&receipt_path, &receipt);
    let manifest = work.join("manifest.json");
    fs::write(&manifest, upgrade::manifest(&format!("v{version}"), &host_artifact, &guest_artifact).to_string()).unwrap();
    let upgrade_args = ["--headless", "system", "upgrade", "--yes", "--manifest", manifest.to_str().unwrap()];
    let result = sandboxed(&command, &upgrade_args, true);
    assert!(result.stderr().ends_with(" is up to date.\n"), "{}", result.stderr());
    assert_eq!(fs::canonicalize(&command).unwrap(), active);
    assert_eq!(fs::read(&selection).unwrap(), before);
    let duplicated = fs::read_to_string(&manifest).unwrap().replacen("\"schemaVersion\":3", "\"schemaVersion\":3,\"schemaVersion\":3", 1);
    assert_ne!(duplicated, fs::read_to_string(&manifest).unwrap());
    fs::write(&manifest, duplicated).unwrap();
    let result = sandboxed(&command, &upgrade_args, false);
    assert_ne!(result.returncode, 0);
    assert_eq!(fs::canonicalize(&command).unwrap(), active);
    assert_eq!(fs::read(&selection).unwrap(), before);
    assert!(!home.join(".hamn/profiles").exists());
}
