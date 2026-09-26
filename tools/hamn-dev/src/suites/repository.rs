//! Source-tree contracts that need no build: test tooling comes only from the
//! flake's Nix shells, tracked shell scripts and workflow YAML parse, removed
//! Desktop sources stay removed, nested virtualization stays capability-gated,
//! and the working tree has no whitespace errors. Run from the repository root.
use crate::runner::{self, case};
use std::fs;
use std::path::Path;
use std::process::{Command, ExitCode};

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "repository",
        "CLI-only repository contracts",
        vec![
            case("test_tools_come_only_from_nix_shells", test_tools_come_only_from_nix_shells),
            case("tracked_shell_scripts_parse", tracked_shell_scripts_parse),
            case("workflow_yaml_parses", workflow_yaml_parses),
            case("desktop_sources_stay_removed", desktop_sources_stay_removed),
            case("nested_virtualization_is_capability_gated", nested_virtualization_is_capability_gated),
            case("working_tree_has_no_whitespace_errors", working_tree_has_no_whitespace_errors),
        ],
        filters,
    )
}

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("{path}: {error} (run from the repository root)"))
}

fn tracked(pattern: &str) -> Vec<String> {
    let output = Command::new("git").args(["ls-files", "--", pattern]).output().expect("git ls-files");
    assert!(output.status.success(), "git ls-files {pattern}: {output:?}");
    let mut files: Vec<String> = String::from_utf8(output.stdout).unwrap().lines().map(str::to_owned).collect();
    files.sort();
    files
}

/// Whether `word` occurs in `text` delimited by non-identifier characters,
/// like the grep -E `(^|[^[:alnum:]_])word([^[:alnum:]_]|$)` it replaces.
fn has_word(text: &str, word: &str) -> bool {
    let identifier = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.match_indices(word).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + word.len()..].chars().next();
        !before.is_some_and(identifier) && !after.is_some_and(identifier)
    })
}

fn test_tools_come_only_from_nix_shells() {
    for removed in ["scripts/ci/setup-test-dependencies.sh", "scripts/ci/use-system-macos-sdk.sh"] {
        assert!(!Path::new(removed).exists(), "non-Nix test environment setup remains: {removed}");
    }
    let flake = read("flake.nix");
    for word in ["brew", "apt-get", "rustup", "swift", "xcodebuild"] {
        assert!(!has_word(&flake, word), "Nix test shells depend on {word}");
    }
}

fn tracked_shell_scripts_parse() {
    let scripts = tracked("*.sh");
    assert!(!scripts.is_empty());
    for script in scripts.iter().filter(|script| Path::new(script).is_file()) {
        let output = Command::new("bash").arg("-n").arg(script).output().expect("bash -n");
        assert!(output.status.success(), "{script}: {}", String::from_utf8_lossy(&output.stderr));
    }
}

fn workflow_yaml_parses() {
    let mut files = tracked(".github/workflows/*.yml");
    assert!(!files.is_empty());
    files.push(".github/actionlint.yaml".into());
    for file in files {
        serde_yaml::from_str::<serde_yaml::Value>(&read(&file)).unwrap_or_else(|error| panic!("{file}: {error}"));
    }
}

fn desktop_sources_stay_removed() {
    assert!(tracked("desktop/*").is_empty(), "tracked Desktop source remains");
    for removed in ["packaging/homebrew", "packaging/release/verify-macos-release.sh", "tests/ci/test_desktop_xcode.sh"] {
        assert!(!Path::new(removed).exists(), "removed Desktop asset remains: {removed}");
    }
}

/// Apple documents `isNestedVirtualizationSupported` as available only on
/// Macs with an M3 chip or later. The capability query, not model-name
/// parsing, guards future supported hardware; both documentation languages
/// state the boundary.
fn nested_virtualization_is_capability_gated() {
    let config = read("host/vz/vz_config.m");
    for (text, meaning) in [
        ("@available(macOS 15.0, *)", "macOS 15 availability guard"),
        ("isNestedVirtualizationSupported", "Apple nested-virtualization capability guard"),
        ("nestedVirtualizationEnabled = YES", "enabling supported nested virtualization"),
        ("M3 or later", "an unsupported-hardware error naming the M3 boundary"),
    ] {
        assert!(config.contains(text), "host/vz/vz_config.m lacks {meaning}");
    }
    for (file, text) in [
        ("docs/ARCHITECTURE.md", "M3 chip or"),
        ("docs/ARCHITECTURE.ko.md", "M3 칩 이상"),
        ("docs/CONFIGURATION.md", "M3-or-later Mac"),
        ("docs/CONFIGURATION.ko.md", "M3 칩 이상 Mac"),
    ] {
        assert!(read(file).contains(text), "{file} omits the M3 boundary");
    }
}

fn working_tree_has_no_whitespace_errors() {
    let output = Command::new("git").args(["diff", "--check"]).output().expect("git diff --check");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
}

#[cfg(test)]
mod tests {
    #[test]
    fn words_are_delimited_by_non_identifier_characters() {
        assert!(super::has_word("pkgs.brew", "brew"));
        assert!(super::has_word("brew", "brew"));
        assert!(super::has_word("[ rustup ]", "rustup"));
        assert!(!super::has_word("homebrew", "brew"));
        assert!(!super::has_word("brew_tap", "brew"));
        assert!(!super::has_word("swiftly", "swift"));
    }
}
