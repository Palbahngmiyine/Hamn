use crate::preferences::Workspace;
use crate::model::{Failure, Request, Result};
use clap::Parser;
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Style},
    widgets::{Block, List, ListItem, ListState, Paragraph, Wrap},
};
use serde_json::Value;

fn split_command(command: &str) -> Result<Vec<String>> {
    let (mut words, mut word, mut quote, mut escaped, mut started) =
        (Vec::new(), String::new(), None, false, false);
    for c in command.chars() {
        if escaped {
            word.push(c);
            escaped = false;
        } else if c == '\\' && quote != Some('\'') {
            escaped = true;
            started = true;
        } else if quote == Some(c) {
            quote = None;
        } else if quote.is_none() && matches!(c, '\'' | '"') {
            quote = Some(c);
            started = true;
        } else if quote.is_none() && c.is_whitespace() {
            if started {
                words.push(std::mem::take(&mut word));
                started = false;
            }
        } else {
            word.push(c);
            started = true;
        }
    }
    if escaped || quote.is_some() {
        return Err(Failure::new(
            "invalidRequest",
            "unfinished quote or escape in command",
        ));
    }
    if started {
        words.push(word);
    }
    Ok(words)
}

pub struct State {
    pub workspace: Workspace,
    pub docker_context: Option<String>,
    pub show_all: bool,
    pub request: Request,
    pub data: Value,
    pub selected: usize,
    pub filter: String,
    pub input: Option<(char, String)>,
    pub message: String,
    pub detail: Option<String>,
    pub pending: Option<Request>,
    pub loading: bool,
    pub scroll: u16,
    pub stale: bool,
    pub uncertain: Vec<Value>,
    pub quit_confirmation: bool,
    pub operation_status: String,
    pub operation_log: String,
    pub show_operation: bool,
}

