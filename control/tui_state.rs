use crate::model::{Failure, Request, Result};
use crate::preferences::Workspace;
use crate::tui_failure::Level;
use clap::Parser;
use crate::tui_theme as theme;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, List, ListItem, ListState, Paragraph, Wrap},
};
use serde_json::Value;

/// The prompt of `State::input` that takes the ports of `State::forward`.
pub const PORT_FORWARD: char = 'F';

// Tokenize shell-style quotes without performing expansions or executing a shell.
// In double quotes, backslash is literal except before $ ` " \ and newline.
pub fn split_command(command: &str) -> Result<Vec<String>> {
    let (mut words, mut word, mut quote, mut escaped, mut started) =
        (Vec::new(), String::new(), None, false, false);
    for c in command.chars() {
        if escaped {
            if c != '\n' {
                if quote == Some('"') && !matches!(c, '$' | '`' | '"' | '\\') {
                    word.push('\\');
                }
                word.push(c);
                started = true;
            }
            escaped = false;
        } else if c == '\\' && quote != Some('\'') {
            escaped = true;
        } else if quote == Some(c) {
            quote = None;
        } else if quote.is_none() && matches!(c, '\'' | '"') {
            quote = Some(c);
            started = true;
        } else if quote.is_none() && matches!(c, ' ' | '\t' | '\n') {
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

/// Refresh scheduling uses monotonic time; failures back off to at most 60s.
/// Pausing affects automatic reads only. Manual refresh and navigation still work.
pub struct Refresh {
    pub interval: std::time::Duration,
    pub timeout: std::time::Duration,
    pub paused: bool,
    pub failures: u32,
    pub next: std::time::Instant,
    pub last_success: Option<jiff::Timestamp>,
}
impl Default for Refresh {
    fn default() -> Self {
        Self {
            interval: std::time::Duration::from_secs(2),
            timeout: std::time::Duration::from_secs(30),
            paused: false,
            failures: 0,
            next: std::time::Instant::now(),
            last_success: None,
        }
    }
}
impl Refresh {
    pub fn due(&self, now: std::time::Instant) -> bool {
        !self.paused && now >= self.next
    }
    pub fn complete(&mut self, success: bool, now: std::time::Instant) {
        if success {
            self.failures = 0;
            self.last_success = Some(jiff::Timestamp::now());
        } else {
            self.failures = self.failures.saturating_add(1);
        }
        self.next = now
            + self
                .interval
                .saturating_mul(1u32 << self.failures.min(5))
                .min(std::time::Duration::from_secs(60));
    }
    /// A Hamn VM that is not ready yet is an observed state, not a failed
    /// query. Poll at the normal interval without backoff, so rows appear
    /// soon after the VM starts; last OK still refers to real rows.
    pub fn wait(&mut self, now: std::time::Instant) {
        self.failures = 0;
        self.next = now + self.interval;
    }
    pub fn label(&self) -> String {
        // Whole seconds keep the title width fixed across refreshes.
        let printer = jiff::fmt::temporal::DateTimePrinter::new().precision(Some(0));
        format!(
            "{} {}s | timeout {}s | last OK {}{}",
            if self.paused { "Paused" } else { "Refresh" },
            self.interval.as_secs(),
            self.timeout.as_secs(),
            self.last_success
                .map_or("never".into(), |time| printer.timestamp_to_string(&time)),
            if self.failures > 0 {
                format!(" | retry backoff ({})", self.failures)
            } else {
                String::new()
            }
        )
    }
}

/// The failure code of a Hamn profile Docker query while its VM is not ready.
pub const VM_NOT_READY: &str = "vmNotReady";

/// A Hamn profile's Docker socket answers only once its VM runs and guest
/// Docker is ready. The Docker CLI proves that it could not reach the daemon
/// (`DOCKER_UNREACHABLE`); the VM status observed by the same refresh then
/// explains why and names the key that starts or repairs the VM. Status alone
/// is no proof: recoveryRequired comes from the operation record without a
/// Docker check. Every other failure, such as a usage error, an unknown
/// command or the output limit, keeps its message because starting the VM
/// cannot fix it. Without an observed status (the profile could not be read),
/// or while Docker reports ready, the failure also stays unchanged.
pub fn hamn_query_failure(profile: &str, status: Option<&Value>, error: Failure) -> Failure {
    let Some(status) = status else {
        return error;
    };
    let docker = status["dockerStatus"].as_str().unwrap_or("unavailable");
    if error.code != crate::native::DOCKER_UNREACHABLE || docker == "ready" {
        return error;
    }
    let vm = status["state"].as_str().unwrap_or("unknown");
    let message = match (vm, docker) {
        (_, "recoveryRequired") => {
            format!("Hamn VM {profile} needs recovery. Press s to repair it.")
        }
        ("starting", _) | (_, "preparing") => {
            format!("Hamn VM {profile} is starting; the list appears when Docker is ready.")
        }
        ("running", _) => {
            format!("Docker is unavailable in Hamn VM {profile}. Press s to repair it.")
        }
        _ => format!(
            "Hamn VM {profile} is not running ({vm}). Press s to start it; the list appears when Docker is ready."
        ),
    };
    Failure::new(VM_NOT_READY, message)
}

struct Browser {
    native: Option<crate::native::Invocation>,
    data: Value,
    selected: Option<usize>,
    filter: String,
    scroll: u16,
    request: Request,
    environment_picker: bool,
    detail: Option<String>,
    connection_status: String,
    stale: bool,
    show_operation: bool,
}
pub struct State {
    browser: Option<Browser>,
    picker_return: Option<Browser>,
    pub workspace: Workspace,
    pub docker_context: Option<String>,
    pub docker_config: Option<String>,
    pub show_all: bool,
    pub native: Option<crate::native::Invocation>,
    pub environment_picker: bool,
    pub connection_status: String,
    pub runtime: Value,
    pub request: Request,
    pub data: Value,
    pub selected: Option<usize>,
    pub filter: String,
    /// The prompt line: `:` a command, `/` a filter, or [`PORT_FORWARD`]
    /// the ports of `forward`.
    pub input: Option<(char, String)>,
    /// The list and row whose port-forward waits for its ports. Set exactly
    /// while `input` is the [`PORT_FORWARD`] prompt: the row is kept as it
    /// was chosen, so a refresh meanwhile cannot change the target.
    pub forward: Option<(crate::native::Invocation, Value)>,
    pub history: Vec<String>,
    pub history_index: Option<usize>,
    /// The one-line message under the view. Written only through `notify`,
    /// `warn`, `fail` and `clear_message`, which keep it, its level and
    /// the failure behind it consistent.
    pub message: String,
    pub message_level: Level,
    /// What helps next, shown after the message.
    pub message_hint: Option<&'static str>,
    /// The failure that `message` summarizes, kept whole for the
    /// diagnostics view (`!`): the summary never replaces the raw text.
    pub last_error: Option<Failure>,
    pub detail: Option<String>,
    pub pending: Option<Request>,
    pub pending_native: Option<crate::native_actions::Action>,
    /// A read that establishes or replaces the view is running; the title
    /// shows it and actions wait for its rows.
    pub loading: bool,
    /// An automatic refresh re-reads the displayed view. The screen and
    /// actions keep the current rows until its result arrives, so an
    /// unchanged result redraws nothing.
    pub refreshing: bool,
    pub scroll: u16,
    pub stale: bool,
    pub uncertain: Vec<Value>,
    pub quit_confirmation: bool,
    pub refresh: Refresh,
    pub operation_status: String,
    pub operation_log: String,
    pub show_operation: bool,
    /// The help screen covers the view; keys keep their meaning.
    pub help: bool,
}

impl State {
    pub fn new(mut request: Request) -> Self {
        request.words = vec!["docker".into(), "containers".into(), "list".into()];
        request.profile.get_or_insert("default".into());
        request.headless = false;
        Self {
            browser: None,
            picker_return: None,
            workspace: Workspace::Containers,
            docker_context: None,
            docker_config: None,
            show_all: false,
            native: None,
            environment_picker: false,
            connection_status: String::new(),
            runtime: Value::Null,
            request,
            data: Value::Null,
            selected: Some(0),
            filter: String::new(),
            input: None,
            forward: None,
            history: Vec::new(),
            history_index: None,
            message: String::new(),
            message_level: Level::Info,
            message_hint: None,
            last_error: None,
            detail: None,
            pending: None,
            pending_native: None,
            loading: false,
            refreshing: false,
            scroll: 0,
            stale: false,
            uncertain: Vec::new(),
            quit_confirmation: false,
            refresh: Refresh::default(),
            operation_status: String::new(),
            operation_log: String::new(),
            show_operation: false,
            help: false,
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
                if let Some(context) = config
                    .contexts
                    .iter()
                    .find(|c| Some(&c.name) == state.request.context.as_ref())
                    .and_then(|c| c.context.as_ref())
                {
                    if config.clusters.iter().any(|entry| {
                        entry.name == context.cluster
                            && entry
                                .cluster
                                .as_ref()
                                .and_then(|c| c.server.as_ref())
                                .is_some_and(|s| !s.is_empty())
                    }) {
                        state.request.namespace =
                            state.request.namespace.or(context.namespace.clone());
                        state.request.words[1] = "pods".into();
                    }
                }
            }
        } else {
            state.request.context = None;
            state.request.namespace = None;
        }
        state
    }
    /// Reports a result or a step that the user asked for.
    pub fn notify(&mut self, text: impl Into<String>) {
        self.set_message(Level::Info, text.into(), None, None);
    }
    /// Says why a key did nothing, or which state blocks it.
    pub fn warn(&mut self, text: impl Into<String>) {
        self.set_message(Level::Warning, text.into(), None, None);
    }
    /// Shows the summary of a failure. The failure itself stays available
    /// to the diagnostics view until another message replaces it.
    pub fn fail(&mut self, failure: Failure) {
        let summary = crate::tui_failure::summarize(&failure);
        self.set_message(summary.level, summary.title, summary.hint, Some(failure));
    }
    /// Rows are drawn whose last refresh failed: they may no longer be true.
    pub fn showing_stale_rows(&self) -> bool {
        self.stale && self.data.as_array().is_some_and(|rows| !rows.is_empty())
    }
    pub fn clear_message(&mut self) {
        self.set_message(Level::Info, String::new(), None, None);
    }
    fn set_message(
        &mut self,
        level: Level,
        text: String,
        hint: Option<&'static str>,
        failure: Option<Failure>,
    ) {
        self.message = text;
        self.message_level = level;
        self.message_hint = hint;
        self.last_error = failure;
    }
    pub fn hamn_environment(&self) -> bool {
        self.workspace == Workspace::Containers
            && self.docker_context.is_none()
            && self
                .native
                .as_ref()
                .is_none_or(|command| command.hamn_profile.is_some())
    }
    pub fn vm_panel(&self) -> bool {
        !self.environment_picker
            && self.native.is_none()
            && self.hamn_environment()
            && self.request.operation() == "vm list"
    }
    pub fn save_browser(&mut self) {
        if self.native.is_some() {
            self.browser = Some(self.snapshot());
        }
    }
    fn snapshot(&self) -> Browser {
        Browser {
            native: self.native.clone(),
            request: self.request.clone(),
            data: self.data.clone(),
            selected: self.selected,
            filter: self.filter.clone(),
            scroll: self.scroll,
            environment_picker: self.environment_picker,
            detail: self.detail.clone(),
            connection_status: self.connection_status.clone(),
            stale: self.stale,
            show_operation: self.show_operation,
        }
    }
    fn restore(&mut self, browser: Browser) {
        self.native = browser.native;
        self.request = browser.request;
        self.data = browser.data;
        self.selected = browser.selected;
        self.filter = browser.filter;
        self.scroll = browser.scroll;
        self.environment_picker = browser.environment_picker;
        self.detail = browser.detail;
        self.connection_status = browser.connection_status;
        self.stale = browser.stale;
        self.show_operation = browser.show_operation;
    }
    pub fn open_picker(&mut self) {
        if self.picker_return.is_none() {
            self.picker_return = Some(self.snapshot());
        }
        self.native = None;
        self.environment_picker = false;
        self.detail = None;
        self.show_operation = false;
        self.data = Value::Null;
        self.selected = Some(0);
        self.filter.clear();
        self.scroll = 0;
        self.stale = true;
        self.connection_status = "Connecting".into();
    }
    pub fn discard_picker(&mut self) {
        self.picker_return = None;
    }
    pub fn cancel_picker(&mut self) -> bool {
        if let Some(browser) = self.picker_return.take() {
            self.restore(browser);
            true
        } else {
            false
        }
    }
    pub fn return_to_browser(&mut self) {
        self.environment_picker = false;
        self.detail = None;
        self.request.words = vec!["docker".into(), "containers".into(), "list".into()];
        if let Some(browser) = self
            .browser
            .take()
            .filter(|b| b.request.profile == self.request.profile)
        {
            self.restore(browser);
        } else {
            self.native = crate::native::parse("", self).ok();
            self.selected = Some(0);
            self.filter.clear();
            self.scroll = 0;
        }
    }
    pub fn invalidate_results(&mut self) {
        self.browser = None;
        self.discard_picker();
        self.data = Value::Null;
        self.stale = true;
        self.selected = Some(0);
        self.filter.clear();
        self.scroll = 0;
        self.detail = None;
        self.pending_native = None;
        if self.forward.take().is_some() {
            self.input = None;
        }
        self.connection_status = "Connecting".into();
    }
    /// `F` on the selected row: the port prompt of its port-forward, which
    /// starts with the first port the row declares.
    pub fn open_port_forward(&mut self) -> Result<()> {
        if self.stale || self.loading {
            return Err(Failure::new(
                "staleData",
                "refresh before acting on previous data",
            ));
        }
        let invocation = self
            .native
            .clone()
            .ok_or_else(|| Failure::new("noSelection", "select a resource list"))?;
        let row = self
            .selected()
            .ok_or_else(|| Failure::new("noSelection", "select a resource first"))?;
        let ports = crate::native_actions::forward_ports(&invocation, &row)?;
        self.forward = Some((invocation, row));
        self.input = Some((PORT_FORWARD, ports));
        Ok(())
    }
    /// Closes the prompt line, and with it a port-forward waiting for ports.
    pub fn close_input(&mut self) {
        self.input = None;
        self.forward = None;
    }
    pub fn rows(&self) -> Vec<&Value> {
        let filter = self.filter.to_lowercase();
        self.data
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter(|row| {
                        (self.native.is_some()
                            || self.environment_picker
                            || self.workspace != Workspace::Containers
                            || self.show_all
                            || self.request.operation() != "docker containers list"
                            || row["State"] == "running")
                            && (filter.is_empty() || label(row).to_lowercase().contains(&filter))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn selected(&self) -> Option<Value> {
        self.selected
            .and_then(|index| self.rows().get(index).map(|v| (*v).clone()))
    }
    fn find_identity(&self, key: &[String]) -> Option<usize> {
        let rows = self.rows();
        let mut matches = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| identity(row).as_deref() == Some(key));
        let index = matches.next()?.0;
        matches.next().is_none().then_some(index)
    }
    // Filtering preserves the selected identity when it remains uniquely
    // visible. Hiding it must never transfer an action to a different row.
    pub fn edit_input(&mut self, text: &str, backspace: bool) {
        let key = self.selected().and_then(|row| identity(&row));
        if let Some((prefix, input)) = &mut self.input {
            if backspace {
                input.pop();
            } else {
                input.push_str(text);
            }
            if *prefix == '/' {
                self.filter = input.clone();
                self.selected = key.and_then(|key| self.find_identity(&key));
            }
        }
    }
    // Command history stays in memory: arbitrary CLI arguments may contain secrets.
    pub fn remember_command(&mut self, command: &str) {
        self.history_index = None;
        if command.is_empty() || self.history.last().is_some_and(|last| last == command) {
            return;
        }
        if self.history.len() == 100 {
            self.history.remove(0);
        }
        self.history.push(command.into());
    }
    pub fn recall_command(&mut self, previous: bool) {
        if !self
            .input
            .as_ref()
            .is_some_and(|(prefix, _)| *prefix == ':')
            || self.history.is_empty()
        {
            return;
        }
        let index = match self.history_index {
            None if previous => self.history.len() - 1,
            Some(index) if previous => index.saturating_sub(1),
            Some(index) => (index + 1).min(self.history.len()),
            None => return,
        };
        self.history_index = (index < self.history.len()).then_some(index);
        self.input = Some((':', self.history.get(index).cloned().unwrap_or_default()));
    }
    pub fn clear_filter(&mut self) {
        let key = self.selected().and_then(|row| identity(&row));
        self.filter.clear();
        self.selected = key.and_then(|key| self.find_identity(&key));
    }
    pub fn move_by(&mut self, delta: isize) {
        if self.detail.is_some() || self.show_operation || self.help {
            self.scroll = self
                .scroll
                .saturating_add_signed(delta.clamp(-32768, 32767) as i16);
            return;
        }
        let count = self.rows().len();
        self.selected = if count == 0 {
            None
        } else {
            Some(match self.selected {
                Some(index) => index.saturating_add_signed(delta).min(count - 1),
                None if delta < 0 => count - 1,
                None => 0,
            })
        };
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
        if request.words.first().is_some_and(|word| word == "docker") {
            // Docker and Kubernetes context names are independent namespaces.
            // Preserve explicit targets; inherit exactly one Docker UI target.
            if request.profile.is_none() && request.context.is_none() {
                request.context = self.docker_context.clone();
                if request.context.is_none() {
                    request.profile = self.request.profile.clone();
                }
            }
            if request.context.is_some() {
                request.docker_config =
                    request.docker_config.or_else(|| self.docker_config.clone());
            }
        } else {
            request.profile = request.profile.or_else(|| self.request.profile.clone());
            request.context = request.context.or_else(|| self.request.context.clone());
        }
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
            if request.words.first().is_some_and(|word| word == "docker") {
                // Structured reads establish the displayed Docker environment.
                // Commit its UI target only after validation, so headers, VM
                // controls and subsequent commands cannot refer to another one.
                self.docker_context = request.context.clone();
                self.docker_config = request.docker_config.clone();
            }
            self.native = None;
            self.environment_picker = false;
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
                // A dry run of vm apply: the list it returns to takes neither.
                self.request.file = None;
                self.request.dry_run = false;
            }
            self.data = Value::Null;
            self.stale = false;
        }
        self.selected = Some(0);
        self.filter.clear();
        self.detail = None;
        self.scroll = 0;
        Ok(request)
    }
    pub fn action(&self, action: &str) -> Result<Request> {
        if self.environment_picker {
            return Err(Failure::new(
                "selectEnvironment",
                "Select an environment with Enter before using resource actions",
            ));
        }
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
    /// `c` in the VM panel: a `vm configure` command with the settings of
    /// the selected profile, to edit and run. A profile whose configuration
    /// cannot be read has no settings to offer; its detail says why instead.
    pub fn edit_settings(&mut self) {
        let Some(row) = self.selected() else {
            return;
        };
        if row["configurationError"].is_string() {
            self.detail = Some(profile_detail(&row));
            return;
        }
        self.input = Some((
            ':',
            format!(
                "vm configure --profile {} --cpu {} --memory {} --disk {}",
                row["name"].as_str().unwrap_or("default"),
                row["cpus"],
                row["memoryMiB"].as_u64().unwrap_or(4096) / 1024,
                row["diskGiB"]
            ),
        ));
    }
    pub fn accept(&mut self, result: Result<Value>) {
        if let Ok(value) = &result {
            if value["type"] == "runtimeStatus" {
                self.runtime = value["data"].clone();
                return;
            }
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
                self.refreshing = false;
                self.notify("Log stream ended");
                return;
            }
        }
        self.loading = false;
        self.refreshing = false;
        match result {
            Ok(value) if value.is_array() => {
                let initial = self.data.is_null();
                let identity = self.selected().and_then(|row| identity(&row));
                self.stale = false;
                self.data = value;
                self.selected = if initial {
                    (!self.rows().is_empty()).then_some(0)
                } else {
                    identity.and_then(|key| self.find_identity(&key))
                };
                self.clear_message();
            }
            Ok(value) => {
                // A detail that arrives is shown, not kept behind the help.
                self.help = false;
                self.detail = Some(
                    value["yaml"]
                        .as_str()
                        .map(String::from)
                        .unwrap_or_else(|| serde_json::to_string_pretty(&value).unwrap()),
                );
                self.clear_message();
            }
            Err(error) if error.code == VM_NOT_READY => {
                // Docker lists nothing until the VM is ready. Old rows would
                // look live, so drop them and show how to start the VM.
                self.data = Value::Null;
                self.stale = true;
                self.fail(error);
            }
            Err(error) => {
                // The rows stay visible and are marked stale where they are
                // drawn; actions on them are refused until a read succeeds.
                self.stale = true;
                self.fail(error);
            }
        }
    }
    pub fn enter(&mut self) -> Result<Option<Request>> {
        let Some(row) = self.selected() else {
            return Ok(None);
        };
        match self.request.operation().as_str() {
            "k8s contexts list" => {
                self.invalidate_results();
                self.request.context = row["name"].as_str().map(String::from);
                self.request.namespace = row["namespace"].as_str().map(String::from);
                self.view("pods").map(Some)
            }
            "k8s namespaces list" => {
                self.invalidate_results();
                self.request.namespace = row["metadata"]["name"].as_str().map(String::from);
                self.view("pods").map(Some)
            }
            "vm list" => {
                self.discard_picker();
                self.request.profile = row["name"].as_str().map(String::from);
                self.detail = Some(profile_detail(&row));
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

/// The detail of a row of the VM panel. A profile whose configuration cannot
/// be read has no settings to show: the detail says why and where the file
/// is, because every operation on the profile is refused until it reads.
fn profile_detail(row: &Value) -> String {
    if let Some(error) = row["configurationError"].as_str() {
        return format!(
            "Hamn profile {}\nVM: {}\nConfiguration: cannot be read: {error}\nFile: {}/config.yaml\n\nStart, stop, delete and settings are refused until the file is corrected.\nEsc returns to containers",
            row["name"],
            row["state"],
            row["directory"].as_str().unwrap_or("~/.hamn/<profile>")
        );
    }
    format!(
        "Hamn profile {}\nVM: {}\nDocker: {}\nCPU: {}\nMemory: {} MiB\nDisk: {} GiB\n\ns start / repair   t stop   c edit CPU, memory and disk\nEsc returns to containers",
        row["name"],
        row["state"],
        row["dockerStatus"],
        row["cpus"],
        row["memoryMiB"],
        row["diskGiB"]
    )
}

// Identity is independent of row order and mutable status/resourceVersion.
// Missing or ambiguous identities cannot preserve a selection across refresh.
fn identity(row: &Value) -> Option<Vec<String>> {
    let value = |key: &Value| key.as_str().filter(|s| !s.is_empty()).map(String::from);
    if row.get("metadata").is_some() {
        return Some(vec![
            "kubernetes".into(),
            value(&row["metadata"]["uid"])?,
            value(&row["metadata"]["namespace"]).unwrap_or_default(),
            value(&row["metadata"]["name"])?,
        ]);
    }
    if let Some(id) = value(&row["ID"]).or_else(|| value(&row["Id"])) {
        return Some(vec![
            "docker".into(),
            id,
            value(&row["Repository"]).unwrap_or_default(),
            value(&row["Tag"]).unwrap_or_default(),
        ]);
    }
    let name = value(&row["name"]).or_else(|| value(&row["Name"]))?;
    Some(vec![
        value(&row["environmentKind"]).unwrap_or_default(),
        name,
    ])
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
        .or_else(|| value["Names"].as_str())
        .or_else(|| value["Repository"].as_str())
        .or_else(|| value["ID"].as_str())
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
                .or_else(|| value["Status"].as_str())
                .or_else(|| value["status"]["phase"].as_str())
        })
        .unwrap_or("");
    let docker = value["dockerStatus"].as_str().unwrap_or("");
    // A Hamn profile whose config.yaml cannot be read is listed with the
    // state of its VM and this reason, in the VM panel and in the picker.
    let status = match value["configurationError"].as_str() {
        Some(error) => format!("{status}  unreadable configuration: {error}"),
        None => status.to_owned(),
    };
    if let Some(kind) = value["environmentKind"].as_str() {
        return clean(&match kind {
            "docker" => format!(
                "{name}  Docker context  {}  {status}",
                value["endpoint"].as_str().unwrap_or("")
            ),
            "hamn" => format!("{name}  Hamn profile  {status}  {docker}"),
            _ => format!("{name}  {status}"),
        });
    }
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
        ("Definition file", &request.file),
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

fn native_confirmation(action: &crate::native_actions::Action) -> String {
    format!(
        "Confirm {}\n\nThis changes the selected resource through the installed CLI.\nCancellation does not undo an applied change.\n\ny = execute; n / Esc = cancel",
        action.description
    )
}
pub fn native_confirmation_visible(
    action: &crate::native_actions::Action,
    area: ratatui::layout::Rect,
) -> bool {
    area.width >= 20
        && area.height >= 8
        && wrap_lines(&native_confirmation(action), area.width - 2).len()
            <= usize::from(area.height - 2)
}
/// A confirmation covers the screen; a deletion is framed in red.
fn confirm_block(title: &'static str, danger: bool) -> Block<'static> {
    Block::bordered()
        .title(Line::styled(
            format!(" {title} "),
            Style::default().add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(if danger { Color::Red } else { Color::Yellow }))
}

/// Lines of styled text broken at `width` columns, character by character as
/// [`wrap_lines`] breaks text, so the height of a header is known before it
/// is drawn. Control characters become spaces.
fn wrap_spans(lines: Vec<Line<'static>>, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width.max(1));
    let mut wrapped = Vec::new();
    for line in lines {
        let mut current: Vec<Span<'static>> = Vec::new();
        let mut used = 0;
        for span in line.spans {
            let mut text = String::new();
            for c in span.content.chars() {
                let c = if c.is_control() { ' ' } else { c };
                let size = Line::raw(c.to_string()).width();
                if used + size > width && used > 0 {
                    if !text.is_empty() {
                        current.push(Span::styled(std::mem::take(&mut text), span.style));
                    }
                    wrapped.push(Line::from(std::mem::take(&mut current)));
                    used = 0;
                }
                text.push(c);
                used += size;
            }
            if !text.is_empty() {
                current.push(Span::styled(text, span.style));
            }
        }
        wrapped.push(Line::from(current));
    }
    wrapped
}

fn field(name: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{name}: "), theme::label()),
        Span::styled(value, theme::value()),
    ])
}

/// The header's account of the target: what the view reads, whether it
/// answered, and changes that are running or whose outcome is unknown.
/// The first lines are wrapped at `width`, beside the keys; the second,
/// which can be long, such as a kubeconfig path, at `full_width` under them.
fn info_lines(state: &State, width: u16, full_width: u16) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
    let mut lines = Vec::new();
    match state.workspace {
        Workspace::Containers if state.environment_picker => {
            lines.push(Line::styled("Choose a Hamn profile or Docker context", theme::value()));
            lines.push(Line::styled("Enter selects the environment; Esc cancels", theme::dim()));
        }
        Workspace::Containers => lines.push(field(
            "Environment",
            if state.hamn_environment() {
                format!("Hamn profile {}", state.request.profile.as_deref().unwrap_or("default"))
            } else {
                state
                    .docker_context
                    .as_ref()
                    .map(|c| format!("Docker context {c}"))
                    .unwrap_or_else(|| "Explicit Docker CLI target".into())
            },
        )),
        Workspace::Kubernetes => {
            lines.push(field(
                "Context",
                state.request.context.as_deref().unwrap_or("choose a context").into(),
            ));
            lines.push(field(
                "Namespace",
                if state.request.all_namespaces {
                    "all namespaces".into()
                } else {
                    state.request.namespace.as_deref().unwrap_or("default").into()
                },
            ));
        }
    }
    let hamn_view = state.workspace == Workspace::Containers
        && state.native.as_ref().is_some_and(|i| i.hamn_profile.is_some())
        && !state.runtime.is_null();
    let mut wrapped = wrap_spans(std::mem::take(&mut lines), width);
    // Why the status is unavailable is a sentence: it takes the full width.
    let mut reason_lines = Vec::new();
    if hamn_view {
        let vm_line = |vm: String, docker: Span<'static>| {
            Line::from(vec![
                Span::styled("VM: ", theme::label()),
                Span::styled(vm, theme::value()),
                Span::styled(" | ", theme::dim()),
                Span::styled("Docker: ", theme::label()),
                docker,
            ])
        };
        // A status that could not be read observed neither the VM nor
        // Docker: the header says so and gives the reason below. The reason
        // gets two lines before it is cut. The rule that a configuration
        // breaks is the end of its sentence, after the name of the profile,
        // and has to stay readable at 80 columns.
        match state.runtime["statusError"].as_str() {
            Some(failure) => {
                let unavailable = Span::styled("status unavailable", theme::level(Level::Warning));
                wrapped.extend(wrap_spans(vec![vm_line("status unavailable".into(), unavailable)], width));
                let mut reason = wrap_lines(&format!("Status: {failure}"), full_width);
                if reason.len() > 2 {
                    let second = wrap_lines(&reason[1], full_width.saturating_sub(2))
                        .first()
                        .cloned()
                        .unwrap_or_default();
                    reason.truncate(1);
                    reason.push(format!("{second} …"));
                }
                for (index, line) in reason.into_iter().enumerate() {
                    reason_lines.push(match line.strip_prefix("Status: ").filter(|_| index == 0) {
                        Some(rest) => Line::from(vec![
                            Span::styled("Status: ", theme::label()),
                            Span::styled(rest.to_owned(), theme::level(Level::Warning)),
                        ]),
                        None => Line::styled(line, theme::level(Level::Warning)),
                    });
                }
            }
            None => {
                let (docker, level) = match state.runtime["dockerStatus"].as_str().unwrap_or("unavailable") {
                    "ready" => ("Available", Level::Info),
                    "preparing" => ("Docker connection preparing", Level::Warning),
                    "recoveryRequired" => ("Recovery required", Level::Error),
                    _ => ("Connection unavailable", Level::Warning),
                };
                let vm = state.runtime["state"].as_str().unwrap_or("not created").to_owned();
                wrapped.extend(wrap_spans(
                    vec![vm_line(vm, Span::styled(docker, theme::level(level)))],
                    width,
                ));
            }
        }
    }
    let side = std::mem::replace(&mut wrapped, reason_lines);
    let width = full_width;
    if let Some(command) = &state.native {
        let level = match state.connection_status.as_str() {
            "Available" => Level::Info,
            "Connecting" | "" => Level::Warning,
            status if status.starts_with("Waiting") => Level::Warning,
            _ => Level::Error,
        };
        lines.push(Line::from(vec![
            Span::styled("Target: ", theme::label()),
            Span::raw(clean(&command.target)),
            Span::styled(": ", theme::dim()),
            Span::styled(state.connection_status.clone(), theme::level(level)),
        ]));
    }
    if !state.uncertain.is_empty() {
        lines.push(Line::styled(
            format!(
                "outcomeUnknown: {} operation(s). ! shows targets; inspect before retry.",
                state.uncertain.len()
            ),
            theme::level(Level::Error),
        ));
    }
    wrapped.extend(wrap_spans(lines, width));
    // Full diagnostics belong in the scrollable log, not the fixed header.
    if !state.operation_status.is_empty() {
        let status = &state.operation_status;
        let summary = if wrap_lines(status, width).len() > 1 {
            format!(
                "{} … (! log)",
                wrap_lines(status, width.saturating_sub(10)).first().cloned().unwrap_or_default()
            )
        } else {
            status.clone()
        };
        let level = if status.contains("in progress") || status.starts_with("Cancelling") {
            Level::Warning
        } else if status == "Operation completed" {
            Level::Info
        } else {
            Level::Error
        };
        wrapped.push(Line::styled(summary, theme::level(level)));
    }
    (side, wrapped)
}

/// The scope of the rows, as k9s writes it after the resource: the
/// namespace, or the profile or context of Docker.
fn scope(state: &State) -> Option<String> {
    let invocation = state.native.as_ref()?;
    if let Some(profile) = &invocation.hamn_profile {
        return Some(profile.clone());
    }
    if invocation.target.contains("all namespaces") {
        return Some("all".into());
    }
    let names: &[&str] = if invocation.workspace == Workspace::Kubernetes {
        &["--namespace", "-n"]
    } else {
        &["--context", "-c"]
    };
    invocation.target.split("  ").find_map(|part| {
        names.iter().find_map(|name| {
            part.strip_prefix(&format!("{name} "))
                .or_else(|| part.strip_prefix(&format!("{name}=")))
                .map(String::from)
        })
    })
}

/// The view's place, for the crumbs: the resource, then what covers it.
fn crumbs(state: &State) -> Vec<String> {
    let resource = if state.environment_picker {
        "environments".into()
    } else if let Some(invocation) = &state.native {
        invocation.resource.clone().unwrap_or_else(|| "command".into())
    } else {
        let words = &state.request.words;
        words.get(words.len().saturating_sub(2)).cloned().unwrap_or_default()
    };
    let mut names = vec![resource];
    if state.help {
        names.push("help".into());
    } else if state.show_operation {
        names.push("operation log".into());
    } else if state.detail.is_some() {
        names.push("detail".into());
    }
    names
}

/// The title of the main view: `resource(scope)[count]`, the filter and
/// whether the rows are loading or stale.
fn title(state: &State) -> Line<'static> {
    let workspace = state.workspace;
    let mut spans = vec![Span::raw(" ")];
    if state.help {
        spans.push(Span::styled("Help", theme::key(workspace)));
    } else if state.show_operation {
        spans.push(Span::styled("Operation log", theme::key(workspace)));
    } else if state.detail.is_some() {
        spans.push(Span::styled("Detail", theme::key(workspace)));
    } else {
        let name = if state.environment_picker {
            "Container environments".into()
        } else if let Some(invocation) = &state.native {
            format!(
                "{} {}",
                invocation.program(),
                invocation.resource.as_deref().unwrap_or("command")
            )
        } else {
            state.request.operation()
        };
        spans.push(Span::styled(name, theme::key(workspace)));
        if let Some(scope) = scope(state) {
            spans.push(Span::styled(format!("({scope})"), theme::label()));
        }
        spans.push(Span::styled(format!("[{}]", state.rows().len()), theme::value()));
        if !state.filter.is_empty() {
            spans.push(Span::styled(
                format!(" </{}>", clean(&state.filter)),
                Style::default().fg(Color::Green),
            ));
        }
    }
    if state.loading {
        spans.push(Span::styled(" [loading]", theme::level(Level::Warning)));
    } else if state.showing_stale_rows() {
        spans.push(Span::styled(" [stale]", theme::level(Level::Error)));
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}

/// Styled text broken at spaces where it can be, so that words stay whole;
/// only a word longer than the line is broken inside. The space at a break
/// is dropped.
fn wrap_words(line: Line<'static>, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width.max(1));
    let cells: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|span| {
            let style = span.style;
            span.content
                .chars()
                .map(move |c| (if c.is_control() { ' ' } else { c }, style))
        })
        .collect();
    let size = |c: char| Line::raw(c.to_string()).width();
    let mut rows: Vec<&[(char, Style)]> = Vec::new();
    let mut start = 0;
    while start < cells.len() {
        let (mut end, mut used) = (start, 0);
        while end < cells.len() && used + size(cells[end].0) <= width {
            used += size(cells[end].0);
            end += 1;
        }
        if end < cells.len() {
            if let Some(space) = (start + 1..=end).rev().find(|&i| cells[i].0 == ' ') {
                rows.push(&cells[start..space]);
                start = space + 1;
                continue;
            }
            end = end.max(start + 1);
        }
        rows.push(&cells[start..end]);
        start = end;
    }
    if rows.is_empty() {
        rows.push(&[]);
    }
    rows.into_iter()
        .map(|row| {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for &(c, style) in row {
                match spans.last_mut() {
                    Some(span) if span.style == style => span.content.to_mut().push(c),
                    _ => spans.push(Span::styled(c.to_string(), style)),
                }
            }
            Line::from(spans)
        })
        .collect()
}

/// The message line: an icon of its level, the message, what helps and
/// where the details are; at most two lines.
fn message_lines(state: &State, width: u16) -> Vec<Line<'static>> {
    if state.message.is_empty() {
        return vec![Line::raw("")];
    }
    let style = theme::level(state.message_level);
    let mut spans = vec![
        Span::styled(format!("{} ", theme::icon(state.message_level)), style),
        Span::styled(clean(&state.message), style),
    ];
    if let Some(hint) = state.message_hint {
        spans.push(Span::styled(format!(" — {hint}"), theme::dim()));
    }
    if state.last_error.is_some() && state.message_level == Level::Error {
        spans.push(Span::styled(" (! details)", theme::dim()));
    }
    let mut lines = wrap_words(Line::from(spans), width);
    if lines.len() > 2 {
        lines.truncate(2);
        let last = &mut lines[1];
        let text: String = last.spans.iter().map(|span| span.content.as_ref()).collect();
        let kept: String = text.chars().take(text.chars().count().saturating_sub(2)).collect();
        *last = Line::from(vec![Span::styled(format!("{kept} …"), style)]);
    }
    lines
}

pub fn draw(frame: &mut Frame, state: &State) {
    let area = frame.area();
    if let Some(action) = &state.pending_native {
        let text = if native_confirmation_visible(action, area) {
            native_confirmation(action)
        } else {
            "Resize terminal to review the full target. Execution disabled. Esc cancels.".into()
        };
        frame.render_widget(
            Paragraph::new(clean(&text))
                .wrap(Wrap { trim: false })
                .block(confirm_block("Hamn confirmation", action.description.contains("delete"))),
            area,
        );
        return;
    }
    if state.quit_confirmation {
        frame.render_widget(Paragraph::new("Cancel the active operation and exit?\nHamn will wait for rollback and resource cleanup.\ny = cancel then exit; n / Esc = keep working")
            .wrap(Wrap { trim: false }).block(confirm_block("Active operation", false)), area);
        return;
    }
    if let Some(pending) = &state.pending {
        let text = if confirmation_visible(pending, area) {
            confirmation(pending, area.width - 2).join("\n")
        } else {
            "Resize terminal to review the full target and impact.\nExecution disabled. Esc cancels.".into()
        };
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .block(confirm_block("Hamn confirmation", pending.operation().ends_with("delete"))),
            area,
        );
        return;
    }
    let workspace = state.workspace;
    let hints = crate::tui_keys::hints(state);
    // As in k9s, wide terminals show the keys beside the target; narrow
    // ones keep the target at full width and list the keys under the view.
    let (info_width, header_hints, footer_hints) = if area.width >= 104 {
        let column = usize::from(theme::hint_grid_width(&hints, 1)).max(1);
        let columns = (usize::from(area.width - 60) / column).clamp(1, 4);
        let width = theme::hint_grid_width(&hints, columns);
        (area.width - width - 2, theme::hint_grid(&hints, workspace, width, 4), Vec::new())
    } else {
        (area.width, Vec::new(), theme::hint_flow(&hints, workspace, area.width, 2))
    };
    let (info, details) = info_lines(state, info_width, area.width);
    let beside = info.len().max(header_hints.len());
    let header_height = beside + details.len();
    let prompt_height = if state.input.is_some() { 3 } else { 0 };
    let message = message_lines(state, area.width);
    let fixed = 1 + header_height + prompt_height + footer_hints.len() + 1 + message.len();
    if area.width < 20 || fixed + 3 > usize::from(area.height) {
        frame.render_widget(
            Paragraph::new("Hamn: resize terminal to show the selected target. q exits.")
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }
    let length = |n: usize| Constraint::Length(n as u16);
    let areas = Layout::vertical([
        Constraint::Length(1),
        length(header_height),
        length(prompt_height),
        Constraint::Min(3),
        length(footer_hints.len()),
        Constraint::Length(1),
        length(message.len()),
    ])
    .split(area);
    frame.render_widget(Paragraph::new(theme::tabs(workspace)), areas[0]);
    let header = Layout::vertical([length(beside), length(details.len())]).split(areas[1]);
    frame.render_widget(Paragraph::new(details), header[1]);
    if header_hints.is_empty() {
        frame.render_widget(Paragraph::new(info), header[0]);
    } else {
        let header = Layout::horizontal([Constraint::Length(info_width), Constraint::Min(0)])
            .spacing(2)
            .split(header[0]);
        frame.render_widget(Paragraph::new(info), header[0]);
        frame.render_widget(Paragraph::new(header_hints), header[1]);
    }
    if let Some((prefix, input)) = &state.input {
        let (name, keys, lead) = match (*prefix, &state.forward) {
            (':', _) => (
                " Command ".to_string(),
                " Enter runs · ↑/↓ history · Esc cancels ",
                ":".to_string(),
            ),
            (PORT_FORWARD, Some((invocation, row))) => (
                clean(&format!(
                    " Port-forward {}/{} — LOCAL:REMOTE ",
                    invocation.resource.as_deref().unwrap_or_default(),
                    row["metadata"]["name"].as_str().unwrap_or_default()
                )),
                " Enter starts · Esc cancels ",
                "ports ".to_string(),
            ),
            _ => (
                " Filter ".to_string(),
                " Enter keeps · Esc closes ",
                prefix.to_string(),
            ),
        };
        let text = format!("{lead}{}", clean(input));
        frame.render_widget(
            Paragraph::new(text.clone()).block(
                Block::bordered()
                    .border_style(theme::border(workspace))
                    .title(Line::styled(name, theme::key(workspace)))
                    .title(Line::styled(keys, theme::dim()).right_aligned()),
            ),
            areas[2],
        );
        let column = Line::raw(text).width() as u16;
        if column + 2 < areas[2].width {
            frame.set_cursor_position((areas[2].x + 1 + column, areas[2].y + 1));
        }
    }
    let block = Block::bordered()
        .border_style(theme::border(workspace))
        .title(title(state))
        .title_alignment(Alignment::Center);
    let operation_detail = format!(
        "{}{}\n{}\n{}",
        state.last_error.as_ref().map_or(String::new(), |error| format!(
            "Last error ({})\n{}\n\n",
            error.code, error.message
        )),
        state.operation_status,
        state.operation_log,
        serde_json::to_string_pretty(&state.uncertain).unwrap()
    );
    if state.help {
        let columns = crate::tui_keys::help(workspace, areas[3].width);
        let inner = block.inner(areas[3]);
        frame.render_widget(block, areas[3]);
        let parts = Layout::horizontal(vec![Constraint::Fill(1); columns.len()])
            .spacing(2)
            .split(inner);
        for (lines, part) in columns.into_iter().zip(parts.iter()) {
            frame.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .scroll((state.scroll, 0)),
                *part,
            );
        }
    } else if let Some(detail) = if state.show_operation {
        Some(&operation_detail)
    } else {
        state.detail.as_ref()
    } {
        frame.render_widget(
            Paragraph::new(clean(detail))
                .scroll((state.scroll, 0))
                .wrap(Wrap { trim: false })
                .block(block),
            areas[3],
        );
    } else if state.native.is_some() {
        crate::resource_table::draw(frame, areas[3], state, block);
    } else {
        let stale = state.showing_stale_rows();
        let rows: Vec<_> = state
            .rows()
            .into_iter()
            .map(|row| {
                let style = if stale { theme::dim() } else { theme::tone(label_tone(row)) };
                ListItem::new(label(row)).style(style)
            })
            .collect();
        frame.render_stateful_widget(
            List::new(rows)
                .block(block)
                .highlight_style(theme::badge(workspace))
                .highlight_symbol("> "),
            areas[3],
            &mut ListState::default().with_selected(state.selected),
        );
    }
    frame.render_widget(Paragraph::new(footer_hints), areas[4]);
    let crumbs = Line::from(theme::crumbs(&crumbs(state), workspace));
    let bottom = Layout::horizontal([Constraint::Length(crumbs.width() as u16), Constraint::Min(0)])
        .split(areas[5]);
    frame.render_widget(Paragraph::new(crumbs), bottom[0]);
    frame.render_widget(
        Paragraph::new(Line::styled(state.refresh.label(), theme::dim()).right_aligned()),
        bottom[1],
    );
    frame.render_widget(Paragraph::new(message), areas[6]);
}

