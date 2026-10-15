//! The keys that each view of the TUI offers, as the key handler in
//! `tui.rs` interprets them: the hints of the header and the help screen.
//!
//! Contract: a hint names only a key that the handler acts on in the view
//! the state describes, ordered by how often it is needed; the help screen
//! lists every key and command form the handler accepts. When the handler
//! gains or changes a key, this module changes with it.

use crate::preferences::Workspace;
use crate::tui_state::State;
use crate::tui_theme::{self, Hint};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

pub fn hints(state: &State) -> Vec<Hint> {
    // A profile whose guest disk is damaged cannot be repaired by a start;
    // its keys offer the reset of the disk instead.
    let selected_damaged = state
        .selected()
        .is_some_and(|row| crate::tui_state::guest_disk_damaged(&row));
    let start = if selected_damaged {
        "start"
    } else {
        "start/repair"
    };
    if state.help {
        return vec![("Esc", "close help"), ("j/k", "scroll"), ("q", "quit")];
    }
    if state.show_operation {
        return vec![
            ("Esc", "back"),
            ("j/k", "scroll"),
            ("!", "close log"),
            ("?", "help"),
            ("q", "quit"),
        ];
    }
    // A profile's detail keeps the keys that act on it.
    if state.detail.is_some() && state.vm_panel() {
        return vec![
            ("Esc", "back"),
            ("s", start),
            ("t", "stop"),
            ("c", "settings"),
            ("x", "reset disk"),
            ("d", "delete"),
            ("j/k", "scroll"),
            ("?", "help"),
            ("q", "quit"),
        ];
    }
    if state.detail.is_some() {
        return vec![
            ("Esc", "back"),
            ("j/k", "scroll"),
            ("!", "operation log"),
            ("?", "help"),
            ("q", "quit"),
        ];
    }
    let operation = state.request.operation();
    if state.environment_picker
        || (state.native.is_none()
            && matches!(
                operation.as_str(),
                "k8s contexts list" | "k8s namespaces list"
            ))
    {
        return vec![
            ("Enter", "select"),
            ("Esc", "cancel"),
            ("/", "filter"),
            (":", "command"),
            ("?", "help"),
            ("q", "quit"),
        ];
    }
    if state.vm_panel() {
        return vec![
            ("Enter", "detail"),
            ("s", start),
            ("t", "stop"),
            ("c", "settings"),
            ("x", "reset disk"),
            ("d", "delete"),
            ("Esc", "containers"),
            ("/", "filter"),
            (":", "command"),
            ("?", "help"),
            ("q", "quit"),
        ];
    }
    let mut hints = vec![
        (":", "command"),
        ("/", "filter"),
        ("Enter", "detail"),
        ("l", "logs"),
    ];
    match state.workspace {
        Workspace::Containers => {
            // s starts the VM of a Hamn profile whose Docker is not ready.
            let hamn_view = state
                .native
                .as_ref()
                .is_some_and(|i| i.hamn_profile.is_some());
            let vm_start = hamn_view && state.runtime["dockerStatus"] != "ready";
            let disk_damaged = hamn_view && crate::tui_state::guest_disk_damaged(&state.runtime);
            if !disk_damaged {
                hints.push(("s", if vm_start { "start VM" } else { "start" }));
            }
            hints.extend([
                ("t", "stop"),
                ("r", "restart"),
                ("d", "delete"),
                ("g", "stats"),
                ("m", "actions"),
                ("e", "environments"),
            ]);
            if disk_damaged {
                // The one key that helps comes first, so it fits a narrow terminal.
                hints.insert(0, ("v", "VM: reset disk"));
            } else if state.hamn_environment() {
                hints.push(("v", "VM settings"));
            }
            if state
                .native
                .as_ref()
                .is_some_and(|i| i.resource.as_deref() == Some("containers"))
            {
                hints.push(("a", "running/all"));
            }
        }
        Workspace::Kubernetes => hints.extend([
            ("d", "delete"),
            ("r", "rollout"),
            ("g", "metrics"),
            ("m", "actions"),
            ("e", "contexts"),
            ("n", "namespaces"),
            ("b", "recent"),
        ]),
    }
    hints.extend([("?", "help"), ("q", "quit")]);
    hints
}

type Section = (&'static str, &'static [Hint]);

