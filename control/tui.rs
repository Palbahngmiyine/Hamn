use crate::preferences::{self, Workspace};
use crate::terminal_session::{Session, Event as TerminalEvent};
use crate::{
    model::{Request, Result},
    service,
    tui_state::{self, State},
};
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

struct Restore;
impl Drop for Restore {
    fn drop(&mut self) {
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste);
        ratatui::restore();
    }
}

struct Job {
    generation: u64,
    cancel: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
    sender: mpsc::Sender<(u64, bool, Result<Value>)>,
    mutation: Option<Request>,
}

fn uncertain(request: &Request) -> Value {
    serde_json::json!({"code":"outcomeUnknown", "operation":request.operation(),
        "target":request.target(), "uid":request.uid, "kubeconfig":request.kubeconfig,
        "message":"The operation may have changed the target. Inspect it before retrying; cancellation does not undo changes."})
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.cancel();
        if self.mutation.is_none() {
            if let Some(task) = &self.task { task.abort(); }
        }
        if let Some(request) = &self.mutation {
            ratatui::restore();
            eprintln!("{}", uncertain(request));
        }
    }
}

impl Job {
    fn cancel(&mut self, state: &mut State) {
        if self.mutation.is_some() { return; }
        self.cancel.cancel();
        if let Some(task) = self.task.take() {
            task.abort();
        }
        if let Some(request) = self.mutation.take() {
            state.uncertain.push(uncertain(&request));
        }
        self.generation += 1;
        state.loading = false;
    }
    fn start(&mut self, request: Request, state: &mut State) {
        if self.mutation.is_some() {
            state.message = "A change is already running; ! shows its log".into();
            return;
        }
        self.cancel(state);
        self.mutation = request.mutates().then(|| request.clone());
        self.cancel = CancellationToken::new();
        self.generation += 1;
        let generation = self.generation;
        let cancel = self.cancel.clone();
        let sender = self.sender.clone();
        state.loading = true;
        self.task = Some(tokio::spawn(async move {
            let (events, mut receiver) = mpsc::channel(32);
            let execution = service::execute_stream(&request, &cancel, Some(events));
            tokio::pin!(execution);
            let result = loop {
                tokio::select! {
                    biased;
                    Some(event) = receiver.recv() => {
                        if sender.send((generation, false, Ok(event))).await.is_err() { return; }
                    }
                    result = &mut execution => break result,
                }
            };
            while let Ok(event) = receiver.try_recv() {
                if sender.send((generation, false, Ok(event))).await.is_err() {
                    return;
                }
            }
            let _ = sender.send((generation, true, result)).await;
        }));
    }
    fn refresh(&mut self, state: &mut State) {
        if state.environment_picker {
            self.start_query(None, state);
        } else if let Some(invocation) = state.native.clone() {
            self.start_query(Some(invocation), state);
        } else { self.start(state.request.clone(), state); }
    }
    fn start_query(&mut self, invocation: Option<crate::native::Invocation>, state: &mut State) {
        let docker_config = state.docker_config.clone();
        self.cancel(state);
        self.cancel = CancellationToken::new();
        let generation = self.generation;
        let sender = self.sender.clone();
        let cancel = self.cancel.clone();
        state.loading = true;
        self.task = Some(tokio::spawn(async move {
            let result = tokio::select! {
                _ = cancel.cancelled() => return,
                result = async { match invocation {
                    Some(invocation) => {
                        if let Some(profile) = &invocation.hamn_profile {
                            let request = Request { words: vec!["vm".into(), "status".into()], profile: Some(profile.clone()), timeout: 30, ..Default::default() };
                            let (result, status) = tokio::join!(crate::native::query(&invocation), crate::core::call(&request));
                            let data = status.unwrap_or_else(|_| serde_json::json!({"state":"not created", "dockerStatus":"unavailable"}));
                            let _ = sender.send((generation, false, Ok(serde_json::json!({"type":"runtimeStatus", "data":data})))).await;
                            result
                        } else { crate::native::query(&invocation).await }
                    },
                    None => crate::environments::containers(docker_config.as_deref()).await,
                }} => result,
            };
            let _ = sender.send((generation, true, result)).await;
        }));
    }
    fn dispatch(&mut self, request: Result<Request>, state: &mut State) {
        match request {
            Ok(request) if request.mutates() => state.pending = Some(request),
            Ok(request) => self.start(request, state),
            Err(error) => state.message = format!("{}: {}", error.code, error.message),
        }
    }
}

