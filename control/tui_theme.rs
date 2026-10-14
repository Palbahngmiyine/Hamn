//! The look of the TUI, after the roles of a k9s skin: an accent color per
//! workspace for its frame, tab, crumbs and keys; status colors for rows; a
//! color per message level; and key hints drawn as `<key> label`.
//!
//! Contract: every distinction that a color makes is also made by text, so
//! the screen reads the same without color: the active tab is `[Name]`, keys
//! are in angle brackets, old rows are titled `[stale]`, and a message starts
//! with an icon of its level. Only the 16 ANSI colors are used, so the
//! terminal's own palette decides the exact shades on dark and light themes.
//! The functions only build styled text; they hold no state.

use crate::preferences::Workspace;
use crate::tui_failure::Level;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

/// Frames, the active tab and keys of the Containers and Kubernetes
/// workspaces differ in color, so that Tab visibly changes the screen.
pub fn accent(workspace: Workspace) -> Color {
    match workspace {
        Workspace::Containers => Color::Cyan,
        Workspace::Kubernetes => Color::Magenta,
    }
}

pub fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

pub fn key(workspace: Workspace) -> Style {
    Style::default()
        .fg(accent(workspace))
        .add_modifier(Modifier::BOLD)
}

/// A label of the header, such as `Context:`.
pub fn label() -> Style {
    Style::default().fg(Color::Yellow)
}

pub fn value() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

pub fn border(workspace: Workspace) -> Style {
    Style::default().fg(accent(workspace))
}

/// Highlighted text on the accent color: the active tab, the selected row
/// and the crumbs. The accent is reversed rather than paired with a fixed
/// text color, so the text takes the terminal's own background color and
/// stays readable on light and dark themes alike.
pub fn badge(workspace: Workspace) -> Style {
    Style::default()
        .fg(accent(workspace))
        .add_modifier(Modifier::BOLD | Modifier::REVERSED)
}

pub fn level(level: Level) -> Style {
    match level {
        Level::Info => Style::default().fg(Color::Green),
        Level::Warning => Style::default().fg(Color::Yellow),
        Level::Error => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    }
}

pub fn icon(level: Level) -> &'static str {
    match level {
        Level::Info => "✓",
        Level::Warning => "!",
        Level::Error => "✗",
    }
}

/// How a row's status reads, from the state its resource reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Running as asked, or a resource without a state.
    Normal,
    /// Changing, waiting or partly ready.
    Pending,
    /// Failed or unhealthy.
    Failed,
    /// Finished or stopped on purpose.
    Done,
}

pub fn tone(tone: Tone) -> Style {
    match tone {
        Tone::Normal => Style::default(),
        Tone::Pending => Style::default().fg(Color::Yellow),
        Tone::Failed => Style::default().fg(Color::Red),
        Tone::Done => Style::default().fg(Color::DarkGray),
    }
}

/// The tab line: the product name and both workspaces, the active one as
/// `[Name]` on its accent color.
pub fn tabs(active: Workspace) -> Line<'static> {
    let mut spans = vec![
        Span::styled(
            " Hamn ",
            Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
        ),
        Span::raw(" "),
    ];
    for workspace in [Workspace::Containers, Workspace::Kubernetes] {
        let name = match workspace {
            Workspace::Containers => "Containers",
            Workspace::Kubernetes => "Kubernetes",
        };
        spans.push(if workspace == active {
            Span::styled(format!("[{name}]"), badge(workspace))
        } else {
            Span::styled(format!(" {name} "), dim())
        });
        spans.push(Span::raw(" "));
    }
    // The key that switches is part of the tabs, where it is always shown.
    spans.extend([
        Span::raw(" "),
        Span::styled("<Tab>", key(active)),
        Span::styled(" switch", dim()),
    ]);
    Line::from(spans)
}

/// A key and what it does in the current view.
pub type Hint = (&'static str, &'static str);

fn hint_width(hint: &Hint) -> usize {
    Line::raw(format!("<{}> {}", hint.0, hint.1)).width()
}

