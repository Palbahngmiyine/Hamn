//! How the TUI shows a detail: the text of a Hamn profile, a YAML or JSON
//! document of a resource, or the operation log. As in k9s's YAML view, the
//! key of each `key: value` line stands out from its value, and a value that
//! is a known state takes the color of that state.
//!
//! Contract: the text is shown unchanged, character for character after
//! control characters are removed; only styles are added. A line that is not
//! `key: value` is shown as it is. Text longer than [`STYLED_LIMIT`] (a log
//! stream can reach 1 MiB) is shown without styles, so a redraw stays cheap.

use crate::tui_failure::Level;
use crate::tui_theme::{self as theme, Tone};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

/// The largest text whose lines are styled.
pub const STYLED_LIMIT: usize = 256 * 1024;

pub fn lines(text: &str) -> Vec<Line<'static>> {
    let text = crate::tui_state::clean(text);
    if text.len() > STYLED_LIMIT {
        return text
            .lines()
            .map(|line| Line::raw(line.to_owned()))
            .collect();
    }
    text.lines().map(line).collect()
}

pub fn key_style() -> Style {
    Style::default()
        .fg(Color::Blue)
        .add_modifier(Modifier::BOLD)
}

fn line(text: &str) -> Line<'static> {
    let Some((prefix, key, value)) = split(text) else {
        return Line::raw(text.to_owned());
    };
    let mut spans = vec![
        Span::raw(prefix.to_owned()),
        Span::styled(key.to_owned(), key_style()),
        Span::styled(":", theme::dim()),
    ];
    if !value.is_empty() {
        let shown = value.trim_start();
        let pad = &value[..value.len() - shown.len()];
        spans.push(Span::raw(pad.to_owned()));
        spans.push(Span::styled(shown.to_owned(), value_style(shown)));
    }
    Line::from(spans)
}

/// Splits `  - key: value` into its indent (with a list dash), key and the
/// text after the colon. A key is a bare word of YAML or of Hamn's details
/// (letters, digits, spaces and `-_./@`, up to 40 characters, starting with a
/// letter) or a JSON string. The colon must end the line or be followed by a
/// space, so a time such as `12:30:00` is not a key.
fn split(text: &str) -> Option<(&str, &str, &str)> {
    let body = text.trim_start();
    let mut start = text.len() - body.len();
    let body = match body.strip_prefix("- ") {
        Some(rest) => {
            start += 2;
            rest
        }
        None => body,
    };
    let end = if let Some(quoted) = body.strip_prefix('"') {
        let close = quoted.find('"')?;
        close + 2
    } else {
        let colon = body.find(':')?;
        let key = &body[..colon];
        let bare = key.chars().next().is_some_and(char::is_alphabetic)
            && key.chars().count() <= 40
            && key
                .chars()
                .all(|c| c.is_alphanumeric() || " -_./@".contains(c))
            && !key.ends_with(' ');
        if !bare {
            return None;
        }
        colon
    };
    let rest = body[end..].strip_prefix(':')?;
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    Some((&text[..start], &body[..end], rest))
}

/// Values that name a state take its color; anything else keeps the text color.
fn value_style(value: &str) -> Style {
    let word = value.trim_end_matches(',').trim_matches('"');
    let tone = match word {
        "running" | "ready" | "Running" | "Available" | "Active" | "Bound" | "True" => {
            Some(Level::Info)
        }
        "starting" | "stopping" | "preparing" | "Pending" | "ContainerCreating" | "Terminating"
        | "restarting" => Some(Level::Warning),
        "recoveryRequired" | "unavailable" | "Failed" | "CrashLoopBackOff" | "ImagePullBackOff"
        | "ErrImagePull" | "OOMKilled" | "Error" | "dead" | "False" | "damaged" => {
            Some(Level::Error)
        }
        _ => None,
    };
    match tone {
        Some(level) => theme::level(level),
        None if matches!(
            word,
            "stopped" | "exited" | "not created" | "Succeeded" | "Completed"
        ) =>
        {
            theme::tone(Tone::Done)
        }
        None => Style::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &Line) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn keys_of_yaml_json_and_hamn_details_stand_out_and_the_text_is_unchanged() {
        let source = "Profile: default\nVM: running\nmetadata:\n  name: api\n  labels:\n    app.kubernetes.io/name: web\n- name: app\n  \"Id\": \"abc\",\nstatus:\n  phase: CrashLoopBackOff\nstarted at 12:30:00 without a key\nhttp://example.com: not a key\n\nplain sentence.";
        let rendered = lines(source);
        assert_eq!(
            rendered.iter().map(text).collect::<Vec<_>>(),
            source.lines().collect::<Vec<_>>()
        );
        let key = |index: usize, key: &str| {
            let line = &rendered[index];
            let span = line.spans.iter().find(|span| span.content == key);
            assert!(
                span.is_some_and(|span| span.style == key_style()),
                "{index}: {line:?}"
            );
        };
        key(0, "Profile");
        key(1, "VM");
        key(2, "metadata");
        key(3, "name");
        key(5, "app.kubernetes.io/name");
        key(6, "name");
        key(7, "\"Id\"");
        key(9, "phase");
        // A time, a URL and a sentence are not keys.
        for index in [10, 11, 13] {
            assert!(
                rendered[index]
                    .spans
                    .iter()
                    .all(|span| span.style != key_style()),
                "{index}"
            );
        }
        // Known states take their colors.
        let value = |index: usize| rendered[index].spans.last().unwrap().style;
        assert_eq!(value(1), theme::level(Level::Info));
        assert_eq!(value(9), theme::level(Level::Error));
        assert_eq!(value(3), Style::default());
    }

    #[test]
    fn long_text_is_shown_without_styles() {
        let long = "key: value\n".repeat(STYLED_LIMIT / 11 + 1);
        let rendered = lines(&long);
        assert_eq!(rendered.len(), long.lines().count());
        assert!(rendered.iter().all(|line| line.spans.len() == 1));
        assert!(
            lines("key: value")[0]
                .spans
                .iter()
                .any(|span| span.style == key_style())
        );
    }

    #[test]
    fn control_characters_are_removed_before_styling() {
        let rendered = lines("name: \u{1b}[31mweb\u{1b}[0m");
        assert_eq!(text(&rendered[0]), "name: [31mweb[0m");
    }
}
