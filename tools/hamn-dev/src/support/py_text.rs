//! Python-compatible text helpers, so a ported assertion keeps the meaning
//! of the Python expression it replaces (`str.split()`, `str.splitlines()`,
//! `shlex.join`, `json.dumps` of a string list, `urllib.parse`).
use std::collections::BTreeMap;

/// Python's `str.isspace` for one character: Unicode White_Space plus the
/// information separators U+001C..U+001F.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `text.split()`: whitespace-separated fields without empty ones.
pub fn split(text: &str) -> Vec<&str> {
    text.split(is_space).filter(|field| !field.is_empty()).collect()
}

/// `' '.join(text.split())`.
pub fn squash(text: &str) -> String {
    split(text).join(" ")
}

/// `text.strip()`.
pub fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// `text.splitlines()`: lines split at any Python line boundary, `\r\n`
/// counting as one; a final boundary does not add an empty line.
pub fn splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        let boundary =
            matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}');
        if !boundary {
            continue;
        }
        lines.push(&text[start..index]);
        start = index + c.len_utf8();
        if c == '\r' && chars.peek().map(|&(_, next)| next) == Some('\n') {
            chars.next();
            start += 1;
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// Output decoded the way `subprocess.run(..., text=True)` returns it:
/// UTF-8 with universal newlines (`\r\n` and `\r` become `\n`).
pub fn universal_newlines(bytes: &[u8]) -> String {
    let text = String::from_utf8(bytes.to_vec()).unwrap_or_else(|error| panic!("output is not UTF-8: {error}"));
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// `shlex.quote(word)`.
pub fn shlex_quote(word: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c);
    if word.is_empty() {
        "''".to_owned()
    } else if word.chars().all(safe) {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\"'\"'"))
    }
}

/// `shlex.join(words)`.
pub fn shlex_join<S: AsRef<str>>(words: &[S]) -> String {
    words.iter().map(|word| shlex_quote(word.as_ref())).collect::<Vec<_>>().join(" ")
}

/// `json.dumps(words)` for a list of strings, with Python's default
/// `", "` separator and ASCII-only escaping.
pub fn json_list<S: AsRef<str>>(words: &[S]) -> String {
    let items: Vec<String> = words.iter().map(|word| json_string(word.as_ref())).collect();
    format!("[{}]", items.join(", "))
}

/// `json.dumps(text)` with `ensure_ascii=True`.
pub fn json_string(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            _ => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
    out
}

/// `urllib.parse.urlsplit(target)` for a request target: the path and the
/// query, without a fragment.
pub fn urlsplit(target: &str) -> (&str, &str) {
    let target = target.split_once('#').map_or(target, |(before, _)| before);
    target.split_once('?').unwrap_or((target, ""))
}

/// `urllib.parse.parse_qs(query)`: names mapped to their values in order;
/// blank values and fields without `=` are dropped.
pub fn parse_qs(query: &str) -> BTreeMap<String, Vec<String>> {
    let mut fields: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for field in query.split('&') {
        let Some((name, value)) = field.split_once('=') else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        fields.entry(unquote_plus(name)).or_default().push(unquote_plus(value));
    }
    fields
}

/// `urllib.parse.unquote(text.replace('+', ' '))` with UTF-8 and
/// `errors='replace'`; malformed escapes stay literal.
pub fn unquote_plus(text: &str) -> String {
    let bytes = text.as_bytes();
    let hex = |index: usize| bytes.get(index).and_then(|&byte| (byte as char).to_digit(16));
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match (bytes[i], hex(i + 1), hex(i + 2)) {
            (b'+', _, _) => decoded.push(b' '),
            (b'%', Some(high), Some(low)) => {
                decoded.push((high * 16 + low) as u8);
                i += 2;
            }
            (byte, _, _) => decoded.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitlines_and_split_follow_python() {
        assert_eq!(splitlines("a\r\nb\rc\n\nd\n"), ["a", "b", "c", "", "d"]);
        assert_eq!(splitlines(""), Vec::<&str>::new());
        assert_eq!(squash("  a \t b\x1fc  "), "a b c");
        assert_eq!(strip("\x1c a \n"), "a");
    }

    #[test]
    fn shlex_and_json_quote_like_python() {
        assert_eq!(
            shlex_join(&["docker", "--config=/tmp/a b", "it's", ""]),
            "docker '--config=/tmp/a b' 'it'\"'\"'s' ''"
        );
        assert_eq!(json_list(&["a", "b\"", "\u{e9}\u{1f600}\x7f"]), r#"["a", "b\"", "\u00e9\ud83d\ude00\u007f"]"#);
        assert_eq!(json_list::<&str>(&[]), "[]");
    }

    #[test]
    fn parse_qs_drops_blank_values_and_decodes() {
        let fields = parse_qs("all=1&limit=&filters=%7B%22a%22%7D&x&size=1&size=2&p=a+b%zz");
        assert_eq!(fields.get("all"), Some(&vec!["1".to_owned()]));
        assert_eq!(fields.get("limit"), None);
        assert_eq!(fields.get("x"), None);
        assert_eq!(fields.get("filters"), Some(&vec!["{\"a\"}".to_owned()]));
        assert_eq!(fields.get("size"), Some(&vec!["1".to_owned(), "2".to_owned()]));
        assert_eq!(fields.get("p"), Some(&vec!["a b%zz".to_owned()]));
        assert_eq!(urlsplit("/a/pods?watch=1#f"), ("/a/pods", "watch=1"));
    }
}