fn hint_spans(hint: &Hint, workspace: Workspace, width: usize) -> Vec<Span<'static>> {
    let used = hint_width(hint);
    vec![
        Span::styled(format!("<{}>", hint.0), key(workspace)),
        Span::raw(" "),
        Span::raw(hint.1),
        Span::raw(" ".repeat(width.saturating_sub(used))),
    ]
}

/// The hints that fit `capacity` places. When some do not fit, the last
/// place shows help (`?`), where every key is listed.
fn fitting(hints: &[Hint], capacity: usize) -> Vec<Hint> {
    if hints.len() <= capacity {
        return hints.to_vec();
    }
    let mut shown: Vec<Hint> = hints
        .iter()
        .filter(|hint| hint.0 != "?")
        .take(capacity.saturating_sub(1))
        .copied()
        .collect();
    if let Some(help) = hints.iter().find(|hint| hint.0 == "?") {
        shown.push(*help);
    }
    shown
}

/// Hints in columns of equal width, filled downward as in k9s's header:
/// at most `rows` lines within `width` columns.
pub fn hint_grid(
    hints: &[Hint],
    workspace: Workspace,
    width: u16,
    rows: usize,
) -> Vec<Line<'static>> {
    let column = hints.iter().map(hint_width).max().unwrap_or(0) + 2;
    let columns = (usize::from(width) / column.max(1)).max(1);
    let shown = fitting(hints, columns * rows.max(1));
    let rows = shown.len().div_ceil(columns).min(rows.max(1));
    (0..rows)
        .map(|row| {
            Line::from(
                (0..columns)
                    .filter_map(|col| shown.get(col * rows + row))
                    .flat_map(|hint| hint_spans(hint, workspace, column))
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

/// The width that [`hint_grid`] needs for `columns` columns.
pub fn hint_grid_width(hints: &[Hint], columns: usize) -> u16 {
    let column = hints.iter().map(hint_width).max().unwrap_or(0) + 2;
    u16::try_from(column * columns).unwrap_or(u16::MAX)
}

/// Hints one after another, wrapped to `width`, in at most `lines` lines:
/// the narrow-terminal form of [`hint_grid`].
pub fn hint_flow(
    hints: &[Hint],
    workspace: Workspace,
    width: u16,
    lines: usize,
) -> Vec<Line<'static>> {
    let width = usize::from(width);
    let place = |shown: &[Hint]| {
        let mut out: Vec<Vec<Hint>> = vec![Vec::new()];
        let mut used = 0;
        for hint in shown {
            let size = hint_width(hint) + 2;
            if used + size > width && !out.last().unwrap().is_empty() {
                out.push(Vec::new());
                used = 0;
            }
            out.last_mut().unwrap().push(*hint);
            used += size;
        }
        out
    };
    let mut capacity = hints.len();
    let mut placed = place(hints);
    while placed.len() > lines.max(1) && capacity > 1 {
        capacity -= 1;
        placed = place(&fitting(hints, capacity));
    }
    placed
        .into_iter()
        .take(lines.max(1))
        .map(|line| {
            Line::from(
                line.iter()
                    .flat_map(|hint| hint_spans(hint, workspace, hint_width(hint) + 2))
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

/// The crumbs at the bottom: where the user is, from the resource to the
/// view that covers it.
pub fn crumbs(names: &[String], workspace: Workspace) -> Vec<Span<'static>> {
    let last = names.len().saturating_sub(1);
    names
        .iter()
        .enumerate()
        .flat_map(|(index, name)| {
            let style = if index == last {
                badge(workspace)
            } else {
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::REVERSED)
            };
            [Span::styled(format!(" {name} "), style), Span::raw(" ")]
        })
        .collect()
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

    const HINTS: &[Hint] = &[
        (":", "command"),
        ("/", "filter"),
        ("Enter", "detail"),
        ("l", "logs"),
        ("d", "delete"),
        ("?", "help"),
        ("q", "quit"),
    ];

    #[test]
    fn the_active_tab_is_marked_by_text_and_by_its_workspace_color() {
        for (active, other) in [
            (Workspace::Containers, Workspace::Kubernetes),
            (Workspace::Kubernetes, Workspace::Containers),
        ] {
            let line = tabs(active);
            let shown = text(&line);
            let name = |w| {
                if w == Workspace::Containers {
                    "Containers"
                } else {
                    "Kubernetes"
                }
            };
            assert!(shown.contains(&format!("[{}]", name(active))), "{shown}");
            assert!(!shown.contains(&format!("[{}]", name(other))), "{shown}");
            assert!(shown.ends_with("<Tab> switch"), "{shown}");
            let tab = line
                .spans
                .iter()
                .find(|span| span.content.contains('['))
                .unwrap();
            assert_eq!(tab.style.fg, Some(accent(active)));
            assert!(tab.style.add_modifier.contains(Modifier::REVERSED));
            // No fixed text color: the terminal's background shows through.
            assert_eq!(tab.style.bg, None);
        }
        assert_ne!(accent(Workspace::Containers), accent(Workspace::Kubernetes));
    }

    #[test]
    fn the_grid_fills_columns_downward_and_keeps_help_when_hints_do_not_fit() {
        // Each cell is `<Enter> detail` plus two spaces: 16 columns.
        let lines = hint_grid(HINTS, Workspace::Containers, 48, 3);
        let shown: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(shown.len(), 3);
        assert!(
            shown[0].starts_with("<:> command") && shown[0].contains("<l> logs"),
            "{shown:?}"
        );
        assert!(
            shown[1].starts_with("</> filter") && shown[1].contains("<d> delete"),
            "{shown:?}"
        );
        // Two rows of one column: the second place is help.
        let shown: Vec<String> = hint_grid(HINTS, Workspace::Containers, 20, 2)
            .iter()
            .map(text)
            .collect();
        assert_eq!(shown.len(), 2);
        assert!(
            shown[0].starts_with("<:> command") && shown[1].starts_with("<?> help"),
            "{shown:?}"
        );
        let key = hint_grid(HINTS, Workspace::Kubernetes, 48, 3)[0].spans[0].clone();
        assert_eq!(
            (key.content.as_ref(), key.style.fg),
            ("<:>", Some(accent(Workspace::Kubernetes)))
        );
        assert_eq!(hint_grid_width(HINTS, 3), 48);
    }

    #[test]
    fn flowing_hints_wrap_within_their_width_and_keep_help() {
        for width in [20u16, 40, 80, 200] {
            for lines in [1usize, 2] {
                let shown: Vec<String> = hint_flow(HINTS, Workspace::Containers, width, lines)
                    .iter()
                    .map(text)
                    .collect();
                assert!(shown.len() <= lines, "{width}/{lines}: {shown:?}");
                assert!(
                    shown
                        .iter()
                        .all(|line| Line::raw(line.trim_end()).width() <= usize::from(width))
                        || width < 20,
                    "{width}/{lines}: {shown:?}"
                );
                assert!(
                    shown.join(" ").contains("<?> help"),
                    "{width}/{lines}: {shown:?}"
                );
            }
        }
        let wide: Vec<String> = hint_flow(HINTS, Workspace::Containers, 200, 1)
            .iter()
            .map(text)
            .collect();
        assert!(
            wide[0].contains("<q> quit") && wide[0].contains("<Enter> detail"),
            "{wide:?}"
        );
    }

    #[test]
    fn crumbs_mark_the_current_view() {
        let spans = crumbs(&["pods".into(), "detail".into()], Workspace::Kubernetes);
        let shown: String = spans.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(shown, " pods   detail  ");
        assert_eq!(spans[2].style.fg, Some(accent(Workspace::Kubernetes)));
        assert!(spans[0].style.add_modifier.contains(Modifier::REVERSED));
        assert_ne!(spans[0].style.fg, spans[2].style.fg);
    }
}