/// Marks where the sentences about `:` go among the sections.
const COMMAND_SECTION: Section = ("", &[]);

const SECTIONS: &[Section] = &[
    (
        "GENERAL",
        &[
            (":", "command in Docker or kubectl syntax"),
            ("/", "filter the rows"),
            ("?", "this help"),
            ("Tab", "switch workspace"),
            (",", "choose the default workspace"),
            ("!", "operation log and last error"),
            ("Esc", "back; clear the filter"),
            ("Ctrl+Z", "suspend"),
            ("q", "quit"),
        ],
    ),
    COMMAND_SECTION,
    (
        "RESOURCE  (selected row; changes ask to confirm)",
        &[
            ("Enter", "detail"),
            ("l", "logs"),
            ("g", "stats (Docker) / metrics (Kubernetes)"),
            ("s", "start; starts or repairs a Hamn VM"),
            ("t", "stop"),
            ("r", "restart / rollout restart"),
            ("d", "delete"),
            ("m", "every action of the row"),
            ("a", "running or all containers"),
        ],
    ),
    (
        "TARGET",
        &[
            ("e", "Hamn profile, Docker context or kube context"),
            ("n", "Kubernetes namespace"),
            (
                "v",
                "Hamn VM controls; c edits CPU, memory, disk; x resets the disk",
            ),
            ("b", "recent targets"),
            ("f", "toggle favorite target"),
            ("F", "favorite targets"),
        ],
    ),
    (
        "NAVIGATION",
        &[
            ("j / ↓", "down; scrolls a detail"),
            ("k / ↑", "up"),
            ("Ctrl+Alt+B", "browser, CLI keeps running"),
            ("Ctrl+Alt+S", "sessions: Enter resumes, d ends"),
        ],
    ),
    (
        "REFRESH",
        &[
            ("p", "pause or resume"),
            ("[ / ]", "halve / double the interval"),
            ("R", "refresh now"),
        ],
    ),
    (
        "INTERNAL TERMINAL",
        &[
            ("Ctrl+C", "interrupts the CLI"),
            ("Ctrl+P Ctrl+Q", "detaches from Docker"),
            ("Ctrl+Alt+C", "interrupts pending input"),
            ("Shift+PgUp", "scrolls back"),
            ("Enter", "after exit: back and refresh"),
        ],
    ),
];

/// What `:` accepts, in sentences; the first starts with `Commands:`.
const COMMANDS: &[&str] = &[
    "Commands: Containers ps, ps -a, images, volume ls, network ls, compose ls",
    "  Kubernetes pods, deployments, services, nodes, get pods -A, get <type>",
    "  docker and kubectl prefixes are accepted; output options are kept.",
    "  Other commands run in the internal terminal, without confirmation.",
    "  :refresh-timeout 30 sets a 1..300 second query deadline.",
    "  Tab or → takes the dimmed completion: history, then known commands;",
    "  get <type> also completes the kinds the cluster serves, CRDs too.",
    "  Up / Down recall this session's command history.",
    "  Shell pipelines, redirections and aliases are not interpreted.",
];

fn section_lines(section: &Section, workspace: Workspace) -> Vec<Line<'static>> {
    let width = section
        .1
        .iter()
        .map(|hint| hint.0.chars().count() + 2)
        .max()
        .unwrap_or(0);
    let mut lines = vec![Line::styled(
        section.0,
        Style::default().add_modifier(Modifier::BOLD),
    )];
    for (key, label) in section.1 {
        let shown = format!("<{key}>");
        let pad = " ".repeat(width.saturating_sub(shown.chars().count()) + 1);
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(shown, tui_theme::key(workspace)),
            Span::raw(pad),
            Span::raw(*label),
        ]));
    }
    lines.push(Line::raw(""));
    lines
}

