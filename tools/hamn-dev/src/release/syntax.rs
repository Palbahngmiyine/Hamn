//! Exact spellings of release identities (versions, tags, digests and
//! repository names) and POSIX shell quoting. Each function states the
//! accepted grammar; anything else is rejected.
use std::cmp::Ordering;

/// `[0-9a-f]{length}`
pub fn is_hex(text: &str, length: usize) -> bool {
    text.len() == length && text.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// `[0-9]+`
pub fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// `0|[1-9][0-9]*`
fn is_canonical_number(text: &str) -> bool {
    is_digits(text) && (text == "0" || !text.starts_with('0'))
}

/// A canonical `MAJOR.MINOR.PATCH` version (no leading zeros, no suffix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    parts: [String; 3],
}

impl Version {
    pub fn parse(text: &str) -> Option<Self> {
        let parts: Vec<&str> = text.split('.').collect();
        let [major, minor, patch] = parts.as_slice() else { return None };
        [major, minor, patch]
            .iter()
            .all(|part| is_canonical_number(part))
            .then(|| Self { parts: [major.to_string(), minor.to_string(), patch.to_string()] })
    }
}

impl Ord for Version {
    /// Numeric order; canonical numbers of any size compare by length first.
    fn cmp(&self, other: &Self) -> Ordering {
        self.parts
            .iter()
            .zip(&other.parts)
            .map(|(left, right)| left.len().cmp(&right.len()).then_with(|| left.cmp(right)))
            .find(|order| order.is_ne())
            .unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// `vMAJOR.MINOR.PATCH` with canonical numbers.
pub fn is_stable_tag(tag: &str) -> bool {
    tag.strip_prefix('v').is_some_and(|version| Version::parse(version).is_some())
}

/// The stable version `vX.Y.Z` of a candidate tag `vX.Y.Z-rc.N`, where each
/// number is `[0-9]+` (the candidate contract does not require canonical
/// numbers).
pub fn candidate_tag_version(tag: &str) -> Option<&str> {
    let (version, candidate) = tag.rsplit_once("-rc.")?;
    let numbers: Vec<&str> = version.strip_prefix('v')?.split('.').collect();
    (numbers.len() == 3 && numbers.iter().all(|number| is_digits(number)) && is_digits(candidate)).then_some(version)
}

/// `NAME/NAME`, each `[A-Za-z0-9_.-]+`: a GitHub `owner/repository`.
pub fn is_repository(text: &str) -> bool {
    let name = |part: &str| {
        !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
    };
    text.split_once('/').is_some_and(|(owner, repository)| name(owner) && name(repository))
}

/// `[A-Za-z0-9._-]+`: a plain artifact file name.
pub fn is_artifact_name(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

/// Quotes `word` for a POSIX shell exactly as Python's `shlex.quote` does:
/// words of `[A-Za-z0-9_@%+=:,./-]` stay bare, the empty word becomes `''`,
/// and anything else is single-quoted with `'"'"'` for each quote.
pub fn shell_quote(word: &str) -> String {
    if word.is_empty() {
        return "''".into();
    }
    if word.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&byte)) {
        return word.into();
    }
    format!("'{}'", word.replace('\'', "'\"'\"'"))
}

/// Joins shell-quoted words with spaces (Python's `shlex.join`).
pub fn shell_join(words: &[&str]) -> String {
    words.iter().map(|word| shell_quote(word)).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_canonical_and_ordered_numerically() {
        for valid in ["0.0.0", "0.1.0", "10.20.30", "123456789012345678901234567890.0.1"] {
            assert!(Version::parse(valid).is_some(), "{valid}");
        }
        for invalid in
            ["", "0.1", "0.1.0.0", "01.0.0", "0.01.0", "0.1.0-rc.1", "v0.1.0", "../tag", "0.1.", "0..1", "٣.0.0"]
        {
            assert!(Version::parse(invalid).is_none(), "{invalid}");
        }
        let version = |text| Version::parse(text).unwrap();
        assert!(version("0.10.0") > version("0.9.9"));
        assert!(version("1.0.0") > version("0.99.99"));
        assert!(version("0.0.2") > version("0.0.1"));
        assert_eq!(version("2.0.0").cmp(&version("2.0.0")), Ordering::Equal);
        assert!(version("100000000000000000000.0.0") > version("99999999999999999999.0.0"));
    }

    #[test]
    fn tags_digests_and_names_match_only_their_grammar() {
        assert!(
            is_stable_tag("v0.1.0")
                && !is_stable_tag("0.1.0")
                && !is_stable_tag("v01.0.0")
                && !is_stable_tag("v0.1.0-rc.1")
        );
        assert_eq!(candidate_tag_version("v1.0.0-rc.1"), Some("v1.0.0"));
        assert_eq!(candidate_tag_version("v01.0.0-rc.017"), Some("v01.0.0"));
        for invalid in ["v1.0.0", "1.0.0-rc.1", "v1.0-rc.1", "v1.0.0-rc.", "v1.0.0-rc.x", "v1.0.0-beta.1"] {
            assert_eq!(candidate_tag_version(invalid), None, "{invalid}");
        }
        assert!(is_hex(&"a".repeat(40), 40) && !is_hex(&"A".repeat(40), 40) && !is_hex(&"a".repeat(39), 40));
        assert!(is_repository("example/hamn") && is_repository("a_b.c-d/e"));
        for invalid in ["example", "example/hamn/../../other", "/hamn", "example/", "exa mple/hamn", "é/hamn"] {
            assert!(!is_repository(invalid), "{invalid}");
        }
        assert!(is_artifact_name("install.sh") && !is_artifact_name("../outside") && !is_artifact_name(""));
    }

    #[test]
    fn shell_quoting_matches_shlex() {
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("https://github.com/a/b/v0.0.1/x.tar.gz"), "https://github.com/a/b/v0.0.1/x.tar.gz");
        assert_eq!(shell_quote("file:///tmp/a b"), "'file:///tmp/a b'");
        assert_eq!(shell_quote("it's"), "'it'\"'\"'s'");
        assert_eq!(shell_quote("$HOME"), "'$HOME'");
        assert_eq!(
            shell_join(&["sudo", "bash", "-euc", "echo 'x'; true"]),
            "sudo bash -euc 'echo '\"'\"'x'\"'\"'; true'"
        );
    }
}
