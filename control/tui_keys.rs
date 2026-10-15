//! The keys that each view of the TUI offers, as the key handler in
//! `tui.rs` interprets them: the hints of the header and the help screen.
//!
//! Contract: a hint names only a key that the handler acts on in the view
//! the state describes, ordered by how often it is needed; the help screen
//! lists every key and command form the handler accepts. When the handler
//! gains or changes a key, this module changes with it. The keys of the
//! selected row are decided here, in [`row_key`], for both.
//!
//! Row keys follow k9s: `d` describe, `y` YAML, `s` shell, `F` port-forward,
//! `l` logs and `Ctrl+D` delete. `t` starts or stops a container by its state.

use crate::native_actions;
use crate::preferences::Workspace;
use crate::tui_state::State;
use crate::tui_theme::{self, Hint};
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

/// What a key does to the selected row, as `tui.rs` runs it.
#[derive(Debug, PartialEq, Eq)]
pub enum RowKey {
    /// `native_actions::selected` on the row of a CLI list.
    Native(&'static str),
    /// The logs menu: containers and previous instances.
    Logs,
    /// A shell; a Pod with several containers asks which first.
    Shell,
    /// The port prompt of `kubectl port-forward`.
    PortForward,
    /// `vm start` of the Hamn profile whose Docker is not ready.
    StartVm,
    /// `State::action` of a Hamn view, such as the VM panel.
    Hamn(&'static str),
    /// The detail of the selected VM-panel row, as Enter shows it.
    Detail,
}

/// The row action of a key: `Ctrl+D` and plain letters; any other modifier
/// with a letter is no row key.
pub fn row_key(state: &State, code: KeyCode, modifiers: KeyModifiers) -> Option<RowKey> {
    let KeyCode::Char(key) = code else {
        return None;
    };
    let control = modifiers.contains(KeyModifiers::CONTROL);
    if modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SUPER) || (control && key != 'd') {
        return None;
    }
    if state.native.is_none() {
        return match (key, control) {
            ('d', true) => Some(RowKey::Hamn("delete")),
            ('d', false) if state.vm_panel() => Some(RowKey::Detail),
            ('s', _) => Some(RowKey::Hamn("start")),
            ('t', _) => Some(RowKey::Hamn("stop")),
            ('r', _) => Some(RowKey::Hamn("restart")),
            ('l', _) => Some(RowKey::Hamn("logs")),
            ('g', _) => Some(RowKey::Hamn("stats")),
            _ => None,
        };
    }
    Some(match (key, control) {
        ('d', true) => RowKey::Native("delete"),
        ('s', _) if vm_start(state) => RowKey::StartVm,
        ('s', _) => RowKey::Shell,
        ('d', _) => RowKey::Native("describe"),
        ('y', _) => RowKey::Native("yaml"),
        ('l', _) => RowKey::Logs,
        ('F', _) => RowKey::PortForward,
        ('t', _) => RowKey::Native(start_or_stop(state)),
        ('r', _) => RowKey::Native("restart"),
        ('g', _) => RowKey::Native("stats"),
        _ => return None,
    })
}

/// A Hamn profile whose Docker is not ready: `s` starts its VM.
fn vm_start(state: &State) -> bool {
    state
        .native
        .as_ref()
        .is_some_and(|i| i.hamn_profile.is_some())
        && state.runtime["dockerStatus"] != "ready"
}

/// `t` starts a created or exited container and stops any other.
fn start_or_stop(state: &State) -> &'static str {
    let row = state.selected().unwrap_or_default();
    if row["State"] == "created" || row["State"] == "exited" {
        "start"
    } else {
        "stop"
    }
}