/// The help screen in as many columns as `width` allows, up to three.
pub fn help(workspace: Workspace, width: u16) -> Vec<Vec<Line<'static>>> {
    let mut commands: Vec<Line<'static>> = COMMANDS
        .iter()
        .enumerate()
        .map(|(index, line)| {
            if index == 0 {
                Line::styled(*line, Style::default().add_modifier(Modifier::BOLD))
            } else {
                Line::raw(*line)
            }
        })
        .collect();
    commands.push(Line::raw(""));
    let mut commands = Some(commands);
    let sections: Vec<Vec<Line<'static>>> = SECTIONS
        .iter()
        .map(|section| {
            if section.0.is_empty() {
                commands.take().unwrap_or_default()
            } else {
                section_lines(section, workspace)
            }
        })
        .collect();
    let count = match width {
        0..100 => 1,
        100..150 => 2,
        _ => 3,
    };
    // Fill the columns in order, each up to an equal share of the lines.
    let share = sections.iter().map(Vec::len).sum::<usize>().div_ceil(count);
    let mut columns: Vec<Vec<Line<'static>>> = vec![Vec::new()];
    for section in sections {
        let last = columns.last().unwrap();
        if !last.is_empty() && last.len() + section.len() / 2 > share && columns.len() < count {
            columns.push(Vec::new());
        }
        columns.last_mut().unwrap().extend(section);
    }
    columns
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Request;

    fn keys(state: &State) -> Vec<&'static str> {
        hints(state).into_iter().map(|hint| hint.0).collect()
    }

    #[test]
    fn hints_follow_the_view_and_offer_only_keys_it_handles() {
        let mut state = State::new(Request::default());
        state.native = Some(crate::native::parse("ps", &state).unwrap());
        let containers = keys(&state);
        for key in [":", "/", "Enter", "s", "t", "d", "e", "v", "a", "?", "q"] {
            assert!(containers.contains(&key), "{key}: {containers:?}");
        }
        // A Hamn profile whose Docker is not ready: s starts the VM.
        assert!(hints(&state).contains(&("s", "start VM")));
        state.runtime = serde_json::json!({"dockerStatus": "ready"});
        assert!(hints(&state).contains(&("s", "start")));
        // A Docker context has no VM, and images have no running/all toggle.
        state.docker_context = Some("remote".into());
        state.native = Some(crate::native::parse("images", &state).unwrap());
        let images = keys(&state);
        assert!(
            !images.contains(&"v") && !images.contains(&"a"),
            "{images:?}"
        );
        state.workspace = Workspace::Kubernetes;
        state.native = Some(crate::native::parse("get pods", &state).unwrap());
        let pods = keys(&state);
        for key in ["n", "e", "l", "d", "r"] {
            assert!(pods.contains(&key), "{key}: {pods:?}");
        }
        assert!(!pods.contains(&"s") && !pods.contains(&"v"), "{pods:?}");
        // Pickers select; details and help scroll and go back.
        state.environment_picker = true;
        assert_eq!(hints(&state)[0], ("Enter", "select"));
        state.environment_picker = false;
        state.detail = Some("text".into());
        assert_eq!(hints(&state)[0], ("Esc", "back"));
        state.help = true;
        assert_eq!(hints(&state)[0], ("Esc", "close help"));
        let mut panel = State::new(Request::default());
        panel.view("vm").unwrap();
        assert!(keys(&panel).contains(&"c"));
    }

    #[test]
    fn help_lists_every_section_in_one_or_two_columns() {
        let text = |columns: &Vec<Vec<Line>>| -> String {
            columns
                .iter()
                .flatten()
                .map(|line| {
                    line.spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let narrow = help(Workspace::Containers, 80);
        let medium = help(Workspace::Containers, 120);
        let wide = help(Workspace::Containers, 160);
        assert_eq!((narrow.len(), medium.len(), wide.len()), (1, 2, 3));
        assert_eq!(text(&narrow), text(&medium));
        // Three columns fit the view of a 32-line terminal without scrolling.
        assert!(
            wide.iter().all(|column| column.len() <= 22),
            "{:?}",
            wide.iter().map(Vec::len).collect::<Vec<_>>()
        );
        // What `:` accepts comes right after the general keys.
        let first = text(&narrow);
        assert!(first.find("Commands:") < first.find("RESOURCE"), "{first}");
        assert_eq!(text(&narrow), text(&wide));
        for marker in [
            "GENERAL",
            "TARGET",
            "INTERNAL TERMINAL",
            "Commands:",
            "<Ctrl+Alt+S>",
            "<:>",
            "refresh-timeout",
            "Tab or → takes the dimmed completion",
        ] {
            assert!(text(&narrow).contains(marker), "{marker}");
        }
    }
}
