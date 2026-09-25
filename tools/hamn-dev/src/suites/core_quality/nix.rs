//! The locked flake: pinned inputs, its shells and checks, the Darwin
//! shells' ownership of the system Apple SDK, and (inside `nix develop` on
//! Darwin) the toolchain those shells actually resolve.
use super::is_regular_file;
use super::search::{contains, lines, read};
use crate::support::exec::{output_within, which};
use regex::Regex;
use serde_json::{Value, json};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

fn json(path: &str, meaning: &str) -> Value {
    serde_json::from_str(&read(path)).unwrap_or_else(|error| panic!("{meaning}: {path}: {error}"))
}

fn is_revision(value: &Value) -> bool {
    value.as_str().is_some_and(|rev| Regex::new("^[0-9a-f]{40}$").unwrap().is_match(rev))
}

fn is_nar_hash(value: &Value) -> bool {
    value.as_str().is_some_and(|hash| hash.starts_with("sha256-"))
}

pub fn nix_flake_pins_inputs_and_integrations() {
    for file in ["flake.nix", "flake.lock"] {
        assert!(is_regular_file(file), "Nix source is missing or unsafe: {file}");
    }
    let lock = json("flake.lock", "flake.lock does not pin nixpkgs");
    let nodes = &lock["nodes"];
    assert!(
        lock["version"] == 7
            && lock["root"] == "root"
            && nodes["root"]["inputs"]["nixpkgs"] == "nixpkgs"
            && is_revision(&nodes["nixpkgs"]["locked"]["rev"])
            && is_nar_hash(&nodes["nixpkgs"]["locked"]["narHash"]),
        "flake.lock does not pin nixpkgs"
    );
    // The Rust toolchain comes from the pinned overlay, evaluated with the same nixpkgs.
    let overlay = &nodes["rust-overlay"];
    assert!(
        nodes["root"]["inputs"]["rust-overlay"] == "rust-overlay"
            && overlay["inputs"]["nixpkgs"] == json!(["nixpkgs"])
            && overlay["locked"]["owner"] == "oxalica"
            && overlay["locked"]["repo"] == "rust-overlay"
            && is_revision(&overlay["locked"]["rev"])
            && is_nar_hash(&overlay["locked"]["narHash"]),
        "flake.lock does not pin the Rust toolchain overlay"
    );
    for requirement in [
        r#""aarch64-darwin""#,
        r#""x86_64-darwin""#,
        r#""aarch64-linux""#,
        r#""x86_64-linux""#,
        "devShells = forAllSystems",
        "            pkgs.mkShellNoCC {",
        "          ci = shellWith [ ];",
        "          live = shellWith (with pkgs; [",
        "          release = shellWith (with pkgs; [",
        "            kind",
        "            kubectl",
        "        docker-client # includes the Compose and buildx CLI plugins",
        "checks = forAllSystems",
        r#"      actionlintVersion = "1.7.12";"#,
        r#"inputs.nixpkgs.follows = "nixpkgs";"#,
        "(rust-overlay.lib.mkRustBin { } rustPkgs).fromRustupToolchainFile ./rust-toolchain.toml;",
        "actionlint -config-file",
    ] {
        assert!(contains("flake.nix", requirement), "Nix flake is missing required integration: {requirement}");
    }
}

pub fn release_please_configuration_is_valid() {
    let config = json("release-please-config.json", "Release Please configuration is invalid");
    assert!(
        config["release-type"] == "simple"
            && config["initial-version"] == "0.0.1"
            && config["skip-github-release"] == true
            && config["bump-minor-pre-major"] == true
            && config["bump-patch-for-minor-pre-major"] == true
            && config["packages"]["."]["package-name"] == "hamn",
        "Release Please configuration is invalid"
    );
    assert!(
        contains("Makefile", "x-release-please-start-version"),
        "Release Please does not update the Makefile version"
    );
    assert!(
        contains("flake.nix", "x-release-please-version"),
        "Release Please does not update the Nix tooling version"
    );
}

/// The Darwin shells, not per-workflow scripts, own the Apple SDK boundary.
pub fn darwin_shells_own_the_system_sdk_boundary() {
    for requirement in [
        "/usr/bin/xcrun --sdk macosx --show-sdk-path) || HAMN_SYSTEM_SDKROOT=",
        "/nix/store/*)",
        "FAIL: Hamn must not compile against the Nix Apple SDK",
        "export SDKROOT=$HAMN_SYSTEM_SDKROOT HAMN_SYSTEM_SDKROOT",
        "export PATH=${pkgs.lib.makeBinPath packages}:/usr/bin:/bin:/usr/sbin:/sbin:$PATH",
        r#"(tool: { name = "bin/${tool}"; path = "/usr/bin/${tool}"; })"#,
        r#"[ "ar" "c++" "cc" "clang" "clang++" "codesign" "ld" "otool" "ranlib" "xcrun" ]);"#,
        "shellHook = lib.optionalString stdenv.isDarwin (darwinShellHook pkgs packages);",
    ] {
        assert!(contains("flake.nix", requirement), "system macOS SDK boundary is incomplete: {requirement}");
    }
}