/// The state that a row of a Hamn profile or environment reports.
fn label_tone(row: &Value) -> theme::Tone {
    if row["configurationError"].is_string() || row["dockerStatus"] == "recoveryRequired" {
        return theme::Tone::Failed;
    }
    if row["disabled"] == true {
        return theme::Tone::Done;
    }
    match row["state"].as_str().or_else(|| row["State"].as_str()).unwrap_or("") {
        "stopped" | "exited" | "not created" => theme::Tone::Done,
        "starting" | "stopping" | "created" | "paused" | "restarting" => theme::Tone::Pending,
        _ => theme::Tone::Normal,
    }
}

pub fn draw_choice(frame: &mut Frame, selected: usize, settings: bool, error: &str) {
    let dependency = |name: &str| {
        use std::os::unix::fs::PermissionsExt;
        std::env::var_os("PATH").is_some_and(|path| {
            std::env::split_paths(&path).any(|directory| {
                std::fs::metadata(directory.join(name)).is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
            })
        })
    };
    let found = |name: &str| {
        if dependency(name) {
            Span::styled("found", theme::level(Level::Info))
        } else {
            Span::styled("missing", theme::level(Level::Error))
        }
    };
    let option = |index: usize, workspace: Workspace, name: &'static str, about: &'static str| {
        let chosen = index == selected;
        Line::from(vec![
            Span::raw(if chosen { "> " } else { "  " }),
            Span::styled(
                format!(" {} {name} ", index + 1),
                if chosen { theme::badge(workspace) } else { theme::value() },
            ),
            Span::raw(format!(" — {about}")),
        ])
    };
    let workspace = if selected == 0 { Workspace::Containers } else { Workspace::Kubernetes };
    let mut lines = vec![
        Line::styled(
            if settings {
                "Choose the workspace to open by default"
            } else {
                "Choose your default workspace"
            },
            theme::value(),
        ),
        Line::raw(""),
        option(0, Workspace::Containers, "Containers", "Docker containers, images, volumes and networks"),
        option(1, Workspace::Kubernetes, "Kubernetes", "resources in your kubeconfig context"),
        Line::raw(""),
        Line::from(vec![
            Span::styled("<1>/<2>", theme::key(workspace)),
            Span::raw(" or arrows to select, "),
            Span::styled("<Enter>", theme::key(workspace)),
            Span::raw(" to save"),
        ]),
        Line::from(if settings {
            vec![Span::styled("<Esc>", theme::key(workspace)), Span::raw(" returns without changing the default")]
        } else {
            vec![Span::styled("<q>", theme::key(workspace)), Span::raw(" exits")]
        }),
        Line::raw(""),
    ];
    lines.extend(
        clean(error)
            .lines()
            .map(|line| Line::styled(line.to_owned(), theme::level(Level::Error))),
    );
    lines.extend([
        Line::raw(""),
        Line::from(vec![
            Span::styled("Docker CLI: ", theme::label()),
            found("docker"),
            Span::styled(" | ", theme::dim()),
            Span::styled("kubectl: ", theme::label()),
            found("kubectl"),
        ]),
        Line::raw("Containers: install Docker CLI, then e selects a Docker context or Hamn profile."),
        Line::raw("Kubernetes: install kubectl and configure kubeconfig; e selects an external cluster."),
        Line::styled("Connection and credentials are checked by the first resource query.", theme::dim()),
    ]);
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::bordered()
                .border_style(theme::border(workspace))
                .title(Line::styled(" Hamn settings ", theme::key(workspace))),
        ),
        frame.area(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn structured_target_switches_keep_header_scope_and_followup_requests_consistent() {
        let mut state = State::new(Request::default());
        let remote = state
            .view("docker containers list --context remote --docker-config /fixture/config")
            .unwrap();
        assert!(remote.profile.is_none());
        assert_eq!(state.docker_context.as_deref(), Some("remote"));
        assert_eq!(state.docker_config.as_deref(), Some("/fixture/config"));
        assert!(!state.hamn_environment());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 32)).unwrap();
        terminal.draw(|frame| draw(frame, &state)).unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("Docker context remote"));
        assert!(!rendered.contains("<v> VM settings"));
        let inherited = state.view("docker containers list").unwrap();
        assert_eq!(inherited.context, remote.context);
        assert_eq!(inherited.docker_config, remote.docker_config);
        let native = crate::native::parse("ps", &state).unwrap();
        assert!(
            native
                .args
                .windows(2)
                .any(|args| args == ["--context", "remote"])
        );
        assert!(
            native
                .args
                .windows(2)
                .any(|args| args == ["--config", "/fixture/config"])
        );
        for command in [
            "docker containers list --context remote --profile local",
            "docker containers list --context ''",
            "docker containers list --profile invalid:name",
        ] {
            assert!(state.view(command).is_err());
            assert_eq!(state.docker_context.as_deref(), Some("remote"));
            assert_eq!(state.docker_config.as_deref(), Some("/fixture/config"));
            assert_eq!(state.request.context.as_deref(), Some("remote"));
        }
        let local = state
            .view("docker containers list --profile local")
            .unwrap();
        assert_eq!(local.profile.as_deref(), Some("local"));
        assert!(state.docker_context.is_none() && state.docker_config.is_none());
        assert!(state.hamn_environment());
        terminal.draw(|frame| draw(frame, &state)).unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("Hamn profile local"));
        assert!(rendered.contains("<v> VM settings"));
        let inherited = state.view("docker containers list").unwrap();
        assert_eq!(inherited.profile.as_deref(), Some("local"));
        assert!(inherited.context.is_none());
        assert!(
            state
                .view("docker containers list --context remote --profile local")
                .is_err()
        );
        assert_eq!(state.request.profile.as_deref(), Some("local"));
        assert!(state.docker_context.is_none());
    }
    #[test]
    fn structured_docker_views_inherit_only_the_docker_target_and_config() {
        let mut state = State::new(Request {
            context: Some("kube-only".into()),
            ..Default::default()
        });
        let request = state.view("docker containers list").unwrap();
        assert_eq!(request.profile.as_deref(), Some("default"));
        assert!(request.context.is_none());
        state.docker_context = Some("docker-context".into());
        state.docker_config = Some("/fixture/docker-config".into());
        let request = state.view("docker containers list").unwrap();
        assert!(request.profile.is_none());
        assert_eq!(request.context.as_deref(), Some("docker-context"));
        assert_eq!(
            request.docker_config.as_deref(),
            Some("/fixture/docker-config")
        );
        let request = state.view("docker containers list --profile own").unwrap();
        assert_eq!(request.profile.as_deref(), Some("own"));
        assert!(request.context.is_none());
        assert!(request.docker_config.is_none());
    }
    #[test]
    fn refresh_keeps_identity_and_never_selects_a_replacement_or_ambiguous_row() {
        let mut state = State::new(Request::default());
        state.native = Some(crate::native::parse("ps", &state).unwrap());
        let a = serde_json::json!({"ID":"a", "Names":"alpha"});
        let b = serde_json::json!({"ID":"b", "Names":"beta"});
        let c = serde_json::json!({"ID":"c", "Names":"gamma"});
        state.accept(Ok(serde_json::json!([a, b, c])));
        state.move_by(1);
        state.accept(Ok(serde_json::json!([c, a, b])));
        assert_eq!(state.selected().unwrap()["ID"], "b");
        state.accept(Ok(serde_json::json!([a, c])));
        assert!(state.selected().is_none());
        state.accept(Ok(serde_json::json!([b, a, c])));
        assert!(
            state.selected().is_none(),
            "automatic refresh cannot reselect"
        );
        state.move_by(1);
        assert_eq!(state.selected().unwrap()["ID"], "b");
        state.accept(Ok(serde_json::json!([b, b])));
        assert!(
            state.selected().is_none(),
            "ambiguous identity must clear selection"
        );
    }
    #[test]
    fn kubernetes_selection_distinguishes_namespace_uid_and_ignores_resource_version() {
        let mut state = State::new(Request::default());
        state.workspace = Workspace::Kubernetes;
        let pod = |ns: &str, uid: &str, version: &str| serde_json::json!({"metadata":{"name":"same", "namespace":ns,"uid":uid,"resourceVersion":version}});
        state.accept(Ok(serde_json::json!([
            pod("a", "a", "1"),
            pod("b", "b", "1")
        ])));
        state.move_by(1);
        state.accept(Ok(serde_json::json!([
            pod("b", "b", "2"),
            pod("a", "a", "1")
        ])));
        assert_eq!(state.selected().unwrap()["metadata"]["namespace"], "b");
        state.accept(Ok(serde_json::json!([
            pod("b", "replacement", "3"),
            pod("a", "a", "1")
        ])));
        assert!(state.selected().is_none());
    }
    #[test]
    fn paste_and_typing_share_filter_updates_and_command_paste_does_not_filter() {
        let mut state = State::new(Request::default());
        state.native = Some(crate::native::parse("ps", &state).unwrap());
        state.accept(Ok(
            serde_json::json!([{"ID":"a","Names":"alpha"},{"ID":"b","Names":"beta"}]),
        ));
        state.move_by(1);
        state.input = Some(('/', String::new()));
        state.edit_input("beta", false);
        assert_eq!(state.rows().len(), 1);
        assert_eq!(state.selected().unwrap()["ID"], "b");
        state.clear_filter();
        assert_eq!(state.selected().unwrap()["ID"], "b");
        state.input = Some((':', String::new()));
        state.edit_input("get pods", false);
        assert!(state.filter.is_empty());
        state.input = Some(('/', String::new()));
        state.edit_input("missing", false);
        assert!(state.selected().is_none());
    }
    #[test]
    fn filtering_preserves_identity_and_clears_hidden_or_ambiguous_selection() {
        let mut state = State::new(Request::default());
        state.native = Some(crate::native::parse("ps", &state).unwrap());
        state.accept(Ok(
            serde_json::json!([{"ID":"a","Names":"alpha"},{"ID":"b","Names":"beta"}]),
        ));
        state.move_by(1);
        state.input = Some(('/', String::new()));
        state.edit_input("a", false);
        assert_eq!(state.selected().unwrap()["ID"], "b");
        state.edit_input("", true);
        assert_eq!(state.selected().unwrap()["ID"], "b");
        state.edit_input("alpha", false);
        assert_eq!(state.rows().len(), 1);
        assert!(state.selected().is_none());
        state.clear_filter();
        assert!(state.selected().is_none());
        state.move_by(1);
        state.accept(Ok(
            serde_json::json!([{"ID":"a","Names":"alpha"},{"ID":"a","Names":"alpha duplicate"}]),
        ));
        state.input = Some(('/', String::new()));
        state.edit_input("alpha", false);
        assert!(state.selected().is_none());
    }
    #[test]
    fn refresh_backoff_is_bounded_and_success_resets_it() {
        let mut refresh = Refresh::default();
        let now = std::time::Instant::now();
        refresh.complete(false, now);
        assert_eq!(refresh.next.duration_since(now).as_secs(), 4);
        for _ in 0..20 {
            refresh.complete(false, now);
        }
        assert_eq!(refresh.next.duration_since(now).as_secs(), 60);
        refresh.paused = true;
        assert!(!refresh.due(now + std::time::Duration::from_secs(100)));
        refresh.complete(true, now);
        assert_eq!(refresh.failures, 0);
        assert_eq!(refresh.next.duration_since(now).as_secs(), 2);
    }
    #[test]
    fn waiting_for_a_vm_polls_at_the_normal_interval_without_claiming_success() {
        let mut refresh = Refresh::default();
        let now = std::time::Instant::now();
        for _ in 0..3 {
            refresh.complete(false, now);
        }
        assert!(refresh.label().contains("retry backoff (3)"));
        refresh.wait(now);
        assert_eq!(refresh.failures, 0);
        assert_eq!(refresh.next.duration_since(now).as_secs(), 2);
        assert!(refresh.last_success.is_none());
        let label = refresh.label();
        assert!(
            label.contains("last OK never") && !label.contains("backoff"),
            "{label}"
        );
        refresh.paused = true;
        refresh.wait(now);
        assert!(!refresh.due(now + std::time::Duration::from_secs(100)));
    }
    #[test]
    fn last_success_label_keeps_a_fixed_width_across_refreshes() {
        let mut refresh = Refresh::default();
        for (time, shown) in [
            (
                "2026-09-30T05:12:33.123456789Z",
                "last OK 2026-09-30T05:12:33Z",
            ),
            ("2026-09-30T05:12:35.9Z", "last OK 2026-09-30T05:12:35Z"),
            ("2026-09-30T05:12:37Z", "last OK 2026-09-30T05:12:37Z"),
        ] {
            refresh.last_success = Some(time.parse().unwrap());
            let label = refresh.label();
            assert!(label.ends_with(shown), "{label}");
        }
    }
    #[test]
    fn failed_hamn_query_explains_a_vm_that_is_not_ready() {
        let cli = || {
            Failure::new(
                crate::native::DOCKER_UNREACHABLE,
                "docker exited 1: Cannot connect to the Docker daemon",
            )
        };
        for (vm, docker, expected) in [
            (
                "stopped",
                "unavailable",
                "Hamn VM work is not running (stopped). Press s to start it",
            ),
            (
                "unknown",
                "unavailable",
                "Hamn VM work is not running (unknown). Press s to start it",
            ),
            (
                "stopping",
                "unavailable",
                "Hamn VM work is not running (stopping). Press s to start it",
            ),
            (
                "starting",
                "unavailable",
                "Hamn VM work is starting; the list appears when Docker is ready.",
            ),
            (
                "running",
                "preparing",
                "Hamn VM work is starting; the list appears when Docker is ready.",
            ),
            (
                "running",
                "unavailable",
                "Docker is unavailable in Hamn VM work. Press s to repair it.",
            ),
            (
                "stopped",
                "recoveryRequired",
                "Hamn VM work needs recovery. Press s to repair it.",
            ),
            (
                "running",
                "recoveryRequired",
                "Hamn VM work needs recovery. Press s to repair it.",
            ),
        ] {
            let status = serde_json::json!({"state": vm, "dockerStatus": docker});
            let failure = hamn_query_failure("work", Some(&status), cli());
            assert_eq!(failure.code, VM_NOT_READY, "{vm}/{docker}");
            assert!(
                failure.message.starts_with(expected),
                "{vm}/{docker}: {}",
                failure.message
            );
            assert!(
                !failure.message.contains("Cannot connect"),
                "{}",
                failure.message
            );
        }
        // A status without fields cannot claim that Docker is ready.
        let failure = hamn_query_failure("work", Some(&Value::Null), cli());
        assert_eq!(failure.code, VM_NOT_READY);
        assert!(
            failure.message.contains("not running (unknown)"),
            "{}",
            failure.message
        );
    }
    #[test]
    fn a_profile_whose_configuration_cannot_be_read_says_why_and_offers_no_settings() {
        let readable = serde_json::json!({"name": "external", "state": "stopped", "dockerStatus": "unavailable",
            "cpus": 2, "memoryMiB": 2048, "diskGiB": 60});
        // What `vm list` holds for a profile it could not read: no settings.
        let unreadable = serde_json::json!({"name": "old", "state": "running", "directory": "/Users/me/.hamn/old",
            "configurationError": "unknown configuration key: kubernetes"});
        let reason = "unreadable configuration: unknown configuration key: kubernetes";

        // The row keeps the state of the VM and adds the reason, in the VM
        // panel and, marked as a Hamn profile, in the environment picker.
        assert_eq!(label(&unreadable).trim_end(), format!("old  running  {reason}"));
        let mut picked = unreadable.clone();
        picked["environmentKind"] = "hamn".into();
        assert_eq!(label(&picked).trim_end(), format!("old  Hamn profile  running  {reason}"));
        assert_eq!(label(&readable), "external  stopped  unavailable");

        let mut state = State::new(Request::default());
        state.view("vm").unwrap();
        state.accept(Ok(serde_json::json!([readable, unreadable])));
        assert!(state.vm_panel());

        // Enter and `c` on the unreadable profile show why and where the
        // file is, and offer no command made of settings that do not exist.
        state.selected = Some(1);
        assert!(state.enter().unwrap().is_none());
        let detail = state.detail.take().unwrap();
        assert!(detail.contains("Configuration: cannot be read: unknown configuration key: kubernetes"), "{detail}");
        assert!(detail.contains("File: /Users/me/.hamn/old/config.yaml"), "{detail}");
        assert!(!detail.contains("CPU") && !detail.contains("null"), "{detail}");
        state.edit_settings();
        assert_eq!(state.detail.take().as_deref(), Some(detail.as_str()));
        assert!(state.input.is_none());

        // A profile that reads keeps its detail and its prefilled command.
        state.selected = Some(0);
        assert!(state.enter().unwrap().is_none());
        assert!(state.detail.take().unwrap().contains("CPU: 2\nMemory: 2048 MiB\nDisk: 60 GiB"));
        state.edit_settings();
        assert!(state.detail.is_none());
        assert_eq!(
            state.input,
            Some((':', "vm configure --profile external --cpu 2 --memory 2 --disk 60".to_owned()))
        );
    }
    #[test]
    fn the_header_says_why_the_status_of_a_profile_is_unavailable() {
        let header = |runtime: Value, width: u16| {
            let mut state = State::new(Request::default());
            state.native = Some(crate::native::parse("ps", &state).unwrap());
            state.runtime = runtime;
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 24)).unwrap();
            terminal.draw(|f| draw(f, &state)).unwrap();
            let buffer = terminal.backend().buffer().clone();
            (0..buffer.area.height)
                .map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect::<String>())
                .collect::<Vec<_>>()
        };
        let unreadable = "cannot read the configuration of profile default: unknown configuration key: kubernetes";
        // The failure of the status is what the header shows: neither the
        // VM, which can be running, nor Docker was observed.
        let lines = header(serde_json::json!({"statusError": unreadable}), 120);
        assert!(lines.iter().any(|line| line.contains("VM: status unavailable | Docker: status unavailable")), "{lines:?}");
        // The reason takes the full width, under the keys of a wide terminal.
        assert!(lines.iter().any(|line| line.contains(&format!("Status: {unreadable}"))), "{lines:?}");
        assert!(!lines.iter().any(|line| line.contains("not created") || line.contains("Connection unavailable")), "{lines:?}");
        // At 80 columns the sentence takes a second line and keeps the rule,
        // which is its end.
        let lines = header(serde_json::json!({"statusError": unreadable}), 80);
        let first = lines.iter().position(|line| line.contains("Status: cannot read the configuration")).unwrap();
        assert!(lines[first + 1].starts_with("key: kubernetes"), "{lines:?}");
        assert!(!lines.iter().any(|line| line.contains('…')), "{lines:?}");
        // A longer reason is cut after its second line: the header must
        // leave room for the rows.
        let long = format!("{unreadable} {}", "x".repeat(300));
        let lines = header(serde_json::json!({"statusError": long}), 80);
        let reason: Vec<_> = lines.iter().filter(|line| line.contains("Status: ") || line.contains("xxxx") || line.contains("kubernetes")).collect();
        assert_eq!(reason.len(), 2, "{lines:?}");
        assert!(reason[1].starts_with("key: kubernetes xxxx") && reason[1].contains('…'), "{lines:?}");
        assert!(!lines.iter().any(|line| line.contains("resize terminal")), "{lines:?}");
        // An observed status is shown as before.
        let lines = header(serde_json::json!({"state": "stopped", "dockerStatus": "unavailable"}), 120);
        assert!(lines.iter().any(|line| line.contains("VM: stopped | Docker: Connection unavailable")), "{lines:?}");
        assert!(!lines.iter().any(|line| line.contains("Status:")), "{lines:?}");
    }
    #[test]
    fn failed_hamn_query_keeps_errors_that_starting_the_vm_cannot_fix() {
        let stopped = serde_json::json!({"state": "stopped", "dockerStatus": "unavailable"});
        let ready = serde_json::json!({"state": "running", "dockerStatus": "ready"});
        // recoveryRequired comes from the operation record while Docker may work.
        let recovery = serde_json::json!({"state": "running", "dockerStatus": "recoveryRequired"});
        let unreachable = crate::native::DOCKER_UNREACHABLE;
        for (status, code, message) in [
            // An unreadable profile proves nothing about the VM.
            (
                None,
                unreachable,
                "docker exited 1: failed to connect to the docker API at",
            ),
            (
                Some(&ready),
                unreachable,
                "docker exited 1: error during connect: EOF",
            ),
            // native::query reports these as cliError; the daemon never answered.
            (
                Some(&stopped),
                "cliError",
                "docker exited 125: unknown flag: --bogus-flag",
            ),
            (
                Some(&stopped),
                "cliError",
                "docker exited 1: docker: unknown command: docker compose",
            ),
            (
                Some(&stopped),
                "cliError",
                "CLI output exceeds 16 MiB; / filters fetched rows only.",
            ),
            (
                Some(&recovery),
                "cliError",
                "docker exited 125: unknown flag: --bogus-flag",
            ),
            (
                Some(&stopped),
                "cliUnavailable",
                "docker: No such file or directory",
            ),
            (
                Some(&stopped),
                "cliProtocol",
                "expected value at line 1 column 1",
            ),
        ] {
            let failure = hamn_query_failure("work", status, Failure::new(code, message));
            assert_eq!(
                (failure.code.as_str(), failure.message.as_str()),
                (code, message)
            );
        }
    }
    #[test]
    fn vm_not_ready_clears_rows_and_shows_guidance_until_docker_answers() {
        let mut state = State::new(Request::default());
        state.native = Some(crate::native::parse("ps", &state).unwrap());
        let rows = serde_json::json!([{"ID":"a", "Names":"alpha"}, {"ID":"b", "Names":"beta"}]);
        state.accept(Ok(rows.clone()));
        state.move_by(1);
        let guidance = "Hamn VM default is not running (stopped). Press s to start it; the list appears when Docker is ready.";
        state.refreshing = true;
        state.accept(Err(Failure::new(VM_NOT_READY, guidance)));
        assert!(!state.refreshing && !state.loading);
        assert!(state.rows().is_empty() && state.selected().is_none());
        assert_eq!(state.message, guidance);
        assert_eq!(state.action("stop").unwrap_err().code, "staleData");
        // The cleared table still renders with the old selection index.
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &state)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            text.contains("Press s to start it") && !text.contains("beta"),
            "{text}"
        );
        state.accept(Ok(rows));
        assert!(!state.stale && state.message.is_empty());
        assert_eq!(state.selected().unwrap()["ID"], "a");
    }
    #[test]
    fn automatic_refresh_of_unchanged_rows_redraws_no_cell() {
        let screen = |state: &State| {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 24)).unwrap();
            terminal.draw(|frame| draw(frame, state)).unwrap();
            terminal.backend().buffer().clone()
        };
        let last_ok: jiff::Timestamp = "2026-09-30T05:12:33Z".parse().unwrap();
        let rows = serde_json::json!([{"ID":"a", "Names":"alpha", "State":"running"}]);
        let runtime = serde_json::json!({"type":"runtimeStatus",
            "data":{"state":"running", "dockerStatus":"ready"}});
        let mut state = State::new(Request::default());
        state.native = Some(crate::native::parse("ps", &state).unwrap());
        state.accept(Ok(runtime.clone()));
        state.accept(Ok(rows.clone()));
        state.connection_status = "Available".into();
        state.refresh.last_success = Some(last_ok);
        let before = screen(&state);
        // The same sequence the TUI applies to a periodic refresh.
        state.refreshing = true;
        assert_eq!(
            screen(&state),
            before,
            "starting a refresh changed the screen"
        );
        state.accept(Ok(runtime));
        state.refresh.complete(true, std::time::Instant::now());
        state.refresh.last_success = Some(last_ok);
        state.accept(Ok(rows));
        assert_eq!(
            screen(&state),
            before,
            "an unchanged result changed the screen"
        );
        // A load that replaces the view still shows that it is running.
        state.loading = true;
        let loading = screen(&state);
        assert_ne!(loading, before);
        let text: String = loading.content().iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("[loading]"), "{text}");
    }
    #[test]
    fn environment_picker_never_interprets_context_names_as_vm_profiles() {
        let mut state = State::new(Request::default());
        state.view("vm").unwrap();
        assert!(state.vm_panel());
        state.open_picker();
        state.environment_picker = true;
        state.accept(Ok(serde_json::json!([
            {"name":"shared", "environmentKind":"hamn", "state":"running", "dockerStatus":"ready"},
            {"name":"shared", "environmentKind":"docker", "endpoint":"unix:///external.sock"}
        ])));
        assert!(!state.vm_panel());
        for selected in [0, 1] {
            state.selected = Some(selected);
            for action in ["start", "stop", "delete", "restart", "logs", "stats"] {
                assert_eq!(state.action(action).unwrap_err().code, "selectEnvironment");
            }
        }
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &state)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        for marker in [
            "shared  Hamn profile",
            "shared  Docker context",
            "unix:///external.sock",
            "<Enter> select",
        ] {
            assert!(screen.contains(marker), "{screen}");
        }
        assert!(!screen.contains("<v> VM settings"));
        assert!(!screen.contains("<t> stop"));
        assert!(state.cancel_picker());
        assert!(state.vm_panel());
        state.data = serde_json::json!([{"name":"shared"}]);
        assert_eq!(
            state.action("stop").unwrap().profile.as_deref(),
            Some("shared")
        );
        state.native = Some(crate::native::parse("docker --context external ps", &state).unwrap());
        assert!(!state.vm_panel()); // A stale vm-list request cannot enable configure in a native view.
    }
    #[test]
    fn long_failure_header_keeps_normal_size_resource_and_scrollable_log_visible() {
        for diagnostic in [
            "x".repeat(8192),
            "긴 작업 진단 ".repeat(400),
            "first\nsecond\n".repeat(300),
        ] {
            let mut state = State::new(Request::default());
            state.show_all = true;
            state.data = serde_json::json!([{"name":"still-browsable"}]);
            state.operation_status = format!("outcomeUnknown: {diagnostic}");
            state.operation_log = "full log retained".into();
            state
                .uncertain
                .push(serde_json::json!({"profile":"owned", "error":diagnostic}));
            let original = state.operation_status.clone();
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
            terminal.draw(|frame| draw(frame, &state)).unwrap();
            let screen: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(screen.contains("still-browsable"), "{screen}");
            assert!(screen.contains("! log"), "{screen}");
            state.show_operation = true;
            let mut found = false;
            for _ in 0..40 {
                terminal.draw(|frame| draw(frame, &state)).unwrap();
                let screen: String = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                assert!(!screen.contains("resize terminal"), "{screen}");
                if screen.contains("full log retained") {
                    found = true;
                    break;
                }
                state.move_by(20);
            }
            assert!(
                found,
                "full operation log must remain reachable by scrolling"
            );
            assert_eq!(state.operation_status, original);
            assert_eq!(state.uncertain[0]["error"], diagnostic);
        }
    }

    #[test]
    fn operation_log_navigation_scrolls_text_without_moving_resource_selection() {
        let mut state = State::new(Request::default());
        state.data = serde_json::json!([{"ID":"first", "State":"running"}, {"ID":"second", "State":"running"}]);
        state.selected = Some(1);
        state.show_operation = true;
        state.move_by(20);
        assert_eq!((state.scroll, state.selected), (20, Some(1)));
        state.move_by(-30);
        assert_eq!((state.scroll, state.selected), (0, Some(1)));
        state.show_operation = false;
        state.move_by(-1);
        assert_eq!(state.selected, Some(0));
    }
    #[test]
    fn workspaces_keep_selection_and_do_not_show_vm_controls_in_kubernetes() {
        let mut containers = State::for_workspace(Request::default(), Workspace::Containers);
        containers.data = serde_json::json!([{"Names":["running"],"State":"running"}, {"Names":["stopped"],"State":"exited"}]);
        assert_eq!(containers.rows().len(), 1);
        containers.show_all = true;
        containers.selected = Some(1);
        let mut kubernetes = State::for_workspace(
            Request {
                kubeconfig: Some("/no-such-hamn-test-config".into()),
                ..Default::default()
            },
            Workspace::Kubernetes,
        );
        assert_eq!(kubernetes.request.operation(), "k8s contexts list");
        assert!(kubernetes.request.profile.is_none());
        std::mem::swap(&mut containers, &mut kubernetes);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 24)).unwrap();
        terminal.draw(|f| draw(f, &containers)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("[Kubernetes]"));
        assert!(!text.contains("VM settings") && !text.contains("Hamn profile"));
        std::mem::swap(&mut containers, &mut kubernetes);
        assert_eq!(containers.selected, Some(1));
        assert_eq!(containers.rows().len(), 2);
    }

    #[test]
    fn direct_details_keep_a_clean_list_request_for_escape_and_refresh() {
        for command in [
            "vm status",
            "vm env",
            "docker containers inspect sample",
            "docker containers stats sample",
            "docker containers logs sample --follow",
            "k8s pods inspect sample --uid original",
            "k8s pods logs sample --follow --container app --previous",
        ] {
            let mut state = State::new(Request {
                context: Some("dev".into()),
                namespace: Some("work".into()),
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
            assert!(
                state
                    .action(if state.request.words[0] == "vm" {
                        "status"
                    } else {
                        "inspect"
                    })
                    .is_ok()
            );
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
    fn explicit_external_query_hides_hamn_controls_without_changing_ui_default() {
        let mut state = State::new(Request::default());
        state.native = Some(crate::native::parse("docker --context external ps", &state).unwrap());
        assert!(!state.hamn_environment());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &state)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(!text.contains("Hamn profile"));
        assert!(!text.contains("<v> VM settings"));
        assert!(text.contains("--context external"));
        state.native = Some(crate::native::parse("ps", &state).unwrap());
        assert!(state.hamn_environment());
    }
    #[test]
    fn picker_cancel_restores_exact_query_and_view_without_rewinding_operations() {
        for (workspace, command, picker) in [
            (
                Workspace::Containers,
                "docker --host unix:///external.sock ps --filter label=app=x",
                "",
            ),
            (Workspace::Containers, "docker --context external ps -a", ""),
            (
                Workspace::Kubernetes,
                "get pods --context explicit --namespace chosen -l app=x",
                "contexts",
            ),
            (
                Workspace::Kubernetes,
                "get deployments -A --field-selector metadata.name=app",
                "ns",
            ),
        ] {
            let mut state = State::new(Request::default());
            state.workspace = workspace;
            state.request.profile = Some("default".into());
            state.request.context = Some("ui-default".into());
            state.native = Some(crate::native::parse(command, &state).unwrap());
            let original_args = state.native.as_ref().unwrap().args.clone();
            let original_request = state.request.clone();
            state.data = serde_json::json!([{"name":"first"}, {"name":"second"}]);
            let original_data = state.data.clone();
            state.selected = Some(1);
            state.filter = "row filter".into();
            state.scroll = 9;
            state.detail = Some("prior detail".into());
            state.connection_status = "Available".into();
            state.show_operation = true;
            state.open_picker();
            assert!(state.data.is_null());
            assert!(state.selected().is_none());
            assert!(state.native.is_none());
            assert!(state.filter.is_empty());
            assert_eq!((state.selected, state.scroll), (Some(0), 0));
            if !picker.is_empty() {
                state.view(picker).unwrap();
            }
            state.data = serde_json::json!([{"name":"picker entry"}]);
            state.open_picker(); // Reopening must retain the original return destination.
            state.accept(Err(Failure::new("fixture", "picker lookup failed")));
            state.runtime = serde_json::json!({"state":"running", "dockerStatus":"ready"});
            state.operation_status = "completed".into();
            state.operation_log = "new log".into();
            state
                .uncertain
                .push(serde_json::json!({"operationId":"unresolved"}));
            assert!(state.cancel_picker());
            assert!(!state.cancel_picker());
            assert_eq!(state.native.as_ref().unwrap().args, original_args);
            assert_eq!(state.request.words, original_request.words);
            assert_eq!(state.request.context, original_request.context);
            assert_eq!(state.request.profile, original_request.profile);
            assert_eq!(state.data, original_data);
            assert_eq!((state.selected, state.scroll), (Some(1), 9));
            assert_eq!(state.filter, "row filter");
            assert_eq!(state.detail.as_deref(), Some("prior detail"));
            assert_eq!(state.connection_status, "Available");
            assert!(!state.stale);
            assert!(state.show_operation);
            assert!(!state.environment_picker);
            assert_eq!(state.runtime["dockerStatus"], "ready");
            assert_eq!(state.operation_status, "completed");
            assert_eq!(state.operation_log, "new log");
            assert_eq!(state.uncertain[0]["operationId"], "unresolved");
        }
    }
    #[test]
    fn picker_commit_and_connection_reload_discard_cancel_destination() {
        for picker in ["contexts", "ns"] {
            let mut state = State::new(Request::default());
            state.workspace = Workspace::Kubernetes;
            state.request.context = Some("ui-default".into());
            state.native =
                Some(crate::native::parse("get pods --context explicit", &state).unwrap());
            state.open_picker();
            state.view(picker).unwrap();
            state.data = serde_json::json!([{"name":"chosen", "namespace":"chosen-ns",
                "metadata":{"name":"chosen-ns"}}]);
            state.filter = "chosen".into();
            state.scroll = 8;
            assert_eq!(state.enter().unwrap().unwrap().operation(), "k8s pods list");
            assert!(!state.cancel_picker());
            assert!(state.filter.is_empty());
            assert_eq!(state.scroll, 0);
            assert_eq!(state.request.namespace.as_deref(), Some("chosen-ns"));
            if picker == "contexts" {
                assert_eq!(state.request.context.as_deref(), Some("chosen"));
            }
        }
        let mut state = State::new(Request::default());
        state.native = Some(crate::native::parse("ps --filter label=old", &state).unwrap());
        state.save_browser();
        state.open_picker();
        state.invalidate_results();
        assert!(!state.cancel_picker());
        assert!(state.browser.is_none());
        assert!(state.data.is_null());
    }
    #[test]
    fn picker_cancel_restores_initial_selector_or_vm_panel_and_keeps_vm_profile_guard() {
        let mut state = State::new(Request::default());
        state.workspace = Workspace::Kubernetes;
        state.view("contexts").unwrap();
        state.open_picker();
        assert!(state.view("ns").is_err());
        assert!(state.cancel_picker());
        assert!(state.native.is_none());
        assert_eq!(state.request.operation(), "k8s contexts list");
        for profile in [None, Some("default".into())] {
            let mut state = State::new(Request {
                profile,
                ..Default::default()
            });
            state.native = Some(crate::native::parse("ps --filter label=kept", &state).unwrap());
            state.save_browser();
            state.view("vm").unwrap();
            state.open_picker();
            assert!(state.cancel_picker());
            assert!(state.native.is_none());
            assert_eq!(state.request.operation(), "vm list");
            state.return_to_browser();
            assert!(
                state
                    .native
                    .as_ref()
                    .unwrap()
                    .args
                    .iter()
                    .any(|a| a == "label=kept")
            );
            state.save_browser();
            state.view("vm").unwrap();
            state.data = serde_json::json!([{"name":"different"}]);
            state.enter().unwrap();
            state.return_to_browser();
            assert_eq!(
                state.native.unwrap().hamn_profile.as_deref(),
                Some("different")
            );
        }
    }
    #[test]
    fn runtime_panel_restores_browser_query_selection_and_filter() {
        let mut state = State::new(Request::default());
        state.request.profile = Some("default".into());
        state.native = Some(crate::native::parse("ps --filter label=app", &state).unwrap());
        state.data = serde_json::json!([{"ID":"a"}, {"ID":"b"}]);
        state.selected = Some(1);
        state.scroll = 4;
        state.filter = "b".into();
        state.save_browser();
        state.native = None;
        state.selected = Some(0);
        state.filter.clear();
        state.return_to_browser();
        assert_eq!(state.selected, Some(1));
        assert_eq!(state.scroll, 4);
        assert_eq!(state.filter, "b");
        assert!(
            state
                .native
                .as_ref()
                .unwrap()
                .args
                .iter()
                .any(|a| a == "label=app")
        );
        state.save_browser();
        state.request.profile = Some("different".into());
        state.return_to_browser();
        assert_eq!(state.selected, Some(0));
        assert!(state.filter.is_empty());
        assert_eq!(
            state.native.unwrap().hamn_profile.as_deref(),
            Some("different")
        );
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
    fn quoted_templates_preserve_backslashes_and_continuations_preserve_word_boundaries() {
        for (input, expected) in [
            (r#""a\nb\tc\.d""#, vec![r"a\nb\tc\.d"]),
            (r#""\$HOME \`literal\` \\\"""#, vec!["$HOME `literal` \\\""]),
            (r#"'a\nb' a\ b"#, vec![r"a\nb", "a b"]),
            ("one\\\ntwo \\\n three", vec!["onetwo", "three"]),
            ("\\\n", vec![]),
            ("\"\\\n\"", vec![""]),
            ("\"one\ntwo\"", vec!["one\ntwo"]),
            ("a\u{a0}b c\u{2003}d", vec!["a\u{a0}b", "c\u{2003}d"]),
            (r#""$HOME $(literal)""#, vec!["$HOME $(literal)"]),
        ] {
            assert_eq!(split_command(input).unwrap(), expected, "{input:?}");
        }
        for input in ["\"unfinished\\", "\"unfinished\\\n", "unfinished\\"] {
            assert!(split_command(input).is_err(), "{input:?}");
        }
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
    fn a_typed_vm_apply_confirms_its_file_and_a_dry_run_returns_to_a_clean_list() {
        let mut state = State::new(Request::default());
        let selected = state.request.profile.clone();
        assert!(selected.is_some());
        // The selected profile is the target; the definition must name it.
        let request = state.view("vm apply --file work.yaml").unwrap();
        assert!(request.mutates() && request.yes && request.profile == selected);
        // Wide enough that no line of the text is wrapped.
        let text = confirmation(&request, 400).join("\n");
        for line in [
            "Confirm vm apply",
            "Definition file: \"work.yaml\"",
            "settings the file omits return to their defaults",
            "The VM is not started or stopped.",
        ] {
            assert!(text.contains(line), "{line}: {text}");
        }
        assert!(text.contains(&format!("Profile: {:?}", selected.as_deref().unwrap())));
        assert!(confirmation_visible(
            &request,
            ratatui::layout::Rect::new(0, 0, 100, 24)
        ));
        assert!(state.request.file.is_none());

        // A dry run is a read. The list that Esc and the refresh return to
        // must not carry its arguments, which no list accepts.
        let dry_run = state.view("vm apply --file work.yaml --dry-run").unwrap();
        assert!(!dry_run.mutates() && dry_run.dry_run);
        assert_eq!(dry_run.file.as_deref(), Some("work.yaml"));
        assert_eq!(state.request.operation(), "vm list");
        assert!(state.request.file.is_none() && !state.request.dry_run);
        state.request.validate().unwrap();

        // The answer of the dry run is its result, shown as the detail,
        // which outlives the next refresh of the list.
        state.accept(Ok(serde_json::json!({"action":"none", "changes":[]})));
        assert!(
            state
                .detail
                .as_deref()
                .is_some_and(|text| text.contains("\"action\": \"none\""))
        );
        state.accept(Ok(serde_json::json!([{"name":"default", "state":"stopped"}])));
        assert!(state.detail.is_some() && state.message.is_empty());

        // No terminal input is a definition file.
        let refused = state.view("vm apply --file -").unwrap_err();
        assert_eq!(refused.code, "invalidRequest");
    }
    #[test]
    fn messages_break_between_words_and_keep_their_styles() {
        let text = |lines: &[Line]| -> Vec<String> {
            lines
                .iter()
                .map(|line| line.spans.iter().map(|span| span.content.as_ref()).collect())
                .collect()
        };
        let plain = |text: &str, width| wrap_words(Line::raw(text.to_owned()), width);
        assert_eq!(text(&plain("aaa bbb cccc dddddddddddd", 10)), ["aaa bbb", "cccc", "dddddddddd", "dd"]);
        assert_eq!(text(&plain("exactly ten", 11)), ["exactly ten"]);
        assert_eq!(text(&plain("", 10)), [""]);
        // Wide characters count two columns and are never split.
        assert_eq!(text(&plain("작업 상태", 6)), ["작업", "상태"]);
        assert_eq!(text(&plain("ab", 1)), ["a", "b"]);
        let styled = Line::from(vec![
            Span::styled("✗ Cannot reach", theme::level(Level::Error)),
            Span::styled(" — e selects another context", theme::dim()),
        ]);
        let lines = wrap_words(styled, 20);
        assert_eq!(text(&lines), ["✗ Cannot reach — e", "selects another", "context"]);
        assert_eq!(lines[0].spans[0].style, theme::level(Level::Error));
        assert_eq!(lines[1].spans[0].style, theme::dim());
        // On screen, the message's second line starts with a whole word.
        let mut state = State::new(Request::default());
        state.workspace = Workspace::Kubernetes;
        state.fail(Failure::new("cliError", "kubectl exited exit status: 1: The connection to the server 127.0.0.1:63621 was refused - did you specify the right host or port?\n"));
        let rendered = message_lines(&state, 132);
        assert_eq!(rendered.len(), 2);
        assert_eq!(text(&rendered)[1], "another context (! details)");
        assert!(text(&rendered)[0].ends_with("; e selects"), "{:?}", text(&rendered));
    }
    #[test]
    fn workspaces_differ_in_tab_text_and_color_and_keys_stand_out() {
        let render = |state: &State, width: u16| {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 24)).unwrap();
            terminal.draw(|frame| draw(frame, state)).unwrap();
            terminal.backend().buffer().clone()
        };
        let find = |buffer: &ratatui::buffer::Buffer, text: &str| {
            let width = usize::from(buffer.area.width);
            let cells: Vec<&str> = buffer.content().iter().map(|cell| cell.symbol()).collect();
            let chars: Vec<String> = text.chars().map(String::from).collect();
            let start = (0..cells.len())
                .find(|&i| chars.iter().enumerate().all(|(n, c)| cells.get(i + n) == Some(&c.as_str())))
                .unwrap_or_else(|| panic!("{text} not on screen"));
            buffer[((start % width) as u16, (start / width) as u16)].clone()
        };
        let mut containers = State::new(Request::default());
        containers.native = Some(crate::native::parse("ps", &containers).unwrap());
        let mut kubernetes = State::new(Request::default());
        kubernetes.workspace = Workspace::Kubernetes;
        kubernetes.request.context = Some("dev".into());
        kubernetes.native = Some(crate::native::parse("get pods", &kubernetes).unwrap());
        for width in [80, 160] {
            for (state, active, inactive) in [
                (&containers, "[Containers]", "[Kubernetes]"),
                (&kubernetes, "[Kubernetes]", "[Containers]"),
            ] {
                let accent = theme::accent(state.workspace);
                let buffer = render(state, width);
                let text: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
                assert!(!text.contains(inactive), "{text}");
                let tab = find(&buffer, active);
                assert_eq!(tab.bg, accent, "{tab:?}");
                // The frame of the view and the keys take the same color.
                assert_eq!(find(&buffer, "┌").fg, accent);
                // The row count in the title keeps the text color, not the pale border.
                let count = format!("[{}]", state.rows().len());
                assert_eq!(find(&buffer, &count).fg, Color::Reset);
                assert_eq!(find(&buffer, "<:>").bg, accent);
                assert!(text.contains("<Tab> switch") && text.contains("<?> help"), "{text}");
            }
        }
        assert_ne!(
            find(&render(&containers, 80), "┌").fg,
            find(&render(&kubernetes, 80), "┌").fg
        );
        // A failure is marked by its icon as well as its color.
        containers.fail(Failure::new("cliError", "docker exited exit status: 1: Error response from daemon: No such container: web"));
        let buffer = render(&containers, 80);
        let icon = find(&buffer, "✗ No such container: web");
        assert_eq!(icon.fg, Color::Red);
        // The command prompt opens above the view and shows the cursor.
        containers.input = Some((':', "get po".into()));
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &containers)).unwrap();
        let text: String = terminal.backend().buffer().content().iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains(" Command ") && text.contains(":get po"), "{text}");
        let cursor = terminal.get_cursor_position().unwrap();
        assert_eq!(terminal.backend().buffer()[(cursor.x - 1, cursor.y)].symbol(), "o");
        // Help covers the view and names every group of keys.
        containers.input = None;
        containers.help = true;
        let text: String = render(&containers, 80).content().iter().map(|cell| cell.symbol()).collect();
        for marker in ["GENERAL", "Commands:", "<Esc> close help", " help "] {
            assert!(text.contains(marker), "{marker}: {text}");
        }
        containers.move_by(40);
        assert_eq!(containers.scroll, 40);
        let text: String = render(&containers, 80).content().iter().map(|cell| cell.symbol()).collect();
        assert!(!text.contains("GENERAL") && text.contains("INTERNAL TERMINAL"), "{text}");
        // A detail that arrives replaces the help.
        containers.accept(Ok(serde_json::json!({"yaml": "kind: Pod"})));
        assert!(!containers.help && containers.detail.as_deref() == Some("kind: Pod"));
    }
    #[test]
    fn a_failed_read_shows_its_summary_and_keeps_the_raw_failure_for_details() {
        let screen = |state: &State| {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 32)).unwrap();
            terminal.draw(|frame| draw(frame, state)).unwrap();
            let buffer = terminal.backend().buffer().clone();
            (0..buffer.area.height)
                .map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        };
        let raw = "kubectl exited exit status: 1: E1015 01:17:26.408480   67207 memcache.go:381] \"Couldn't get current server API group list\" err=\"dial tcp 127.0.0.1:63621: connect: connection refused\"\nThe connection to the server 127.0.0.1:63621 was refused - did you specify the right host or port?\n";
        let mut state = State::new(Request::default());
        state.workspace = Workspace::Kubernetes;
        state.native = Some(crate::native::parse("get pods", &state).unwrap());
        state.accept(Ok(serde_json::json!([{"metadata":{"name":"api", "namespace":"work", "uid":"1"}}])));
        assert!(!screen(&state).contains("[stale]"));
        state.accept(Err(Failure::new("cliError", raw)));
        // The message is one sentence with the key that helps; the client
        // log lines and the failure code stay off the list screen.
        assert_eq!(
            state.message,
            "Cannot reach the Kubernetes API server at 127.0.0.1:63621: connection refused"
        );
        assert_eq!(state.message_level, Level::Error);
        let text = screen(&state);
        assert!(text.contains("Cannot reach the Kubernetes API server at 127.0.0.1:63621"), "{text}");
        assert!(text.contains("e selects another context") && text.contains("(! details)"), "{text}");
        assert!(!text.contains("cliError") && !text.contains("memcache.go"), "{text}");
        // The rows of the last good read stay and say that they may be old.
        assert!(text.contains("api") && text.contains("[stale]"), "{text}");
        assert_eq!(state.action("delete").unwrap_err().code, "staleData");
        // ! shows the failure as it was received.
        state.show_operation = true;
        let text = screen(&state);
        assert!(text.contains("Last error (cliError)") && text.contains("memcache.go:381"), "{text}");
        state.show_operation = false;
        // Another message replaces the failure; a successful read clears both.
        state.warn("Refresh before reading previous data");
        assert!(state.last_error.is_none() && state.message_level == Level::Warning);
        state.accept(Err(Failure::new("cliError", raw)));
        state.accept(Ok(serde_json::json!([])));
        assert!(state.message.is_empty() && state.last_error.is_none() && state.message_hint.is_none());
        assert!(!screen(&state).contains("[stale]"));
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
        assert_eq!(state.selected, Some(0));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(30, 10)).unwrap();
        terminal.draw(|frame| draw(frame, &state)).unwrap();
        assert!(!clean("hello\x1b[2J").contains('\x1b'));
    }
}
