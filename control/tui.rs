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
    sender: mpsc::Sender<(u64, Result<Value>)>,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

impl Job {
    fn start(&mut self, request: Request, state: &mut State) {
        self.cancel.cancel();
        if let Some(task) = self.task.take() {
            task.abort();
        }
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
                        if sender.send((generation, Ok(event))).await.is_err() { return; }
                    }
                    result = &mut execution => break result,
                }
            };
            while let Ok(event) = receiver.try_recv() {
                if sender.send((generation, Ok(event))).await.is_err() {
                    return;
                }
            }
            let _ = sender.send((generation, result)).await;
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
    let mut state = State::new(request);
    if let Ok(config) = crate::kubeconfig::load(&state.request) {
        state.request.context = state.request.context.or(config.current_context);
        if state.request.namespace.is_none() {
            state.request.namespace = config
                .contexts
                .iter()
                .find(|entry| Some(&entry.name) == state.request.context.as_ref())
                .and_then(|entry| entry.context.as_ref())
                .and_then(|context| context.namespace.clone());
        }
    }
    let mut terminal = ratatui::init();
    let _restore = Restore;
    let (sender, mut responses) = mpsc::channel(16);
    let (migration_sender, mut migrations) = mpsc::channel(16);
    let _migration = crate::migration::Startup::new(migration_sender);
    let mut job = Job {
        generation: 0,
        cancel: CancellationToken::new(),
        task: None,
        sender,
    };
    job.start(state.request.clone(), &mut state);
    let mut events = EventStream::new();
    let mut refresh = tokio::time::interval(std::time::Duration::from_secs(2));
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut suspend =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::from_raw(libc::SIGTSTP))?;
    loop {
        terminal.draw(|frame| tui_state::draw(frame, &state))?;
        tokio::select! {
            Some(message) = migrations.recv() => state.message = message,
            _ = terminate.recv() => break,
            _ = interrupt.recv() => break,
            _ = suspend.recv() => {
                ratatui::restore();
                unsafe { libc::raise(libc::SIGSTOP); }
                terminal = ratatui::init();
            }
            Some((generation, result)) = responses.recv() => {
                if generation == job.generation { state.accept(result); }
            }
            _ = refresh.tick() => {
                if !state.loading && state.pending.is_none() && state.input.is_none() && state.detail.is_none() {
                    job.start(state.request.clone(), &mut state);
                }
            }
            event = events.next() => {
                let Some(Ok(event)) = event else { break; };
                let Event::Key(key) = event else { continue; };
                if key.kind != KeyEventKind::Press { continue; }
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') { break; }
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('z') {
                    ratatui::restore();
                    unsafe { libc::raise(libc::SIGSTOP); }
                    terminal = ratatui::init();
                    continue;
                }
                if state.pending.is_some() {
                    match key.code {
                        KeyCode::Char('y') if tui_state::confirmation_visible(state.pending.as_ref().unwrap(), terminal.get_frame().area()) => {
                            let request = state.pending.take().unwrap(); job.start(request, &mut state);
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
                                if input == "q" { break; }
                                let request = state.view(&input);
                                job.dispatch(request, &mut state);
                            }
                        }
                        _ => {}
                    }
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Char(':') => state.input = Some((':', String::new())),
                    KeyCode::Char('/') => state.input = Some(('/', String::new())),
                    KeyCode::Esc => { state.detail = None; state.filter.clear(); job.cancel.cancel(); job.generation += 1; state.loading = false; },
                    KeyCode::Down | KeyCode::Char('j') => state.move_by(1),
                    KeyCode::Up | KeyCode::Char('k') => state.move_by(-1),
                    KeyCode::Enter => match state.enter() {
                        Ok(Some(request)) => job.dispatch(Ok(request), &mut state),
                        Err(error) => state.message = error.message,
                        _ => {}
                    },
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
    job.cancel.cancel();
    if let Some(task) = job.task.take() {
        task.abort();
        let _ = task.await;
    }
    Ok(())
}
