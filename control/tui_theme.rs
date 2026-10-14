//! The look of the TUI, after the roles of a k9s skin and the palette of
//! subux.dev: a pastel accent per workspace (sky blue for Containers, lilac
//! for Kubernetes) frames its view and fills its pills, the active tab, the
//! keys, the selected row and the crumbs, which carry dark text; rows take a
//! status color, messages a level color, and key hints read `<key> label`.
//!
//! Contract: every distinction that a color makes is also made by text, so
//! the screen reads the same without color: the active tab is `[Name]`, keys
//! are in angle brackets, old rows are titled `[stale]`, and a message starts
//! with an icon of its level. A pill sets both its background and its text
//! color, so it reads the same on light and dark terminals. The accents are
//! 256-color pastels, which terminals show without truecolor support; status
//! and message colors stay in the 16 ANSI colors, whose shades the
//! terminal's own palette adapts to its background.
//! The functions only build styled text; they hold no state.

use crate::preferences::Workspace;
use crate::tui_failure::Level;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

/// Frames and pills of the Containers and Kubernetes workspaces differ in
/// color, so that Tab visibly changes the screen. Containers takes the
/// 256-color sky blue nearest subux.dev's #c2e7ff (#afd7ff); Kubernetes a
/// lilac of the same lightness (#d7afff).
pub fn accent(workspace: Workspace) -> Color {
    match workspace {
        Workspace::Containers => Color::Indexed(153),
        Workspace::Kubernetes => Color::Indexed(183),
    }
}

/// The text of a pill: near subux.dev's #1f1f1f (#262626).
const PILL_TEXT: Color = Color::Indexed(235);
/// A pill that marks no workspace, such as an outer crumb (#e4e4e4).
const PILL_NEUTRAL: Color = Color::Indexed(254);

/// Dark text on a light background color, readable on any terminal.
fn pill(color: Color) -> Style {
    Style::default().fg(PILL_TEXT).bg(color)
}

pub fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// A key, drawn as a pill like a keycap.
pub fn key(workspace: Workspace) -> Style {
    pill(accent(workspace)).add_modifier(Modifier::BOLD)
}

/// A label of the header, such as `Context:`, muted as subux.dev's #666.
pub fn label() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// A value, in the terminal's own text color. The color is set rather than
/// inherited, so a value in a block title does not take the pale border color.
pub fn value() -> Style {
    Style::default()
        .fg(Color::Reset)
        .add_modifier(Modifier::BOLD)
}

pub fn border(workspace: Workspace) -> Style {
    Style::default().fg(accent(workspace))
}

/// The highlight of the workspace: the active tab, the selected row and the
/// current crumb, as a pill of its accent.
pub fn badge(workspace: Workspace) -> Style {
    pill(accent(workspace)).add_modifier(Modifier::BOLD)
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
                pill(PILL_NEUTRAL)
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
            // A pill of the accent with its own dark text, on any terminal.
            assert_eq!(
                (tab.style.bg, tab.style.fg),
                (Some(accent(active)), Some(PILL_TEXT))
            );
        }
        assert_ne!(accent(Workspace::Containers), accent(Workspace::Kubernetes));
        // subux.dev's sky blue for Containers, in the 256-color cube.
        assert_eq!(accent(Workspace::Containers), Color::Indexed(153));
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
            (key.content.as_ref(), key.style.bg, key.style.fg),
            ("<:>", Some(accent(Workspace::Kubernetes)), Some(PILL_TEXT))
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
        assert_eq!(spans[2].style.bg, Some(accent(Workspace::Kubernetes)));
        assert_eq!(spans[0].style.bg, Some(PILL_NEUTRAL));
        assert!(
            spans
                .iter()
                .step_by(2)
                .all(|span| span.style.fg == Some(PILL_TEXT))
        );
    }
}
