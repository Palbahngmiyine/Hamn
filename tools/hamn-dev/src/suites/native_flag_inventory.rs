//! Compares the TUI's routing arity (control/native_flags.rs) with every
//! public command and global flag of the installed kubectl: the inventory
//! of `kubectl help` is handed to an ignored product test through
//! `HAMN_KUBECTL_FLAG_INVENTORY`.
use crate::runner::{self, case};
use crate::support::exec::{output_within, which};
use crate::support::tmp::TempDir;
use serde_json::{Map, Value};
use std::collections::VecDeque;
use std::path::Path;
use std::process::{Command, ExitCode, Output};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    let Some(kubectl) = which("kubectl") else {
        println!("SKIP: installed kubectl unavailable for public flag inventory");
        return ExitCode::SUCCESS;
    };
    runner::run(
        "native-flag-inventory",
        "installed kubectl public arities and flag-like values match the routing inventory",
        vec![case("installed_kubectl_flag_inventory", move || installed_kubectl_flag_inventory(&kubectl))],
        filters,
    )
}

fn installed_kubectl_flag_inventory(kubectl: &Path) {
    let directory = TempDir::new("hamn-kubectl-flags-");
    let root = directory.path();
    let kubectl_command = |args: &[&str]| {
        let mut command = Command::new(kubectl);
        command.args(args).env("HOME", root).env("KUBECONFIG", root.join("absent")).env("LANG", "C").env("LC_ALL", "C");
        output_within(&mut command, Duration::from_secs(10))
    };
    let text = |output: &Output| String::from_utf8(output.stdout.clone()).expect("UTF-8 kubectl output");
    let mut inventory: Map<String, Value> = Map::new();
    let mut queue: VecDeque<Vec<String>> = VecDeque::from([Vec::new()]);
    while let Some(path) = queue.pop_front() {
        let key = if path.is_empty() { "root".to_owned() } else { path.join(" ") };
        if inventory.contains_key(&key) {
            continue;
        }
        assert!(inventory.len() < 512, "unexpected command inventory growth");
        let mut args = vec!["help", "--"];
        args.extend(path.iter().map(String::as_str));
        let result = kubectl_command(&args);
        assert!(result.status.success(), "{path:?} {}", String::from_utf8_lossy(&result.stderr));
        let mut flags = Map::new();
        let mut section = String::new();
        for line in splitlines(&text(&result)) {
            if line.chars().next().is_some_and(|first| !python_space(first)) && line.ends_with(':') {
                section = line.to_owned();
            }
            if section.contains("Commands") || section == "Subcommands:" {
                if let Some(child) = subcommand(line) {
                    let mut child_path = path.clone();
                    child_path.push(child.to_owned());
                    queue.push_back(child_path);
                }
            }
            if let Some(option) = option(line) {
                // Kubectl's public help prints bool defaults without quotes.
                // These three string flags also have NoOptDefVal: see the
                // github.com/kubernetes/kubectl/blob/master/pkg/cmd/util/helpers.go
                // github.com/kubernetes/kubectl/blob/master/pkg/cmd/delete/delete_flags.go
                let consumes =
                    !matches!(option.default, "true" | "false") && !matches!(option.name, "cascade" | "dry-run" | "validate");
                flags.insert(format!("--{}", option.name), Value::Bool(consumes));
                if let Some(short) = option.short {
                    flags.insert(format!("-{short}"), Value::Bool(consumes));
                }
            }
        }
        inventory.insert(key, Value::Object(flags));
    }
    let result = kubectl_command(&["version", "--client", "-o", "json"]);
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let version_output: Value = serde_json::from_str(&text(&result)).unwrap();
    let version = version_output["clientVersion"]["gitVersion"].as_str().expect("clientVersion.gitVersion").to_owned();
    // `help options` suppresses the body; obtain inherited flags explicitly.
    let result = kubectl_command(&["options"]);
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let mut global = Map::new();
    // re.findall(..., re.MULTILINE): lines end only at '\n'.
    for line in text(&result).split('\n') {
        if let Some(option) = option(line) {
            let consumes = !matches!(option.default, "true" | "false");
            global.insert(format!("--{}", option.name), Value::Bool(consumes));
            if let Some(short) = option.short {
                global.insert(format!("-{short}"), Value::Bool(consumes));
            }
        }
    }
    inventory.insert("global".to_owned(), Value::Object(global));
    assert!(inventory.len() > 50);
    assert_eq!(inventory["create configmap"]["--from-literal"], true);
    assert_eq!(inventory["logs"]["-f"], false);
    assert_eq!(inventory["get"]["-f"], true);
    let fixture = root.join("inventory.json");
    std::fs::write(&fixture, Value::Object(inventory.clone()).to_string()).unwrap();
    let result = output_within(
        Command::new("cargo")
            .args(["test", "--locked", "native_flags::tests::installed_kubectl_flag_inventory", "--", "--ignored", "--exact"])
            .env("HAMN_KUBECTL_FLAG_INVENTORY", &fixture),
        Duration::from_secs(120),
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(result.status.success() && stdout.contains("1 passed"), "{stdout}{}", String::from_utf8_lossy(&result.stderr));
    println!("{version} public arities and flag-like values across {} command/global entries", inventory.len());
}

/// Python's `str.isspace` for one character: Unicode White_Space plus the
/// information separators U+001C..U+001F.
fn python_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python's `str.splitlines()`: lines end at \n, \r, \r\n, \v, \f,
/// U+001C..U+001E, U+0085, U+2028 and U+2029, which are not kept.
fn splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        let breaks =
            matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}');
        if !breaks {
            continue;
        }
        lines.push(&text[start..index]);
        start = index + c.len_utf8();
        if c == '\r' && chars.peek().is_some_and(|&(_, next)| next == '\n') {
            chars.next();
            start += 1;
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_lowercase()
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'
}

