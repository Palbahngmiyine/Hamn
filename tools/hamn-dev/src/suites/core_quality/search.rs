//! Line searches over the files that the ripgrep, git grep and grep calls
//! they replace would read, with those tools' line semantics: a pattern
//! (Rust `regex` syntax, which ripgrep shares) matches within one line and
//! never spans a newline, so `[[:space:]]` cannot match one and `^`/`$`
//! anchor at line boundaries. Paths are relative to the repository root,
//! the working directory of every suite.
use crate::support::exec::output_within;
use regex::bytes::Regex;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

const GIT_TIMEOUT: Duration = Duration::from_secs(60);
/// git grep -I treats a file as binary when this prefix holds a NUL byte.
const GIT_BINARY_PREFIX: usize = 8000;

pub fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("{path}: {error} (run from the repository root)"))
}

/// `grep -F TEXT PATH`: whether the file holds `text` (which has no newline).
pub fn contains(path: &str, text: &str) -> bool {
    read(path).contains(text)
}

/// The lines of `text` as grep and awk split them: on `\n` only (a `\r`
/// stays part of its line), with no empty line after a final newline.
pub fn lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    text.strip_suffix('\n').unwrap_or(text).split('\n').collect()
}

/// `grep -Fx LINE`: whether some line of `text` is exactly `line`.
pub fn has_line(text: &str, line: &str) -> bool {
    lines(text).contains(&line)
}

/// `grep -E PATTERN`: whether some line of `text` matches `pattern`.
pub fn any_line_matches(text: &str, pattern: &str) -> bool {
    let regex = regex(pattern);
    lines(text).iter().any(|line| regex.is_match(line.as_bytes()))
}

/// `rg PATTERN PATH...`, returning `path:line` for each matching line
/// (never the line itself, which may hold a credential). A named file is
/// searched whole, as ripgrep searches a named file even if it looks binary,
/// replacing NUL bytes with line breaks. A named directory holds the files
/// ripgrep's traversal visits: regular files that are tracked or untracked
/// but not ignored (`git ls-files --cached --others --exclude-standard`),
/// without hidden entries below the directory or symbolic links. Where
/// ripgrep skips a file whose first read holds a NUL byte, the content
/// before the first NUL byte is still searched. Unlike ripgrep, which
/// reports and skips a path it cannot read, a missing path fails the search,
/// so a renamed file or directory cannot silently leave a scan.
pub fn rg(pattern: &str, paths: &[&str]) -> Vec<String> {
    let regex = regex(pattern);
    let mut hits = Vec::new();
    for path in paths {
        let metadata =
            fs::metadata(path).unwrap_or_else(|error| panic!("{path}: {error} (run from the repository root)"));
        if !metadata.is_dir() {
            let content = fs::read(path).unwrap_or_else(|error| panic!("{path}: {error}"));
            hits.extend(matching_lines(&regex, path, &content, |byte| byte == b'\n' || byte == 0));
            continue;
        }
        for file in listed(&["--cached", "--others", "--exclude-standard"], &[path]) {
            let below = Path::new(&file).strip_prefix(path).unwrap_or_else(|_| panic!("{file} is not below {path}"));
            if below.iter().any(|component| component.as_encoded_bytes().starts_with(b".")) {
                continue;
            }
            let Some(content) = regular_file(&file) else { continue };
            let text = content.iter().position(|&byte| byte == 0).map_or(&content[..], |nul| &content[..nul]);
            hits.extend(matching_lines(&regex, &file, text, |byte| byte == b'\n'));
        }
    }
    hits
}

/// `if rg PATTERN PATH...; then fail MEANING`, naming the matching lines.
pub fn assert_absent(pattern: &str, paths: &[&str], meaning: &str) {
    let hits = rg(pattern, paths);
    assert!(hits.is_empty(), "{meaning}: {}", hits.join(", "));
}