impl State {
    pub fn new(mut request: Request) -> Self {
        request.words = vec!["docker".into(), "containers".into(), "list".into()];
        request.profile.get_or_insert("default".into());
        request.headless = false;
        Self {
            workspace: Workspace::Containers,
            docker_context: None,
            show_all: false,
            request,
            data: Value::Null,
            selected: 0,
            filter: String::new(),
            input: None,
            message: String::new(),
            detail: None,
            pending: None,
            loading: false,
            scroll: 0,
            stale: false,
            uncertain: Vec::new(),
            quit_confirmation: false,
            operation_status: String::new(),
            operation_log: String::new(),
            show_operation: false,
        }
    }
    pub fn for_workspace(request: Request, workspace: Workspace) -> Self {
        let mut state = Self::new(request);
        state.workspace = workspace;
        if workspace == Workspace::Kubernetes {
            state.request.profile = None;
            state.request.words = vec!["k8s".into(), "contexts".into(), "list".into()];
            if let Ok(config) = crate::kubeconfig::load(&state.request) {
                state.request.context = state.request.context.or(config.current_context);
                if let Some(context) = config.contexts.iter().find(|c| Some(&c.name) == state.request.context.as_ref()).and_then(|c| c.context.as_ref()) {
                    state.request.namespace = state.request.namespace.or(context.namespace.clone());
                    state.request.words[1] = "pods".into();
                }
            }
        } else {
            state.request.context = None;
            state.request.namespace = None;
        }
        state
    }
    pub fn rows(&self) -> Vec<&Value> {
        self.data
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter(|row| {
                        (self.workspace != Workspace::Containers || self.show_all ||
                         self.request.operation() != "docker containers list" || row["State"] == "running") &&
                        label(row)
                            .to_lowercase()
                            .contains(&self.filter.to_lowercase())
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn selected(&self) -> Option<Value> {
        self.rows().get(self.selected).map(|v| (*v).clone())
    }
    pub fn move_by(&mut self, delta: isize) {
        if self.detail.is_some() {
            self.scroll = self
                .scroll
                .saturating_add_signed(delta.clamp(-32768, 32767) as i16);
            return;
        }
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.rows().len().saturating_sub(1));
    }
    pub fn view(&mut self, command: &str) -> Result<Request> {
        let words = match command {
            "vm" => "vm list",
            "containers" => "docker containers list",
            "images" => "docker images list",
            "volumes" => "docker volumes list",
            "networks" => "docker networks list",
            "contexts" | "ctx" => "k8s contexts list",
            "ns" | "namespaces" => "k8s namespaces list",
            "pods" | "po" => "k8s pods list",
            "deployments" | "deploy" => "k8s deployments list",
            "sts" => "k8s statefulsets list",
            "ds" => "k8s daemonsets list",
            "services" | "svc" => "k8s services list",
            "nodes" | "no" => "k8s nodes list",
            "events" => "k8s events list",
            "jobs" => "k8s jobs list",
            "cronjobs" => "k8s cronjobs list",
            "ingresses" => "k8s ingresses list",
            "pvcs" => "k8s pvcs list",
            _ => command,
        };
        let mut request = Request::try_parse_from(
            std::iter::once("hamn".to_owned()).chain(split_command(words)?),
        )
        .map_err(|e| Failure::new("invalidRequest", e))?;
        request.normalize()?;
        let context_changed = request
            .context
            .as_ref()
            .is_some_and(|value| Some(value) != self.request.context.as_ref())
            || request
                .kubeconfig
                .as_ref()
                .is_some_and(|value| Some(value) != self.request.kubeconfig.as_ref());
        request.profile = request.profile.or_else(|| self.request.profile.clone());
        request.context = request.context.or_else(|| self.request.context.clone());
        if !context_changed {
            request.namespace = request.namespace.or_else(|| self.request.namespace.clone());
        }
        request.kubeconfig = request
            .kubeconfig
            .or_else(|| self.request.kubeconfig.clone());
        if context_changed
            && request.namespace.is_none()
            && request.words.first().is_some_and(|word| word == "k8s")
        {
            let config = crate::kubeconfig::load(&request)?;
            let context = config
                .contexts
                .iter()
                .find(|entry| Some(&entry.name) == request.context.as_ref())
                .and_then(|entry| entry.context.as_ref())
                .ok_or_else(|| {
                    Failure::new("contextNotFound", "selected context is unavailable")
                })?;
            request.namespace = Some(
                context
                    .namespace
                    .clone()
                    .unwrap_or_else(|| "default".into()),
            );
        }
        request.yes = true; // UI confirmation is mandatory before dispatch.
        request.validate()?;
        if !request.mutates() {
            self.request = request.clone();
            // Detail commands dispatch once; Esc and periodic refresh must
            // return to the resource list, just like keyboard detail actions.
            if self.request.words.last().is_some_and(|word| word != "list") {
                *self.request.words.last_mut().unwrap() = "list".into();
                self.request.name = None;
                self.request.uid = None;
                self.request.container = None;
                self.request.previous = false;
                self.request.follow = false;
                self.request.watch = false;
            }
            self.data = Value::Null;
            self.stale = false;
        }
        self.selected = 0;
        self.filter.clear();
        self.detail = None;
        self.scroll = 0;
        Ok(request)
    }
    pub fn action(&self, action: &str) -> Result<Request> {
        if self.stale {
            return Err(Failure::new(
                "staleData",
                "refresh this view before acting on previous data",
            ));
        }
        let row = self
            .selected()
            .ok_or_else(|| Failure::new("noSelection", "select a resource first"))?;
        let mut request = self.request.clone();
        *request.words.last_mut().unwrap() = action.into();
        request.yes = true;
        if request.words[0] == "vm" {
            request.profile = row["name"].as_str().map(String::from);
        } else {
            request.name = row["Id"]
                .as_str()
                .or_else(|| row["metadata"]["name"].as_str())
                .map(String::from);
            request.uid = row["metadata"]["uid"].as_str().map(String::from);
            if let Some(namespace) = row["metadata"]["namespace"].as_str() {
                request.namespace = Some(namespace.into());
            }
        }
        request.watch = false;
        request.follow = action == "logs";
        request.all_namespaces = false;
        request.validate()?;
        Ok(request)
    }
    pub fn accept(&mut self, result: Result<Value>) {
        if let Ok(value) = &result {
            if value["type"] == "log" {
                let text = self.detail.get_or_insert_with(String::new);
                text.push_str(value["text"].as_str().unwrap_or_default());
                if text.len() > 1024 * 1024 {
                    let mut split = text.len() - 1024 * 1024;
                    while !text.is_char_boundary(split) {
                        split += 1;
                    }
                    text.drain(..split);
                }
                return;
            }
            if value["ended"] == true {
                self.loading = false;
                self.message = "log stream ended".into();
                return;
            }
        }
        self.loading = false;
        match result {
            Ok(value) if value.is_array() => {
                self.stale = false;
                self.data = value;
                self.move_by(0);
                self.message.clear();
            }
            Ok(value) => {
                self.detail = Some(
                    value["yaml"]
                        .as_str()
                        .map(String::from)
                        .unwrap_or_else(|| serde_json::to_string_pretty(&value).unwrap()),
                );
                self.message.clear();
            }
            Err(error) => {
                self.stale = true;
                self.message = format!(
                    "{}: {} (previous data may be stale)",
                    error.code, error.message
                )
            }
        }
    }
    pub fn enter(&mut self) -> Result<Option<Request>> {
        let Some(row) = self.selected() else {
            return Ok(None);
        };
        match self.request.operation().as_str() {
            "k8s contexts list" => {
                if row["available"] == false {
                    return Err(Failure::new(
                        "managedK3sRemoved",
                        "legacy Hamn context is unavailable",
                    ));
                }
                self.request.context = row["name"].as_str().map(String::from);
                self.request.namespace = row["namespace"].as_str().map(String::from);
                self.view("pods").map(Some)
            }
            "k8s namespaces list" => {
                self.request.namespace = row["metadata"]["name"].as_str().map(String::from);
                self.view("pods").map(Some)
            }
            "vm list" => {
                self.request.profile = row["name"].as_str().map(String::from);
                self.detail = Some(serde_json::to_string_pretty(&row).unwrap());
                Ok(None)
            }
            "docker images list" | "docker volumes list" | "docker networks list" => {
                self.detail = Some(serde_json::to_string_pretty(&row).unwrap());
                Ok(None)
            }
            _ => self.action("inspect").map(Some),
        }
    }
}

pub fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

fn label(value: &Value) -> String {
    let name = value["name"]
        .as_str()
        .or_else(|| value["metadata"]["name"].as_str())
        .or_else(|| value["Name"].as_str())
        .or_else(|| value["Names"][0].as_str())
        .or_else(|| value["RepoTags"][0].as_str())
        .or_else(|| value["Id"].as_str())
        .unwrap_or("-");
    let status = value["reason"]
        .as_str()
        .or_else(|| {
            value["state"]
                .as_str()
                .or_else(|| value["State"].as_str())
                .or_else(|| value["status"]["phase"].as_str())
        })
        .unwrap_or("");
    let docker = value["dockerStatus"].as_str().unwrap_or("");
    clean(&format!("{name}  {status}  {docker}"))
}

fn confirmation(request: &Request, width: u16) -> Vec<String> {
    let mut text = format!("Confirm {}\n", request.operation());
    for (key, value) in [
        ("Profile", &request.profile),
        ("Context", &request.context),
        ("Namespace", &request.namespace),
        ("Name", &request.name),
        ("UID", &request.uid),
        ("Kubeconfig", &request.kubeconfig),
        ("Output path", &request.path),
        ("Manifest", &request.manifest),
    ] {
        if let Some(value) = value {
            text.push_str(&format!("{key}: {value:?}\n"));
        }
    }
    for (key, value) in [
        ("CPU", request.cpu),
        ("Memory GiB", request.memory),
        ("Disk GiB", request.disk),
    ] {
        if let Some(value) = value {
            text.push_str(&format!("{key}: {value}\n"));
        }
    }
    text.push_str(&format!(
        "\nImpact: {}\n\ny = execute; Esc / n = cancel",
        request.impact()
    ));
    if request.mutates()
        && matches!(
            request.words.first().map(String::as_str),
            Some("vm" | "docker")
        )
        && request.operation() != "vm create"
    {
        text.push_str("\nPending legacy K3s retirement permanently deletes its cluster data and local volumes before this operation. Docker data is preserved.");
    }
    wrap_lines(&text, width)
}

fn wrap_lines(text: &str, width: u16) -> Vec<String> {
    let mut lines = Vec::new();
    for line in clean(&text).lines() {
        let mut current = String::new();
        let mut columns = 0;
        for c in line.chars() {
            let size = ratatui::text::Line::raw(c.to_string()).width();
            if columns + size > usize::from(width) && !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                columns = 0;
            }
            current.push(c);
            columns += size;
        }
        lines.push(current);
    }
    lines
}

pub fn confirmation_visible(request: &Request, area: ratatui::layout::Rect) -> bool {
    area.width >= 20
        && area.height >= 8
        && confirmation(request, area.width - 2).len() <= usize::from(area.height - 2)
}

pub fn draw(frame: &mut Frame, state: &State) {
    if state.quit_confirmation {
        frame.render_widget(Paragraph::new("Cancel the active operation and exit?\nHamn will wait for rollback and resource cleanup.\ny = cancel then exit; n / Esc = keep working")
            .wrap(Wrap { trim: false }).block(Block::bordered().title("Active operation")), frame.area());
        return;
    }
    if let Some(pending) = &state.pending {
        let area = frame.area();
        let text = if confirmation_visible(pending, area) {
            confirmation(pending, area.width - 2).join("\n")
        } else {
            "Resize terminal to review the full target and impact.\nExecution disabled. Esc cancels.".into()
        };
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .block(Block::bordered().title("Hamn confirmation")),
            area,
        );
        return;
    }
    let header = match state.workspace {
        Workspace::Containers => format!("Hamn | [Containers]   Kubernetes   Tab switch   , settings\nEnvironment: {}\n{}",
            state.docker_context.as_ref().map(|c| format!("Docker context {c}")).unwrap_or_else(|| format!("Hamn profile {}", state.request.profile.as_deref().unwrap_or("default"))),
            if state.docker_context.is_none() { "e environments   v VM settings   a running/all" } else { "e environments   a running/all" }),
        Workspace::Kubernetes => format!("Hamn | Containers   [Kubernetes]   Tab switch   , settings\nContext: {}   Namespace: {}\ne contexts   n namespaces",
            state.request.context.as_deref().unwrap_or("choose a context"),
            if state.request.all_namespaces { "all namespaces" } else { state.request.namespace.as_deref().unwrap_or("default") }),
    };
    let header = if state.uncertain.is_empty() {
        header
    } else {
        format!(
            "{header}\noutcomeUnknown: {} operation(s). ! shows targets; inspect before retry.",
            state.uncertain.len()
        )
    };
    let header = format!("{header}\n{}", state.operation_status);
    let header = wrap_lines(&header, frame.area().width.saturating_sub(2));
    let header_height = header.len().saturating_add(2).min(u16::MAX as usize) as u16;
    if frame.area().width < 20 || header_height.saturating_add(6) > frame.area().height {
        frame.render_widget(
            Paragraph::new("Hamn: resize terminal to show the selected target. q exits.")
                .wrap(Wrap { trim: false }),
            frame.area(),
        );
        return;
    }
    let areas = Layout::vertical([
        Constraint::Length(header_height),
        Constraint::Min(3),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .split(frame.area());
    frame.render_widget(
        Paragraph::new(header.join("\n")).block(Block::bordered()),
        areas[0],
    );
    let title = format!(
        "{} {}",
        state.request.operation(),
        if state.loading { "[loading]" } else { "" }
    );
    let operation_detail = format!("{}\n{}\n{}", state.operation_status, state.operation_log, serde_json::to_string_pretty(&state.uncertain).unwrap());
    if let Some(detail) = if state.show_operation { Some(&operation_detail) } else { state.detail.as_ref() } {
        frame.render_widget(
            Paragraph::new(clean(detail))
                .scroll((state.scroll, 0))
                .wrap(Wrap { trim: false })
                .block(Block::bordered().title(title)),
            areas[1],
        );
    } else {
        let rows: Vec<_> = state
            .rows()
            .into_iter()
            .map(|row| ListItem::new(label(row)))
            .collect();
        frame.render_stateful_widget(
            List::new(rows)
                .block(Block::bordered().title(title))
                .highlight_style(Style::default().fg(Color::Cyan))
                .highlight_symbol("> "),
            areas[1],
            &mut ListState::default().with_selected(Some(state.selected)),
        );
    }
    let message = if let Some((prefix, input)) = &state.input {
        format!("{prefix}{input}")
    } else {
        state.message.clone()
    };
    frame.render_widget(
        Paragraph::new(clean(&message))
            .wrap(Wrap { trim: false })
            .block(Block::bordered()),
        areas[2],
    );
    frame.render_widget(Paragraph::new(": command  / filter  Enter detail  s start  t stop  r restart  d delete  l logs  g stats  ? help  q quit"), areas[3]);
}

pub fn draw_choice(frame: &mut Frame, selected: usize, settings: bool, error: &str) {
    let text = format!("{}\n\n{} Containers — Docker containers, images, volumes and networks\n{} Kubernetes — resources in your kubeconfig context\n\n1 / 2 or arrows to select, Enter to save\n{}\n\n{}",
        if settings { "Choose the workspace to open by default" } else { "Choose your default workspace" },
        if selected == 0 { ">" } else { " " }, if selected == 1 { ">" } else { " " },
        if settings { "Esc returns without changing the default" } else { "q exits" }, clean(error));
    frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false })
        .block(Block::bordered().title("Hamn settings")), frame.area());
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workspaces_keep_selection_and_do_not_show_vm_controls_in_kubernetes() {
        let mut containers = State::for_workspace(Request::default(), Workspace::Containers);
        containers.data = serde_json::json!([{"Names":["running"],"State":"running"}, {"Names":["stopped"],"State":"exited"}]);
        assert_eq!(containers.rows().len(), 1);
        containers.show_all = true;
        containers.selected = 1;
        let mut kubernetes = State::for_workspace(Request { kubeconfig: Some("/no-such-hamn-test-config".into()), ..Default::default() }, Workspace::Kubernetes);
        assert_eq!(kubernetes.request.operation(), "k8s contexts list");
        assert!(kubernetes.request.profile.is_none());
        std::mem::swap(&mut containers, &mut kubernetes);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 24)).unwrap();
        terminal.draw(|f| draw(f, &containers)).unwrap();
        let text: String = terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect();
        assert!(text.contains("[Kubernetes]"));
        assert!(!text.contains("VM settings") && !text.contains("Hamn profile"));
        std::mem::swap(&mut containers, &mut kubernetes);
        assert_eq!(containers.selected, 1);
        assert_eq!(containers.rows().len(), 2);
    }

    #[test]
    fn direct_details_keep_a_clean_list_request_for_escape_and_refresh() {
        for command in [
            "vm status", "vm env", "docker containers inspect sample",
            "docker containers stats sample", "docker containers logs sample --follow",
            "k8s pods inspect sample --uid original",
            "k8s pods logs sample --follow --container app --previous",
        ] {
            let mut state = State::new(Request {
                context: Some("dev".into()), namespace: Some("work".into()),
                ..Default::default()
            });
            let dispatched = state.view(command).unwrap();
            assert_ne!(dispatched.words.last().unwrap(), "list");
            assert_eq!(state.request.words.last().unwrap(), "list");
            assert_eq!(state.request.profile, dispatched.profile);
            assert_eq!(state.request.context, dispatched.context);
            assert_eq!(state.request.namespace, dispatched.namespace);
            assert!(state.request.name.is_none() && state.request.uid.is_none());
            assert!(state.request.container.is_none());
            assert!(!state.request.follow && !state.request.watch && !state.request.previous);
            state.request.validate().unwrap();
            state.accept(Ok(serde_json::json!({"yaml":"detail"})));
            state.detail = None; // Esc clears the detail before the next refresh.
            state.accept(Ok(serde_json::json!([{"metadata":{"name":"sample", "uid":"original", "namespace":"work"}, "Id":"sample", "name":"default", "State":"running"}])));
            assert_eq!(state.rows().len(), 1);
            assert!(state.action(if state.request.words[0] == "vm" {"status"} else {"inspect"}).is_ok());
        }
    }

    #[test]
    fn switching_context_uses_its_namespace_unless_explicitly_overridden() {
        struct Fixture(std::path::PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let path =
            std::env::temp_dir().join(format!("hamn-tui-context-{}.json", std::process::id()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let _fixture = Fixture(path.clone());
        use std::io::Write;
        file.write_all(br#"{"apiVersion":"v1","kind":"Config","contexts":[{"name":"new","context":{"cluster":"cluster","namespace":"new-ns"}}]}"#).unwrap();
        let mut state = State::new(Request {
            context: Some("old".into()),
            namespace: Some("old-ns".into()),
            kubeconfig: Some(path.to_str().unwrap().into()),
            ..Default::default()
        });
        let request = state.view("k8s pods list --context new").unwrap();
        assert_eq!(request.namespace.as_deref(), Some("new-ns"));
        let request = state.view("k8s pods list --namespace explicit").unwrap();
        assert_eq!(request.namespace.as_deref(), Some("explicit"));
        let previous = state.request.context.clone();
        assert!(state.view("k8s pods list --context missing").is_err());
        assert_eq!(state.request.context, previous);
    }

    #[test]
    fn command_paths_support_quotes_without_shell_expansion() {
        let mut state = State::new(Request::default());
        let request = state
            .view("vm diagnostics --path '/tmp/한글 report.tar'")
            .unwrap();
        assert_eq!(request.path.as_deref(), Some("/tmp/한글 report.tar"));
        assert_eq!(
            split_command(r#"one "two three" 'four\\five' $HOME $(command)"#).unwrap(),
            ["one", "two three", "four\\\\five", "$HOME", "$(command)"]
        );
        assert!(split_command("'unfinished").is_err());
        assert!(split_command("unfinished\\").is_err());
        assert_eq!(split_command("''").unwrap(), [""]);
    }
    #[test]
    fn confirmation_requires_complete_target_and_impact_to_fit() {
        let mut request = Request::try_parse_from([
            "hamn",
            "k8s",
            "pods",
            "delete",
            "api",
            "--context",
            "dev",
            "--namespace",
            "default",
            "--yes",
        ])
        .unwrap();
        request.normalize().unwrap();
        let area = ratatui::layout::Rect::new(0, 0, 100, 24);
        assert!(confirmation_visible(&request, area));
        assert!(!confirmation_visible(
            &request,
            ratatui::layout::Rect::new(0, 0, 20, 8)
        ));
        request.context = Some("long-context-".repeat(300));
        assert!(!confirmation_visible(&request, area));
        let mut state = State::new(Request::default());
        state.pending = Some(request);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &state)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Execution disabled"));
    }
    #[test]
    fn failed_refresh_blocks_actions_and_view_switch_clears_old_rows() {
        let mut state = State::new(Request::default());
        state.accept(Ok(serde_json::json!([{"name":"work"}])));
        state.accept(Err(Failure::new("connection", "disconnected")));
        assert_eq!(state.action("stop").unwrap_err().code, "staleData");
        state.view("containers").unwrap();
        assert!(state.rows().is_empty());
        assert!(!state.stale);
    }
    #[test]
    fn renders_small_terminal_and_filters_unicode_without_panicking() {
        let mut state = State::new(Request::try_parse_from(["hamn"]).unwrap());
        state.data = serde_json::json!([{"name":"작업", "State":"running"},{"name":"other", "State":"running"}]);
        state.filter = "작".into();
        assert_eq!(state.rows().len(), 1);
        state.move_by(100);
        assert_eq!(state.selected, 0);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(30, 10)).unwrap();
        terminal.draw(|frame| draw(frame, &state)).unwrap();
        assert!(!clean("hello\x1b[2J").contains('\x1b'));
    }
}