/// `NAME` from `^  ([a-z][a-z0-9-]*)  +\S`: two spaces, a command name, at
/// least two spaces, then a non-space character.
fn subcommand(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("  ")?;
    if !rest.starts_with(is_name_start) {
        return None;
    }
    let end = rest.find(|c: char| !is_name_char(c)).unwrap_or(rest.len());
    let (name, after) = rest.split_at(end);
    let spaces = after.len() - after.trim_start_matches(' ').len();
    let next = after[spaces..].chars().next()?;
    (spaces >= 2 && !python_space(next)).then_some(name)
}

struct OptionLine<'a> {
    short: Option<char>,
    name: &'a str,
    default: &'a str,
}

/// `^    (?:-([^, ]), )?--([a-z][a-z0-9-]*)=(.*):$` on one line: four
/// spaces, an optional `-X, ` short form, `--NAME=DEFAULT` and a final `:`.
fn option(line: &str) -> Option<OptionLine<'_>> {
    let rest = line.strip_prefix("    ")?;
    let mut chars = rest.chars();
    if let (Some('-'), Some(short)) = (chars.next(), chars.next())
        && short != ','
        && short != ' '
        && let Some(after) = chars.as_str().strip_prefix(", ")
        && let Some((name, default)) = long_option(after)
    {
        return Some(OptionLine { short: Some(short), name, default });
    }
    let (name, default) = long_option(rest)?;
    Some(OptionLine { short: None, name, default })
}

/// `--([a-z][a-z0-9-]*)=(.*):$`: the name and default of a long option.
fn long_option(rest: &str) -> Option<(&str, &str)> {
    let rest = rest.strip_prefix("--")?;
    if !rest.starts_with(is_name_start) {
        return None;
    }
    let end = rest.find(|c: char| !is_name_char(c)).unwrap_or(rest.len());
    let (name, after) = rest.split_at(end);
    // `.` matches anything but a newline; the default is greedy, so it ends
    // at the line's final ':'.
    let default = after.strip_prefix('=')?.strip_suffix(':')?;
    (!default.contains('\n')).then_some((name, default))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_lines_parse_like_the_former_regular_expressions() {
        assert_eq!(subcommand("  create        Create a resource"), Some("create"));
        assert_eq!(subcommand("  set-image  x"), Some("set-image"));
        for line in ["  create Create", "   create  x", "  Create  x", "  create  ", "  create  \tx", "  create_x  y"] {
            assert_eq!(subcommand(line), None, "{line:?}");
        }
        let parsed = option("    -n, --namespace='': If present").map(|o| (o.short, o.name, o.default));
        assert_eq!(parsed, None, "the line must end with ':'");
        let parsed = option("    -f, --filename=[]:").map(|o| (o.short, o.name, o.default));
        assert_eq!(parsed, Some((Some('f'), "filename", "[]")));
        let parsed = option("    --all-namespaces=false:").map(|o| (o.short, o.name, o.default));
        assert_eq!(parsed, Some((None, "all-namespaces", "false")));
        let parsed = option("    --template='a:b':").map(|o| (o.short, o.name, o.default));
        assert_eq!(parsed, Some((None, "template", "'a:b'")));
        let parsed = option("    --, --dash=1:").map(|o| (o.short, o.name, o.default));
        assert_eq!(parsed, Some((Some('-'), "dash", "1")));
        for line in [
            "   --all=false:",
            "    --All=false:",
            "    -ab, --x=1:",
            "    --x:",
            "     --x=1:",
            "    -,, --x=1:",
            "    ---, --dash=1:",
        ] {
            assert!(option(line).is_none(), "{line:?}");
        }
        assert_eq!(splitlines("a\r\nb\rc\nd\x0be\u{2028}f\n"), ["a", "b", "c", "d", "e", "f"]);
        assert_eq!(splitlines("a\n\nb"), ["a", "", "b"]);
    }
}