/// `git grep -I -E PATTERN -- PATHSPEC...` in the working tree, returning
/// `path:line` for each matching line: tracked regular files selected by
/// git's own pathspec rules, skipping binary files (a NUL byte within the
/// first 8000 bytes, as git decides without attributes; this repository's
/// .gitattributes marks no file binary).
pub fn git_grep(pattern: &str, pathspecs: &[&str]) -> Vec<String> {
    let regex = regex(pattern);
    let mut hits = Vec::new();
    for file in listed(&["--cached"], pathspecs) {
        let Some(content) = regular_file(&file) else { continue };
        if content[..content.len().min(GIT_BINARY_PREFIX)].contains(&0) {
            continue;
        }
        hits.extend(matching_lines(&regex, &file, &content, |byte| byte == b'\n'));
    }
    hits
}

/// `git ls-files OPTIONS -- PATHSPEC...`, sorted and without duplicates
/// (an unmerged path is listed once per stage).
pub fn listed(options: &[&str], pathspecs: &[&str]) -> Vec<String> {
    let output = output_within(
        Command::new("git").arg("ls-files").arg("-z").args(options).arg("--").args(pathspecs),
        GIT_TIMEOUT,
    );
    assert!(output.status.success(), "git ls-files {pathspecs:?}: {}", String::from_utf8_lossy(&output.stderr));
    let text = String::from_utf8(output.stdout).expect("UTF-8 tracked paths");
    let mut files: Vec<String> = text.split_terminator('\0').map(str::to_owned).collect();
    files.sort();
    files.dedup();
    files
}

/// The content of `path` if it is a regular file (not a symbolic link, a
/// directory or a submodule); `None` if it is something else or was
/// deleted from the working tree.
fn regular_file(path: &str) -> Option<Vec<u8>> {
    let metadata = fs::symlink_metadata(path).ok()?;
    metadata.is_file().then(|| fs::read(path).unwrap_or_else(|error| panic!("{path}: {error}")))
}

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|error| panic!("pattern {pattern:?}: {error}"))
}

fn matching_lines(regex: &Regex, path: &str, content: &[u8], separator: impl Fn(u8) -> bool) -> Vec<String> {
    if content.is_empty() {
        return Vec::new();
    }
    let body = content.strip_suffix(b"\n").unwrap_or(content);
    body.split(|&byte| separator(byte))
        .enumerate()
        .filter(|(_, line)| regex.is_match(line))
        .map(|(index, _)| format!("{path}:{}", index + 1))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hits(pattern: &str, content: &[u8]) -> Vec<String> {
        matching_lines(&regex(pattern), "f", content, |byte| byte == b'\n')
    }

    #[test]
    fn patterns_match_within_one_line() {
        assert_eq!(hits(r"docker[[:space:]]+login", b"docker\nlogin\n"), Vec::<String>::new());
        assert_eq!(hits(r"docker[[:space:]]+login", b"x\ndocker \t login\n"), ["f:2"]);
        assert_eq!(hits(r#"-I(shared|guest/hamnd)([[:space:]"]|$)"#, b"-Ishared\n-Isharedx\n"), ["f:1"]);
        assert_eq!(hits(r"^[[:space:]]*make (host|install)[[:space:]]*$", b"  make host  \nmake hosts\n"), ["f:1"]);
        assert_eq!(hits(r"(?i)colima-benchmark", b"Colima-Benchmark\r\n"), ["f:1"]);
    }

    #[test]
    fn lines_follow_grep_splitting() {
        assert_eq!(lines(""), Vec::<&str>::new());
        assert_eq!(lines("a\n\nb"), ["a", "", "b"]);
        assert_eq!(lines("a\r\nb\n"), ["a\r", "b"]);
        assert!(has_line("  contents: read\n", "  contents: read"));
        assert!(!has_line("  contents: read\r\n", "  contents: read"));
        assert!(!has_line("   contents: read\n", "  contents: read"));
        assert!(any_line_matches("a\n  tags:\n", r"^[[:space:]]+tags:"));
        assert!(!any_line_matches("tags:\n", r"^[[:space:]]+tags:"));
    }
}
