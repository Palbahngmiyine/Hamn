use crate::model::{Failure, Request, Result};
use clap::Parser;
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Style},
    widgets::{Block, List, ListItem, ListState, Paragraph, Wrap},
};
use serde_json::Value;

pub struct State {
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
}

impl State {
    pub fn new(mut request: Request) -> Self {
        request.words = vec!["vm".into(), "list".into()];
        request.profile.get_or_insert("default".into());
        request.headless = false;
        Self {
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
        }
    }
    pub fn rows(&self) -> Vec<&Value> {
        self.data
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter(|row| {
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
        let mut request =
            Request::try_parse_from(std::iter::once("hamn").chain(words.split_whitespace()))
                .map_err(|e| Failure::new("invalidRequest", e))?;
        request.normalize()?;
        request.profile = request.profile.or_else(|| self.request.profile.clone());
        request.context = request.context.or_else(|| self.request.context.clone());
        request.namespace = request.namespace.or_else(|| self.request.namespace.clone());
        request.kubeconfig = request
            .kubeconfig
            .or_else(|| self.request.kubeconfig.clone());
        request.yes = true; // UI confirmation is mandatory before dispatch.
        request.validate()?;
        if !request.mutates() {
            self.request = request.clone();
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
    clean(&format!("{name}  {status}"))
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
            text.push_str(&format!("{key}: {value}\n"));
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
    let areas = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .split(frame.area());
    let header = format!(
        "Hamn  |  profile: {}  |  context: {}  |  namespace: {}",
        state.request.profile.as_deref().unwrap_or("-"),
        state.request.context.as_deref().unwrap_or("-"),
        state.request.namespace.as_deref().unwrap_or("default")
    );
    frame.render_widget(
        Paragraph::new(clean(&header)).block(Block::bordered()),
        areas[0],
    );
    let title = format!(
        "{} {}",
        state.request.operation(),
        if state.loading { "[loading]" } else { "" }
    );
    if let Some(detail) = &state.detail {
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

#[cfg(test)]
mod tests {
    use super::*;
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
        state.data = serde_json::json!([{"name":"작업"},{"name":"other"}]);
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
