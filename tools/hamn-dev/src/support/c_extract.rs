//! Copies C function definitions out of production sources by brace
//! matching, so a suite can compile them against its own stubs and fault
//! injection without linking the rest of the file.
use std::path::Path;

/// The definition of `name` in `text`: from the start of the line holding
/// the first occurrence of `name(` through the brace that closes the first
/// `{` after that line start, followed by a newline.
///
/// The first occurrence is a plain substring match, so a name that is the
/// suffix of an earlier identifier (`deployment_refresh_locked` inside
/// `guest_deployment_refresh_locked`) matches there; callers name functions
/// whose first mention is their definition. Braces are counted without
/// regard to strings or comments, which the extracted functions keep
/// balanced. Panics when the name or a balanced body is missing.
pub fn function(text: &str, name: &str) -> String {
    let call = format!("{name}(");
    let found = text.find(&call).unwrap_or_else(|| panic!("{call} is not in the source"));
    let start = text[..found].rfind('\n').map_or(0, |newline| newline + 1);
    let opening = start + text[start..].find('{').unwrap_or_else(|| panic!("{name} has no body"));
    let bytes = text.as_bytes();
    let mut depth = 1usize;
    let mut end = opening + 1;
    while depth > 0 {
        let byte = *bytes.get(end).unwrap_or_else(|| panic!("{name} has an unbalanced body"));
        match byte {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            _ => {}
        }
        end += 1;
    }
    // Braces and newlines are ASCII, so every index above is a character
    // boundary of the UTF-8 text.
    format!("{}\n", &text[start..end])
}

/// `function` applied to the file at `path`.
pub fn function_in(path: &Path, name: &str) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    function(&text, name)
}

#[cfg(test)]
mod tests {
    use super::function;

    #[test]
    fn copies_from_line_start_through_the_matching_brace() {
        let text = "/* 설명 */\nstatic int\nhelper(void) { return 0; }\nint target(int a)\n{\n    if (a) { return 1; }\n    return 0;\n}\nint after(void) {}\n";
        assert_eq!(function(text, "target"), "int target(int a)\n{\n    if (a) { return 1; }\n    return 0;\n}\n");
        assert_eq!(function(text, "helper"), "helper(void) { return 0; }\n");
    }

    #[test]
    fn matches_the_first_occurrence_even_inside_a_longer_name() {
        let text = "int outer_name(void) { return 1; }\nint name(void) { return 2; }\n";
        assert_eq!(function(text, "name"), "int outer_name(void) { return 1; }\n");
    }

    #[test]
    fn a_function_on_the_first_line_starts_at_offset_zero() {
        assert_eq!(function("int first(void) { { } }", "first"), "int first(void) { { } }\n");
    }

    #[test]
    #[should_panic(expected = "unbalanced body")]
    fn rejects_an_unbalanced_body() {
        function("int broken(void) { {\n", "broken");
    }

    #[test]
    #[should_panic(expected = "missing(")]
    fn rejects_a_missing_name() {
        function("int present(void) {}\n", "missing");
    }
}