fn resource_action(action: &str, state: &mut State, job: &mut Job, cli: &mut Option<Session>, area: ratatui::layout::Rect) {
    let result = if state.stale || state.loading { Err(crate::model::Failure::new("staleData", "refresh before acting on previous data")) }
        else { state.selected().ok_or_else(|| crate::model::Failure::new("noSelection", "select a resource first"))
            .and_then(|row| crate::native_actions::selected(state.native.as_ref().unwrap(), &row, action)) };
    match result {
        Ok(action) if action.changes => state.pending_native = Some(action),
        Ok(action) => { job.cancel(state); match Session::start(action.invocation, area.width, area.height) {
            Ok(session) => *cli = Some(session), Err(error) => state.message = error.to_string(),
        }},
        Err(error) => state.message = error.message,
    }
}

pub async fn run(request: Request) -> std::io::Result<()> {
    let preferences_path = preferences::path()?;
    let mut settings_error = String::new();
    let saved = match preferences::load(&preferences_path) {
        Ok(value) => value, Err(error) => { settings_error = error.to_string(); None }
    };
    let mut choosing = saved.is_none();
    let mut settings = false;
    let mut choice = saved.unwrap_or(Workspace::Containers).index();
    let mut state = State::for_workspace(request.clone(), saved.unwrap_or(Workspace::Containers));
    let mut other = State::for_workspace(request, if state.workspace == Workspace::Containers { Workspace::Kubernetes } else { Workspace::Containers });
    let mut terminal = ratatui::init();
    let _restore = Restore;
    crossterm::execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste)?;
    let mut cli: Option<Session> = None;
    let (sender, mut responses) = mpsc::channel(16);
    let mut job = Job {
        generation: 0,
        cancel: CancellationToken::new(),
        task: None,
        sender,
        mutation: None,
    };
    let (mutation_sender, mut mutation_responses) = mpsc::channel(64);
    let mut mutation_job = Job { generation: 0, cancel: CancellationToken::new(),
        task: None, sender: mutation_sender, mutation: None };
    let mut exit_after_cancel = false;
    if state.request.operation() != "k8s contexts list" { state.native = crate::native::parse("", &state).ok(); }
    if other.request.operation() != "k8s contexts list" { other.native = crate::native::parse("", &other).ok(); }
    if !choosing { job.refresh(&mut state); }
    let mut events = EventStream::new();
    let mut refresh = tokio::time::interval(std::time::Duration::from_secs(2));
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut suspend =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::from_raw(libc::SIGTSTP))?;
    let mut draw_error = None;
    loop {
        if let Err(error) = terminal.draw(|frame| if let Some(session) = &cli { session.draw(frame); } else if choosing { tui_state::draw_choice(frame, choice, settings, &settings_error) } else { tui_state::draw(frame, &state) }) {
            draw_error = Some(error); break;
        }
        tokio::select! {
            event = async { match cli.as_mut() { Some(session) => session.next().await, None => std::future::pending().await } } => {
                match event {
                    Ok(TerminalEvent::Output(bytes)) => {
                        let session = cli.as_mut().unwrap(); session.parser.process(&bytes);
                        let replies = std::mem::take(&mut session.parser.callbacks_mut().0);
                        if !replies.is_empty() { let _ = session.write(&replies).await; }
                    },
                    Ok(TerminalEvent::Exited(code)) => {
                        if exit_after_cancel {
                            cli.take();
                            if mutation_job.mutation.is_none() { break; }
                        }
                        state.message = format!("CLI exit code {code}; Enter returns to the list");
                    },
                    Ok(TerminalEvent::Ended) => {},
                    Err(error) => { state.message = error.to_string(); cli.take(); job.refresh(&mut state); },
                }
            },
            _ = terminate.recv() => {
                if let Some(session) = &mut cli {
                    let completed = session.exit.is_some();
                    let _ = session.signal(libc::SIGTERM);
                    mutation_job.cancel.cancel(); exit_after_cancel = true;
                    if completed { cli.take(); if mutation_job.mutation.is_none() { break; } }
                    continue;
                }
                if mutation_job.mutation.is_none() { break; }
                mutation_job.cancel.cancel(); exit_after_cancel = true;
            },
            _ = interrupt.recv() => {
                if let Some(session) = &mut cli { let _ = session.signal(libc::SIGINT); continue; }
                if mutation_job.mutation.is_none() { break; }
                state.quit_confirmation = true;
            },
            Some((generation, finished, result)) = mutation_responses.recv() => {
                if generation == mutation_job.generation {
                    let owner = if mutation_job.mutation.as_ref().is_some_and(|r| r.words.first().is_some_and(|w| w == "k8s")) { Workspace::Kubernetes } else { Workspace::Containers };
                    if finished { state.quit_confirmation = false; }
                    let progress = if state.workspace == owner { &mut state } else { &mut other };
                    if finished {
                        if let Some(request) = mutation_job.mutation.take() {
                            if result.as_ref().is_err_and(|e| e.code == "outcomeUnknown") {
                                progress.uncertain.push(uncertain(&request));
                            }
                        }
                        progress.operation_status = match &result {
                            Ok(_) => "Operation completed".into(),
                            Err(e) => format!("{}: {}", e.code, e.message),
                        };
                        if let Some(task) = mutation_job.task.take() { let _ = task.await; }
                        if exit_after_cancel { break; }
                        job.refresh(&mut state);
                    } else if let Ok(event) = result {
                        let text = event["text"].as_str().unwrap_or_default();
                        progress.operation_log.push_str(text);
                        if progress.operation_log.len() > 1024 * 1024 {
                            let mut split = progress.operation_log.len() - 1024 * 1024;
                            while !progress.operation_log.is_char_boundary(split) { split += 1; }
                            progress.operation_log.drain(..split);
                        }
                    }
                }
            },
            _ = suspend.recv() => {
                ratatui::restore();
                unsafe { libc::raise(libc::SIGSTOP); }
                terminal = ratatui::init();
            }
            Some((generation, finished, result)) = responses.recv() => {
                if generation == job.generation {
                    if finished {
                        if let Some(request) = job.mutation.take() {
                            if result.as_ref().is_err_and(|error| error.code == "outcomeUnknown") {
                                state.uncertain.push(uncertain(&request));
                            }
                        }
                    }
                    if finished && state.native.is_some() {
                        state.connection_status = if result.is_ok() { "Available" } else if state.hamn_environment() { "Connection failed; e environments, v VM controls" } else { "Connection failed; e selects the connection target" }.into();
                    }
                    state.accept(result);
                }
            }
            _ = refresh.tick() => {
                if cli.is_none() && !choosing && !state.loading && state.pending.is_none() && state.pending_native.is_none() && state.input.is_none() && state.detail.is_none() {
                    job.refresh(&mut state);
                }
            }
            event = events.next() => {
                let Some(Ok(event)) = event else { break; };
                if let Some(session) = &mut cli {
                    match event {
                        Event::Resize(width, height) => { if let Err(e) = session.resize(width, height) { state.message = e.to_string(); } },
                        Event::Paste(text) if session.exit.is_none() => {
                            let bytes = if session.parser.screen().bracketed_paste() { format!("\x1b[200~{text}\x1b[201~").into_bytes() } else { text.into_bytes() };
                            let _ = session.input(&bytes).await;
                        },
                        Event::Key(key) if key.kind == KeyEventKind::Press => {
                            if session.scroll_key(key) { continue; }
                            if session.exit.is_some() && matches!(key.code, KeyCode::Enter | KeyCode::Esc) {
                                let completed = cli.take().unwrap();
                                if completed.invocation.reset_selection {
                                    if let Err(error) = crate::environments::reload(&completed.invocation, &mut state).await { state.message = error.message; }
                                }
                                job.refresh(&mut state);
                            } else if session.exit.is_none() {
                                let bytes = crate::terminal_io::key_bytes(key, session.parser.screen().application_cursor());
                                let _ = session.input(&bytes).await;
                            }
                        },
                        _ => {},
                    }
                    continue;
                }
                if exit_after_cancel { continue; }
                if let Event::Paste(text) = &event {
                    if let Some((_, input)) = state.input.as_mut() { input.push_str(text); }
                    continue;
                }
                let Event::Key(key) = event else { continue; };
                if key.kind != KeyEventKind::Press { continue; }
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    if mutation_job.mutation.is_none() { break; }
                    state.quit_confirmation = true; continue;
                }
                if state.quit_confirmation {
                    match key.code {
                        KeyCode::Char('y') => { mutation_job.cancel.cancel(); exit_after_cancel = true;
                            state.quit_confirmation = false; state.operation_status = "Cancelling; waiting for recovery and cleanup".into(); },
                        KeyCode::Esc | KeyCode::Char('n') => state.quit_confirmation = false,
                        _ => {}
                    }
                    continue;
                }
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('z') {
                    ratatui::restore();
                    unsafe { libc::raise(libc::SIGSTOP); }
                    terminal = ratatui::init();
                    continue;
                }
                if choosing {
                    match key.code {
                        KeyCode::Up | KeyCode::Down | KeyCode::Tab => choice = 1 - choice,
                        KeyCode::Char('1') => choice = 0,
                        KeyCode::Char('2') => choice = 1,
                        KeyCode::Enter => {
                            let workspace = if choice == 0 { Workspace::Containers } else { Workspace::Kubernetes };
                            match preferences::save(&preferences_path, workspace) {
                                Ok(()) => {
                                    choosing = false; settings = false; settings_error.clear();
                                    if state.workspace != workspace { std::mem::swap(&mut state, &mut other); }
                                    job.refresh(&mut state);
                                },
                                Err(error) => settings_error = error.to_string(),
                            }
                        },
                        KeyCode::Esc if settings => { choosing = false; settings = false; },
                        KeyCode::Char('q') => {
                            if mutation_job.mutation.is_none() { break; }
                            choosing = false; state.quit_confirmation = true;
                        },
                        _ => {},
                    }
                    continue;
                }
                if let Some(action) = &state.pending_native {
                    let area = terminal.get_frame().area();
                    match key.code {
                        KeyCode::Char('y') if tui_state::native_confirmation_visible(action, area) => {
                            let action = state.pending_native.take().unwrap(); job.cancel(&mut state);
                            match Session::start(action.invocation, area.width, area.height) {
                                Ok(session) => cli = Some(session), Err(error) => state.message = error.to_string(),
                            }
                        },
                        KeyCode::Esc | KeyCode::Char('n') => state.pending_native = None,
                        _ => {},
                    }
                    continue;
                }
                if state.pending.is_some() {
                    match key.code {
                        KeyCode::Char('y') if tui_state::confirmation_visible(state.pending.as_ref().unwrap(), terminal.get_frame().area()) => {
                            let request = state.pending.take().unwrap();
                            if mutation_job.mutation.is_none() {
                                state.operation_log.clear();
                                state.operation_status = format!("{} in progress; ! shows logs", request.operation());
                                mutation_job.start(request, &mut state);
                            } else { state.message = "A change is already running".into(); }
                        },
                        KeyCode::Esc | KeyCode::Char('n') => state.pending = None,
                        _ => {}
                    }
                    continue;
                }
                if let Some((prefix, input)) = state.input.as_mut() {
                    match key.code {
                        KeyCode::Esc => state.input = None,
                        KeyCode::Backspace => { input.pop(); if *prefix == '/' { state.filter = input.clone(); state.selected = 0; } },
                        KeyCode::Char(c) => { input.push(c); if *prefix == '/' { state.filter = input.clone(); state.selected = 0; } },
                        KeyCode::Enter => {
                            let (prefix, input) = state.input.take().unwrap();
                            if prefix == ':' {
                                if input == "q" {
                                    if mutation_job.mutation.is_none() { break; }
                                    state.quit_confirmation = true; continue;
                                }
                                let legacy = input == "vm" || input.starts_with("vm ") || input.starts_with("k8s ") || input.starts_with("docker containers ") || ["contexts", "ctx"].contains(&input.as_str());
                                let target = crate::native::command_workspace(&input, state.workspace);
                                if state.workspace != target { job.cancel(&mut state); std::mem::swap(&mut state, &mut other); }
                                if legacy {
                                    let request = state.view(&input); job.dispatch(request, &mut state);
                                } else {
                                    match crate::native::parse(&input, &state) {
                                        Ok(invocation) if invocation.resource.is_some() => {
                                            state.native = Some(invocation); state.environment_picker = false;
                                            state.selected = 0; state.filter.clear(); state.detail = None; state.data = Value::Null;
                                            job.refresh(&mut state);
                                        },
                                        Ok(invocation) => {
                                            job.cancel(&mut state);
                                            let area = terminal.get_frame().area();
                                            match Session::start(invocation, area.width, area.height) {
                                                Ok(session) => cli = Some(session), Err(error) => state.message = format!("CLI unavailable: {error}"),
                                            }
                                        },
                                        Err(error) => state.message = error.message,
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') => {
                        if mutation_job.mutation.is_none() { break; }
                        state.quit_confirmation = true;
                    },
                    KeyCode::Tab => {
                        job.cancel(&mut state); std::mem::swap(&mut state, &mut other);
                        job.refresh(&mut state);
                    },
                    KeyCode::Char(',') => { choosing = true; settings = true; choice = state.workspace.index(); },
                    KeyCode::Char('a') if state.workspace == Workspace::Containers && !state.environment_picker => {
                        if let Some(invocation) = state.native.as_mut().filter(|i| i.resource.as_deref() == Some("containers")) {
                            crate::native::toggle_all(invocation); state.selected = 0; job.refresh(&mut state);
                        }
                    },
                    KeyCode::Char('c') if state.request.operation() == "vm list" && state.docker_context.is_none() => {
                        if let Some(row) = state.selected() {
                            state.input = Some((':', format!("vm configure --profile {} --cpu {} --memory {} --disk {}", row["name"].as_str().unwrap_or("default"), row["cpus"], row["memoryMiB"].as_u64().unwrap_or(4096) / 1024, row["diskGiB"])));
                        }
                    },
                    KeyCode::Char('v') if state.hamn_environment() => {
                        state.save_browser(); state.native = None; state.environment_picker = false;
                        let request = state.view("vm"); job.dispatch(request, &mut state);
                    },
                    KeyCode::Char('e') if state.workspace == Workspace::Containers => {
                        state.environment_picker = true; state.native = None; state.detail = None; state.selected = 0;
                        job.refresh(&mut state);
                    },
                    KeyCode::Char('e') if state.workspace == Workspace::Kubernetes => {
                        let request = state.view("contexts"); job.dispatch(request, &mut state);
                    },
                    KeyCode::Char('n') if state.workspace == Workspace::Kubernetes => {
                        let request = state.view("ns"); job.dispatch(request, &mut state);
                    },
                    KeyCode::Char(':') => state.input = Some((':', String::new())),
                    KeyCode::Char('/') => state.input = Some(('/', String::new())),
                    KeyCode::Esc => {
                        state.show_operation = false; state.detail = None; state.filter.clear(); job.cancel(&mut state);
                        if state.request.operation() == "vm list" || state.environment_picker {
                            state.return_to_browser(); job.refresh(&mut state);
                        }
                    },
                    KeyCode::Down | KeyCode::Char('j') => state.move_by(1),
                    KeyCode::Up | KeyCode::Char('k') => state.move_by(-1),
                    KeyCode::Enter if state.environment_picker => {
                        if let Some(row) = state.selected().filter(|r| r["disabled"] != true) {
                            if row["environmentKind"] == "docker" {
                                state.docker_context = row["name"].as_str().map(String::from);
                                state.request.profile = None;
                            } else {
                                state.docker_context = None;
                                state.request.profile = row["name"].as_str().map(String::from);
                            }
                            state.environment_picker = false; state.invalidate_results();
                            state.native = crate::native::parse("ps", &state).ok(); job.refresh(&mut state);
                        }
                    },
                    KeyCode::Enter if state.native.is_some() => resource_action("inspect", &mut state, &mut job, &mut cli, terminal.get_frame().area()),
                    KeyCode::Enter => match state.enter() {
                        Ok(Some(request)) if request.operation() == "k8s pods list" => {
                            state.native = crate::native::parse("", &state).ok(); job.refresh(&mut state);
                        },
                        Ok(Some(request)) => job.dispatch(Ok(request), &mut state),
                        Err(error) => state.message = error.message,
                        _ => {}
                    },
                    KeyCode::Char('!') => state.show_operation = !state.show_operation,
                    KeyCode::Char('?') | KeyCode::Char('m') => state.detail = Some("Commands: use : to enter Docker or kubectl commands.\nContainers: ps, ps -a, images, volume ls, network ls\nKubernetes: pods, deployments, services, get pods -A\nExplicit docker / kubectl prefixes are also accepted.\nOutput options are preserved; other commands run in the internal terminal.\n\nSelected resource: Enter detail, l logs, g stats, s start, t stop, r restart, d delete.\nChanges from this menu require confirmation; typed CLI commands run directly.\n\nTab switches workspace; , changes the default workspace.\ne chooses the environment/context; n chooses a namespace.\nv opens Hamn VM controls for a Hamn environment.\n! shows active operation logs. Esc returns. q exits.\nCLI terminal: Ctrl-C interrupts, Docker Ctrl-P Ctrl-Q detaches.\nAfter CLI exit, Enter returns and refreshes the list.\nShell pipelines, redirections and aliases are not interpreted.".into()),
                    KeyCode::Char(c) if "strdlg".contains(c) => {
                        let action = match c { 's'=>"start", 't'=>"stop", 'r'=>"restart", 'd'=>"delete", 'l'=>"logs", _=>"stats" };
                        if action == "start" && state.native.as_ref().is_some_and(|i| i.hamn_profile.is_some()) && state.runtime["dockerStatus"] != "ready" {
                            state.pending = Some(Request { words: vec!["vm".into(), "start".into()], profile: state.native.as_ref().unwrap().hamn_profile.clone(), yes: true, timeout: 600, ..Default::default() });
                        } else if state.native.is_some() {
                            resource_action(action, &mut state, &mut job, &mut cli, terminal.get_frame().area());
                        } else {
                            let request = state.action(action); job.dispatch(request, &mut state);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    cli.take();
    job.cancel(&mut state);
    if mutation_job.mutation.is_some() {
        mutation_job.cancel.cancel();
        // Keep draining progress so the worker can finish rollback before exit.
        while let Some((_, finished, result)) = mutation_responses.recv().await {
            if finished {
                if let Some(request) = mutation_job.mutation.take() {
                    if result.as_ref().is_err_and(|e| e.code == "outcomeUnknown") {
                        state.uncertain.push(uncertain(&request));
                    }
                }
                break;
            }
        }
    }
    if let Some(task) = mutation_job.task.take() { let _ = task.await; }
    drop(_restore);
    for outcome in &state.uncertain {
        eprintln!("{outcome}");
    }
    match draw_error { Some(error) => Err(error), None => Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn navigation_cannot_cancel_or_replace_a_mutation() {
        let request = Request { words: vec!["vm".into(), "start".into()],
            profile: Some("owned".into()), ..Default::default() };
        let (sender, _receiver) = mpsc::channel(1);
        let mut job = Job { generation: 7, cancel: CancellationToken::new(),
            task: None, sender, mutation: Some(request) };
        let mut state = State::new(Request::default());
        job.cancel(&mut state);
        job.start(Request::default(), &mut state);
        assert!(!job.cancel.is_cancelled());
        assert_eq!(job.generation, 7);
        assert!(state.uncertain.is_empty());
        assert_eq!(job.mutation.as_ref().unwrap().profile.as_deref(), Some("owned"));
        job.mutation = None;
        job.cancel(&mut state);
        assert!(job.cancel.is_cancelled());
    }

    #[test]
    #[ignore = "executed in a PTY by tests/host/test_tui.py; intentionally panics"]
    fn panic_restores_terminal_fixture() {
        let mut terminal = ratatui::init();
        let _restore = Restore;
        terminal
            .draw(|frame| {
                frame.render_widget(
                    ratatui::widgets::Paragraph::new("Hamn panic fixture"),
                    frame.area(),
                )
            })
            .unwrap();
        panic!("intentional terminal restoration fault");
    }
}