/// The row keys of a CLI list in the order of the hints, with the action
/// that [`row_key`] gives each and its label in Docker and Kubernetes.
const ROW_KEYS: &[(&str, &str, &str, &str)] = &[
    ("l", "logs", "logs", "logs"),
    ("s", "shell", "shell", "shell"),
    ("d", "describe", "describe", "describe"),
    ("y", "yaml", "json", "yaml"),
    ("F", "port-forward", "port-forward", "port-forward"),
    ("t", "stop", "", ""),
    ("r", "restart", "restart", "rollout"),
    ("Ctrl+D", "delete", "delete", "delete"),
    ("g", "stats", "stats", "metrics"),
];

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
            ("Ctrl+D", "delete"),
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
            ("Ctrl+D", "delete"),
            ("Esc", "containers"),
            ("/", "filter"),
            (":", "command"),
            ("?", "help"),
            ("q", "quit"),
        ];
    }
    // A Hamn profile whose guest disk is damaged: a start cannot repair
    // it, so the reset of the disk takes the place of the start key.
    let disk_damaged = state
        .native
        .as_ref()
        .is_some_and(|i| i.hamn_profile.is_some())
        && crate::tui_state::guest_disk_damaged(&state.runtime);
    let mut hints = vec![(":", "command"), ("/", "filter"), ("Enter", "detail")];
    if let Some(invocation) = &state.native {
        let docker = state.workspace == Workspace::Containers;
        if vm_start(state) && !disk_damaged {
            hints.push(("s", "start VM"));
        }
        for &(key, action, docker_label, kube_label) in ROW_KEYS {
            if !native_actions::offers(invocation, action) || (key == "s" && vm_start(state)) {
                continue;
            }
            let label = if key == "t" {
                start_or_stop(state)
            } else if docker {
                docker_label
            } else {
                kube_label
            };
            hints.push((key, label));
        }
        hints.push(("m", "actions"));
    }
    match state.workspace {
        Workspace::Containers => {
            hints.push(("e", "environments"));
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
        Workspace::Kubernetes => {
            hints.extend([("e", "contexts"), ("n", "namespaces"), ("b", "recent")])
        }
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
            ("d", "describe (Docker: inspect)"),
            ("y", "YAML (Docker: inspect JSON)"),
            ("l", "logs"),
            ("s", "shell; VM start if Docker is down"),
            ("F", "port-forward (Kubernetes)"),
            ("t", "start or stop a container"),
            ("r", "restart / rollout restart"),
            ("Ctrl+D", "delete"),
            ("g", "stats (Docker) / metrics (Kubernetes)"),
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
            ("B", "favorite targets"),
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

    fn lacks(keys: &[&str], absent: &[&str]) -> bool {
        absent.iter().all(|key| !keys.contains(key))
    }

    /// The states of the views that the key tests walk through, by name.
    fn views() -> Vec<(&'static str, State)> {
        let mut views = Vec::new();
        let mut state = State::new(Request::default());
        state.native = Some(crate::native::parse("ps", &state).unwrap());
        views.push(("containers, Docker not ready", state));
        let mut state = State::new(Request::default());
        state.runtime = serde_json::json!({"dockerStatus": "ready"});
        state.native = Some(crate::native::parse("ps", &state).unwrap());
        state.data = serde_json::json!([{"ID": "exited-id", "State": "exited"}]);
        views.push(("containers", state));
        let mut state = State::new(Request::default());
        state.docker_context = Some("remote".into());
        state.native = Some(crate::native::parse("images", &state).unwrap());
        views.push(("images", state));
        for (name, command) in [
            ("pods", "get pods"),
            ("deployments", "get deployments"),
            ("services", "get svc"),
            ("configmaps", "get configmaps"),
        ] {
            let mut state = State::new(Request::default());
            state.workspace = Workspace::Kubernetes;
            state.native = Some(crate::native::parse(command, &state).unwrap());
            views.push((name, state));
        }
        let mut panel = State::new(Request::default());
        panel.view("vm").unwrap();
        views.push(("VM panel", panel));
        views
    }

    #[test]
    fn hints_follow_the_view_and_offer_only_keys_it_handles() {
        let views = views();
        let view = |name: &str| &views.iter().find(|view| view.0 == name).unwrap().1;
        // A Hamn profile whose Docker is not ready: s starts the VM.
        let state = view("containers, Docker not ready");
        let containers = keys(state);
        for key in [
            ":", "/", "Enter", "l", "s", "d", "y", "t", "r", "Ctrl+D", "g", "m", "e", "v", "a",
            "?", "q",
        ] {
            assert!(containers.contains(&key), "{key}: {containers:?}");
        }
        assert!(hints(state).contains(&("s", "start VM")));
        assert!(!hints(state).contains(&("s", "shell")));
        // Docker is ready: s opens a shell, y the JSON of `docker inspect`,
        // and t starts the selected container that has exited.
        let state = view("containers");
        for hint in [
            ("s", "shell"),
            ("y", "json"),
            ("t", "start"),
            ("Ctrl+D", "delete"),
        ] {
            assert!(hints(state).contains(&hint), "{hint:?}: {:?}", hints(state));
        }
        // A Docker context has no VM; images have no container keys.
        let images = keys(view("images"));
        for key in ["d", "y", "Ctrl+D", "m"] {
            assert!(images.contains(&key), "{key}: {images:?}");
        }
        assert!(
            lacks(&images, &["s", "t", "r", "l", "g", "v", "a", "F"]),
            "{images:?}"
        );
        let pods = keys(view("pods"));
        for key in ["n", "e", "b", "l", "s", "d", "y", "F", "Ctrl+D", "g"] {
            assert!(pods.contains(&key), "{key}: {pods:?}");
        }
        assert!(lacks(&pods, &["r", "t", "v", "a"]), "{pods:?}");
        assert!(hints(view("pods")).contains(&("y", "yaml")));
        let deployments = keys(view("deployments"));
        assert!(
            deployments.contains(&"r") && deployments.contains(&"F") && deployments.contains(&"l"),
            "{deployments:?}"
        );
        assert!(lacks(&deployments, &["s", "g"]), "{deployments:?}");
        let services = keys(view("services"));
        assert!(
            services.contains(&"F") && lacks(&services, &["l", "s", "r"]),
            "{services:?}"
        );
        let configmaps = keys(view("configmaps"));
        assert!(
            lacks(&configmaps, &["l", "s", "F", "r", "g", "t"]) && configmaps.contains(&"d"),
            "{configmaps:?}"
        );
        let mut state = State::new(Request::default());
        // Pickers select; details and help scroll and go back.
        state.environment_picker = true;
        assert_eq!(hints(&state)[0], ("Enter", "select"));
        state.environment_picker = false;
        state.detail = Some("text".into());
        assert_eq!(hints(&state)[0], ("Esc", "back"));
        state.help = true;
        assert_eq!(hints(&state)[0], ("Esc", "close help"));
        let panel = keys(view("VM panel"));
        assert!(
            panel.contains(&"c") && panel.contains(&"Ctrl+D") && !panel.contains(&"d"),
            "{panel:?}"
        );
    }

    fn press(key: &str) -> (KeyCode, KeyModifiers) {
        match key {
            "Ctrl+D" => (KeyCode::Char('d'), KeyModifiers::CONTROL),
            "F" => (KeyCode::Char('F'), KeyModifiers::SHIFT),
            key => (
                KeyCode::Char(key.chars().next().unwrap()),
                KeyModifiers::NONE,
            ),
        }
    }

    /// Every row key that a hint names is one the handler acts on in that
    /// view: a row action of the list's resource, or a Hamn action.
    #[test]
    fn every_hinted_row_key_reaches_an_action_the_list_offers() {
        for (name, state) in views() {
            for (key, label) in hints(&state) {
                let is_row_key = key == "Ctrl+D" || "lsdyFtrg".contains(key) && key.len() == 1;
                if !is_row_key {
                    continue;
                }
                let (code, modifiers) = press(key);
                let action = row_key(&state, code, modifiers);
                let offered = match (&action, &state.native) {
                    (Some(RowKey::Native(action)), Some(invocation)) => {
                        native_actions::offers(invocation, action)
                    }
                    (Some(RowKey::Shell), Some(invocation)) => {
                        native_actions::offers(invocation, "shell")
                    }
                    (Some(RowKey::PortForward), Some(invocation)) => {
                        native_actions::offers(invocation, "port-forward")
                    }
                    (Some(RowKey::Logs), Some(invocation)) => {
                        native_actions::offers(invocation, "logs")
                    }
                    (Some(RowKey::StartVm), _) => label == "start VM",
                    (Some(RowKey::Hamn(_)), None) => state.vm_panel(),
                    _ => false,
                };
                assert!(offered, "{name}: <{key}> {label} gives {action:?}");
            }
        }
    }

    #[test]
    fn row_keys_follow_k9s_and_ctrl_d_alone_deletes() {
        let views = views();
        let view = |name: &str| &views.iter().find(|view| view.0 == name).unwrap().1;
        let key = |state: &State, key: &str| {
            let (code, modifiers) = press(key);
            row_key(state, code, modifiers)
        };
        let pods = view("pods");
        for (pressed, expected) in [
            ("d", RowKey::Native("describe")),
            ("Ctrl+D", RowKey::Native("delete")),
            ("y", RowKey::Native("yaml")),
            ("s", RowKey::Shell),
            ("F", RowKey::PortForward),
            ("l", RowKey::Logs),
            ("r", RowKey::Native("restart")),
            ("g", RowKey::Native("stats")),
        ] {
            assert_eq!(key(pods, pressed), Some(expected), "{pressed}");
        }
        // Other modifiers make no row key, so Ctrl+S or Alt+D do nothing.
        assert_eq!(
            row_key(pods, KeyCode::Char('s'), KeyModifiers::CONTROL),
            None
        );
        assert_eq!(row_key(pods, KeyCode::Char('d'), KeyModifiers::ALT), None);
        assert_eq!(
            row_key(
                pods,
                KeyCode::Char('d'),
                KeyModifiers::CONTROL | KeyModifiers::ALT
            ),
            None
        );
        assert_eq!(key(pods, "f"), None);
        // t follows the selected container: an exited one starts, any
        // other stops; without a row it stops, which then fails to select.
        let mut containers = State::new(Request::default());
        containers.runtime = serde_json::json!({"dockerStatus": "ready"});
        containers.native = Some(crate::native::parse("ps", &containers).unwrap());
        for (state, expected) in [
            ("created", "start"),
            ("exited", "start"),
            ("running", "stop"),
            ("restarting", "stop"),
            ("paused", "stop"),
        ] {
            containers.data = serde_json::json!([{"ID": "id", "State": state}]);
            assert_eq!(
                key(&containers, "t"),
                Some(RowKey::Native(expected)),
                "{state}"
            );
        }
        containers.data = serde_json::Value::Null;
        assert_eq!(key(&containers, "t"), Some(RowKey::Native("stop")));
        assert_eq!(key(&containers, "s"), Some(RowKey::Shell));
        assert_eq!(
            key(view("containers, Docker not ready"), "s"),
            Some(RowKey::StartVm)
        );
        // The VM panel deletes only with Ctrl+D; d shows the detail there.
        let panel = view("VM panel");
        assert_eq!(key(panel, "Ctrl+D"), Some(RowKey::Hamn("delete")));
        assert_eq!(key(panel, "d"), Some(RowKey::Detail));
        assert_eq!(key(panel, "s"), Some(RowKey::Hamn("start")));
        assert_eq!(key(panel, "y"), None);
        // A picker has no detail to show for d.
        let mut picker = State::new(Request::default());
        picker.environment_picker = true;
        assert_eq!(key(&picker, "d"), None);
        assert_eq!(key(&picker, "F"), None);
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
