use crate::preferences::{self, Workspace};
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
    fn dispatch(&mut self, request: Result<Request>, state: &mut State) {
        match request {
            Ok(request) if request.mutates() => state.pending = Some(request),
            Ok(request) => self.start(request, state),
            Err(error) => state.message = format!("{}: {}", error.code, error.message),
        }
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
    if !choosing { job.start(state.request.clone(), &mut state); }
    let mut events = EventStream::new();
    let mut refresh = tokio::time::interval(std::time::Duration::from_secs(2));
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut suspend =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::from_raw(libc::SIGTSTP))?;
    let mut draw_error = None;
    loop {
        if let Err(error) = terminal.draw(|frame| if choosing { tui_state::draw_choice(frame, choice, settings, &settings_error) } else { tui_state::draw(frame, &state) }) {
            draw_error = Some(error); break;
        }
        tokio::select! {
            _ = terminate.recv() => {
                if mutation_job.mutation.is_none() { break; }
                mutation_job.cancel.cancel(); exit_after_cancel = true;
            },
            _ = interrupt.recv() => {
                if mutation_job.mutation.is_none() { break; }
                state.quit_confirmation = true;
            },
            Some((generation, finished, result)) = mutation_responses.recv() => {
                if generation == mutation_job.generation {
                    if finished {
                        state.quit_confirmation = false;
                        if let Some(request) = mutation_job.mutation.take() {
                            if result.as_ref().is_err_and(|e| e.code == "outcomeUnknown") {
                                state.uncertain.push(uncertain(&request));
                            }
                        }
                        state.operation_status = match &result {
                            Ok(_) => "Operation completed".into(),
                            Err(e) => format!("{}: {}", e.code, e.message),
                        };
                        if let Some(task) = mutation_job.task.take() { let _ = task.await; }
                        if exit_after_cancel { break; }
                        job.start(state.request.clone(), &mut state);
                    } else if let Ok(event) = result {
                        let text = event["text"].as_str().unwrap_or_default();
                        state.operation_log.push_str(text);
                        if state.operation_log.len() > 1024 * 1024 {
                            let mut split = state.operation_log.len() - 1024 * 1024;
                            while !state.operation_log.is_char_boundary(split) { split += 1; }
                            state.operation_log.drain(..split);
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
                    state.accept(result);
                }
            }
            _ = refresh.tick() => {
                if !choosing && !state.loading && state.pending.is_none() && state.input.is_none() && state.detail.is_none() {
                    job.start(state.request.clone(), &mut state);
                }
            }
            event = events.next() => {
                let Some(Ok(event)) = event else { break; };
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
                                    job.start(state.request.clone(), &mut state);
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
                                let target = if input.starts_with("k8s ") || ["contexts", "ctx", "pods", "po", "ns", "deployments", "services"].contains(&input.as_str()) {
                                    Workspace::Kubernetes
                                } else if input.starts_with("vm ") || input == "vm" || input.starts_with("docker ") || ["containers", "images", "volumes", "networks"].contains(&input.as_str()) {
                                    Workspace::Containers
                                } else { state.workspace };
                                if state.workspace != target {
                                    job.cancel(&mut state);
                                    std::mem::swap(&mut state, &mut other);
                                }
                                let request = state.view(&input);
                                job.dispatch(request, &mut state);
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
                        job.start(state.request.clone(), &mut state);
                    },
                    KeyCode::Char(',') => { choosing = true; settings = true; choice = state.workspace.index(); },
                    KeyCode::Char('a') if state.workspace == Workspace::Containers => { state.show_all = !state.show_all; state.selected = 0; },
                    KeyCode::Char('v') if state.workspace == Workspace::Containers && state.docker_context.is_none() => {
                        let request = state.view("vm"); job.dispatch(request, &mut state);
                    },
                    KeyCode::Char('e') if state.workspace == Workspace::Kubernetes => {
                        let request = state.view("contexts"); job.dispatch(request, &mut state);
                    },
                    KeyCode::Char('n') if state.workspace == Workspace::Kubernetes => {
                        let request = state.view("ns"); job.dispatch(request, &mut state);
                    },
                    KeyCode::Char(':') => state.input = Some((':', String::new())),
                    KeyCode::Char('/') => state.input = Some(('/', String::new())),
                    KeyCode::Esc => { state.show_operation = false; state.detail = None; state.filter.clear(); job.cancel(&mut state); },
                    KeyCode::Down | KeyCode::Char('j') => state.move_by(1),
                    KeyCode::Up | KeyCode::Char('k') => state.move_by(-1),
                    KeyCode::Enter => match state.enter() {
                        Ok(Some(request)) => job.dispatch(Ok(request), &mut state),
                        Err(error) => state.message = error.message,
                        _ => {}
                    },
                    KeyCode::Char('!') => state.show_operation = !state.show_operation,
                    KeyCode::Char('?') => state.detail = Some("Commands: :vm :containers :images :volumes :networks :contexts :ns :pods :deployments :sts :ds :services :nodes :events :jobs :cronjobs :ingresses :pvcs\n\nUse a full headless operation after ':' for configuration and scaling.\nExample: :vm create --profile work --cpu 2 --memory 4\nExample: :k8s deployments scale api --replicas 3 --namespace default\n\nEnter selects context/namespace/profile. Mutations require y confirmation. Esc returns to the list. Ctrl-Z suspends; q exits without stopping VMs.".into()),
                    KeyCode::Char(c) if "strdlg".contains(c) => {
                        let action = match c { 's'=>"start", 't'=>"stop", 'r'=>"restart", 'd'=>"delete", 'l'=>"logs", _=>"stats" };
                        let request = state.action(action);
                        job.dispatch(request, &mut state);
                    }
                    _ => {}
                }
            }
        }
    }
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