fn nonempty_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

fn shown(path: &Option<PathBuf>) -> String {
    path.as_ref().map_or_else(String::new, |path| path.display().to_string())
}

/// The immediate target of a symbolic link, or the path itself, like
/// `readlink PATH || printf '%s' PATH`.
fn link_or_self(path: &Path) -> PathBuf {
    fs::read_link(path).unwrap_or_else(|_| path.to_owned())
}

/// Inside the flake shell on Darwin, observes the resolved toolchain rather
/// than the flake's source text; elsewhere, macOS CI must not run outside it.
pub fn nix_shell_resolves_the_pinned_toolchain() {
    let darwin = cfg!(target_os = "macos");
    if !(darwin && nonempty_env("IN_NIX_SHELL").is_some()) {
        assert!(
            !(darwin && nonempty_env("GITHUB_ACTIONS").is_some()),
            "macOS CI gates must run inside the flake shell (nix develop .#ci)"
        );
        eprintln!("NOTE: Nix shell toolchain observation skipped outside nix develop on Darwin");
        return;
    }
    for tool in ["ar", "cc", "clang", "codesign", "otool", "xcrun"] {
        let resolved = which(tool).unwrap_or_else(|| panic!("Apple {tool} is unavailable in the Nix shell"));
        assert!(
            link_or_self(&resolved) == Path::new("/usr/bin").join(tool),
            "{tool} must resolve to Apple's /usr/bin/{tool}, not {}",
            resolved.display()
        );
    }
    // The tools the repository's gates run from the pinned shell.
    for tool in ["bash", "cargo", "git", "jq", "make", "rustc"] {
        let resolved = which(tool);
        assert!(
            resolved.as_ref().and_then(|path| path.to_str()).is_some_and(|path| path.starts_with("/nix/store/")),
            "{tool} must come from the pinned Nix shell, not {}",
            shown(&resolved)
        );
    }
    for tool in ["awk", "find", "grep", "sed", "stat", "tar", "xargs"] {
        let resolved = which(tool);
        assert!(
            resolved
                .as_ref()
                .and_then(|path| path.to_str())
                .is_some_and(|path| path.starts_with("/usr/bin/") || path.starts_with("/bin/")),
            "{tool} must be the macOS userland tool, not {}",
            shown(&resolved)
        );
    }
    let sdk = nonempty_env("SDKROOT");
    assert!(
        sdk.as_ref().is_some_and(|sdk| {
            env::var("HAMN_SYSTEM_SDKROOT").is_ok_and(|system| &system == sdk)
                && Path::new(sdk).is_dir()
                && !sdk.starts_with("/nix/store/")
        }),
        "Nix shell must select the system macOS SDK: {}",
        sdk.as_deref().unwrap_or("unset")
    );
    // The Darwin toolchain carries no Nix C compiler: nixpkgs' cc-wrapper
    // setup hook would export NIX_CC and put a Nix clang or ld on PATH.
    let nix_cc = nonempty_env("NIX_CC");
    assert!(nix_cc.is_none(), "the Darwin shell must not carry a Nix C compiler: {}", nix_cc.unwrap_or_default());
    let path = env::var("PATH").unwrap_or_default();
    for directory in path.split(':').filter(|directory| directory.starts_with("/nix/store/")) {
        for tool in ["cc", "clang", "ld"] {
            let candidate = Path::new(directory).join(tool);
            if !candidate.exists() {
                continue;
            }
            assert!(
                fs::read_link(&candidate).is_ok_and(|target| target == Path::new("/usr/bin").join(tool)),
                "a Nix {tool} is on the shell PATH: {}",
                candidate.display()
            );
        }
    }
    let channel_line = Regex::new(r#"^channel = "([0-9.]+)"$"#).unwrap();
    let toolchain = read("rust-toolchain.toml");
    let channels: Vec<&str> = lines(&toolchain)
        .into_iter()
        .filter_map(|line| channel_line.captures(line).map(|captures| captures.get(1).unwrap().as_str()))
        .collect();
    assert!(!channels.is_empty(), "rust-toolchain.toml does not pin a stable channel");
    let channel = channels.join("\n");
    let rustc = output_within(Command::new("rustc").arg("--version"), Duration::from_secs(30));
    let version = String::from_utf8_lossy(&rustc.stdout);
    assert!(
        version.starts_with(&format!("rustc {channel} ")),
        "Nix Rust toolchain does not match rust-toolchain.toml: {}",
        version.trim_end()
    );
}
