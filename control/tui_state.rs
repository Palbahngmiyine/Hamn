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
        }
        self.selected = 0;
        self.filter.clear();
        self.detail = None;
        Ok(request)
    }
    pub fn action(&self, action: &str) -> Result<Request> {
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
        request.follow = false;
        request.all_namespaces = false;
        request.validate()?;
        Ok(request)
    }
    pub fn accept(&mut self, result: Result<Value>) {
        self.loading = false;
        match result {
            Ok(value) if value.is_array() => {
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
    let status = value["state"]
        .as_str()
        .or_else(|| value["State"].as_str())
        .or_else(|| value["status"]["phase"].as_str())
        .unwrap_or("");
    clean(&format!("{name}  {status}"))
}

pub fn draw(frame: &mut Frame, state: &State) {
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
    let message = if let Some(pending) = &state.pending {
        format!(
            "Confirm {} on {}? y = execute, Esc = cancel",
            pending.operation(),
            pending.target()
        )
    } else if let Some((prefix, input)) = &state.input {
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
