mod form;
mod view;

use anyhow::{ensure, Context, Result};
use crossterm::{
    cursor::Show,
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use form::{Action, Form};
use oto::{
    audio::{self, Device},
    session::{self, LocalCommand, LocalResponse, Status},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{
    collections::VecDeque,
    io::{self, IsTerminal},
    os::unix::process::CommandExt,
    process::Stdio,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    sync::mpsc,
    task::JoinHandle,
};

const MAX_LOGS: usize = 500;
type UiResult<T> = std::result::Result<T, String>;

enum Update {
    Status(UiResult<Option<Status>>),
    Log(String),
    Command {
        purpose: &'static str,
        result: UiResult<LocalResponse>,
    },
    Devices(UiResult<Vec<Device>>),
}
enum Request {
    Command {
        purpose: &'static str,
        command: LocalCommand,
    },
    Devices,
}

pub(super) struct LogEntry {
    pub time: String,
    pub text: String,
}
pub(super) enum Modal {
    Help,
    Addresses {
        selected: usize,
    },
    Quit {
        stop: bool,
    },
    Outputs {
        devices: Vec<Device>,
        selected: usize,
        loaded: bool,
    },
}

struct ManagedSession {
    child: Child,
    readers: Vec<JoinHandle<()>>,
    stop_started: Option<Instant>,
}
impl Drop for ManagedSession {
    fn drop(&mut self) {
        for reader in &self.readers {
            reader.abort();
        }
    }
}

pub(super) struct App {
    pub form: Form,
    pub status: Option<Status>,
    managed: Option<ManagedSession>,
    pub modal: Option<Modal>,
    pub logs: VecDeque<LogEntry>,
    pub log_scroll: usize,
    pub addresses: Vec<String>,
    pub notice: String,
    pub notice_error: bool,
    pub action_focus: usize,
    pub stopping: bool,
    pub quitting: bool,
    pub checking: bool,
    pub pending_commands: usize,
    pub device_pending: bool,
    pub requested_role: &'static str,
    requests: mpsc::Sender<Request>,
    updates: mpsc::Sender<Update>,
}
impl App {
    pub fn active(&self) -> bool {
        self.status.is_some() || self.managed.is_some()
    }
    fn log(&mut self, text: impl AsRef<str>) {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as libc::time_t;
        let mut local = unsafe { std::mem::zeroed::<libc::tm>() };
        unsafe {
            libc::localtime_r(&seconds, &mut local);
        }
        let time = format!(
            "{:02}:{:02}:{:02}",
            local.tm_hour, local.tm_min, local.tm_sec
        );
        for line in text.as_ref().lines().filter(|line| !line.trim().is_empty()) {
            // Remote host/device names and subprocess output cannot emit terminal control codes.
            let text = line
                .chars()
                .filter(|c| !c.is_control())
                .take(1024)
                .collect();
            if self.log_scroll > 0 {
                self.log_scroll += 1;
            }
            self.logs.push_back(LogEntry {
                time: time.clone(),
                text,
            });
            if self.logs.len() > MAX_LOGS {
                self.logs.pop_front();
            }
        }
        self.log_scroll = self.log_scroll.min(self.logs.len().saturating_sub(1));
    }
    fn message(&mut self, text: impl Into<String>, error: bool) {
        self.notice = text.into();
        self.notice_error = error;
        self.log(self.notice.clone());
    }
    fn send(&mut self, purpose: &'static str, command: LocalCommand) -> bool {
        if self
            .requests
            .try_send(Request::Command { purpose, command })
            .is_ok()
        {
            self.pending_commands += 1;
            true
        } else {
            self.message("Controls are busy; try again in a moment", true);
            false
        }
    }
    fn set_status(&mut self, next: Option<Status>) {
        if let Some(next) = &next {
            if self.status.is_none() {
                self.log(format!("Session available: {} • {}", next.role, next.state));
            } else if let Some(previous) = &self.status {
                let mut changes = Vec::new();
                if previous.state != next.state {
                    changes.push(format!("Session: {}", next.state));
                }
                if previous.output_uid != next.output_uid || previous.output != next.output {
                    changes.push(format!(
                        "Output: {} • speaker delay {} ms",
                        next.output, next.latency_ms
                    ));
                }
                if previous.clients != next.clients {
                    changes.push(format!("Connected clients: {}", next.clients.len()));
                }
                if previous.compensation_ms != next.compensation_ms
                    || previous.target_delay_ms != next.target_delay_ms
                {
                    changes.push(format!(
                        "Automatic added delay: {} ms • slowest speaker: {} ms",
                        next.compensation_ms, next.target_delay_ms
                    ));
                }
                for change in changes {
                    self.log(change);
                }
            }
        } else if self.status.is_some() {
            self.log("Session ended");
        }
        self.status = next;
        if !self.active() {
            self.stopping = false;
        }
    }
    fn update(&mut self, update: Update) {
        match update {
            Update::Log(line) => self.log(line),
            Update::Status(Ok(status)) => {
                self.checking = false;
                self.set_status(status);
            }
            Update::Status(Err(error)) => {
                self.checking = false;
                if self.notice != error {
                    self.message(error, true);
                }
            }
            Update::Command { purpose, result } => {
                self.pending_commands = self.pending_commands.saturating_sub(1);
                match result {
                    Ok(response) if response.ok => {
                        let message = if purpose == "Latency" {
                            response
                                .status
                                .as_ref()
                                .map(|status| {
                                    format!(
                                        "{} speaker delay: {} ms",
                                        status.output, status.latency_ms
                                    )
                                })
                                .unwrap_or_else(|| "Speaker delay updated".into())
                        } else {
                            format!("{purpose}: {}", response.message)
                        };
                        self.set_status(response.status);
                        self.message(message, false);
                    }
                    result => {
                        let error = match result {
                            Ok(response) => response.message,
                            Err(error) => error,
                        };
                        self.message(error, true);
                        if purpose == "Stop" && self.managed.is_none() {
                            self.stopping = false;
                            self.quitting = false;
                        }
                    }
                }
                if purpose == "Output" {
                    self.device_pending = false;
                }
            }
            Update::Devices(result) => match result {
                Ok(devices) => {
                    if let Some(Modal::Outputs {
                        devices: list,
                        selected,
                        loaded,
                    }) = &mut self.modal
                    {
                        *selected = self
                            .status
                            .as_ref()
                            .filter(|status| !status.follow_default)
                            .and_then(|status| {
                                devices.iter().position(|device| {
                                    Some(&device.uid) == status.output_uid.as_ref()
                                })
                            })
                            .map(|index| index + 1)
                            .unwrap_or(0);
                        *list = devices;
                        *loaded = true;
                    }
                }
                Err(error) => {
                    self.modal = None;
                    self.message(error, true);
                }
            },
        }
    }
    fn start(&mut self) -> Result<()> {
        ensure!(
            !self.checking,
            "Checking for an existing session; try again in a moment"
        );
        ensure!(
            !self.active(),
            "Stop the current session before starting another"
        );
        let args = self.form.arguments()?;
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // Keep session signals independent of the terminal's foreground process group.
        command.as_std_mut().process_group(0);
        let mut child = command
            .spawn()
            .context("Could not start the audio session")?;
        let stdout = child.stdout.take().context("Missing session output pipe")?;
        let stderr = child.stderr.take().context("Missing session error pipe")?;
        let readers = vec![
            read_logs(stdout, self.updates.clone()),
            read_logs(stderr, self.updates.clone()),
        ];
        self.requested_role = if args[0] == "host" {
            "Hosting"
        } else {
            "Joining"
        };
        self.managed = Some(ManagedSession {
            child,
            readers,
            stop_started: None,
        });
        self.stopping = false;
        self.quitting = false;
        self.log_scroll = 0;
        self.message(
            format!(
                "{} started; waiting for the audio engine",
                self.requested_role
            ),
            false,
        );
        Ok(())
    }
    fn stop(&mut self) {
        if !self.active() || self.stopping {
            return;
        }
        self.stopping = true;
        if let Some(managed) = &mut self.managed {
            managed.stop_started = Some(Instant::now());
            if self.status.as_ref().map(|status| status.pid) != managed.child.id() {
                // Startup may be waiting for macOS permission and cannot serve IPC yet.
                if let Some(pid) = managed.child.id() {
                    unsafe {
                        libc::kill(pid as i32, libc::SIGTERM);
                    }
                }
                self.message("Stopping the starting session…", false);
                return;
            }
        }
        if !self.send("Stop", LocalCommand::Leave) {
            if self.managed.is_none() {
                self.stopping = false;
                self.quitting = false;
            }
            return;
        }
        self.notice = "Stopping session…".into();
        self.notice_error = false;
    }
    fn quit(&mut self) {
        if self.managed.is_some() || self.status.is_some() {
            self.modal = Some(Modal::Quit { stop: true });
        } else {
            self.quitting = true;
        }
    }
    fn adjust(&mut self, delta: Option<i32>) {
        if self.stopping || self.device_pending {
            return;
        }
        if let Some(status) = &self.status {
            let output_uid = status.output_uid.clone();
            self.send(
                "Latency",
                match delta {
                    Some(delta_ms) => LocalCommand::AdjustLatency {
                        delta_ms,
                        output_uid,
                    },
                    None => LocalCommand::ResetLatency { output_uid },
                },
            );
        }
    }
    fn outputs(&mut self) {
        if self.stopping || self.device_pending || self.pending_commands > 0 {
            return;
        }
        self.modal = Some(Modal::Outputs {
            devices: Vec::new(),
            selected: 0,
            loaded: false,
        });
        if self.requests.try_send(Request::Devices).is_err() {
            self.modal = None;
            self.message("Controls are busy; try again", true);
        }
    }
    fn modal_key(&mut self, key: KeyEvent) {
        match self.modal.as_mut().unwrap() {
            Modal::Addresses { selected } => match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::F(4) => self.modal = None,
                KeyCode::Up | KeyCode::BackTab => *selected = selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Tab => {
                    *selected = (*selected + 1).min(self.addresses.len().saturating_sub(1))
                }
                _ => {}
            },
            Modal::Help => {
                if matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') | KeyCode::Char('q')
                ) {
                    self.modal = None;
                }
            }
            Modal::Quit { stop } => match key.code {
                KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => *stop = !*stop,
                KeyCode::Esc | KeyCode::Char('n') => self.modal = None,
                KeyCode::Char('y') => {
                    self.modal = None;
                    self.quitting = true;
                    self.stop();
                }
                KeyCode::Enter if *stop => {
                    self.modal = None;
                    self.quitting = true;
                    self.stop();
                }
                KeyCode::Enter => self.modal = None,
                KeyCode::Char('d') if self.managed.is_none() => {
                    self.modal = None;
                    self.status = None;
                    self.quitting = true;
                }
                _ => {}
            },
            Modal::Outputs {
                devices,
                selected,
                loaded,
            } => match key.code {
                KeyCode::Esc => self.modal = None,
                KeyCode::Up | KeyCode::BackTab => {
                    *selected = (*selected + devices.len()) % (devices.len() + 1)
                }
                KeyCode::Down | KeyCode::Tab => *selected = (*selected + 1) % (devices.len() + 1),
                KeyCode::Enter if *loaded => {
                    let (uid, name) = if *selected == 0 {
                        (None, "System default".into())
                    } else {
                        (
                            Some(devices[*selected - 1].uid.clone()),
                            devices[*selected - 1].name.clone(),
                        )
                    };
                    self.modal = None;
                    self.device_pending = self.send("Output", LocalCommand::Device { uid, name });
                }
                _ => {}
            },
        }
    }
    fn key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if self.modal.is_some() {
                self.modal = None;
            } else {
                self.quit();
            }
            return;
        }
        if self.modal.is_some() {
            self.modal_key(key);
            return;
        }
        if key.code == KeyCode::F(4) {
            self.modal = Some(Modal::Addresses { selected: 0 });
            return;
        }
        match key.code {
            KeyCode::PageUp => {
                self.log_scroll = (self.log_scroll + 5).min(self.logs.len().saturating_sub(1));
                return;
            }
            KeyCode::PageDown => {
                self.log_scroll = self.log_scroll.saturating_sub(5);
                return;
            }
            _ => {}
        }
        if self.active() {
            match key.code {
                KeyCode::Char('+') | KeyCode::Char('=') => self.adjust(Some(10)),
                KeyCode::Char('-') => self.adjust(Some(-10)),
                KeyCode::Char(']') => self.adjust(Some(1)),
                KeyCode::Char('[') => self.adjust(Some(-1)),
                KeyCode::Char('0') => self.adjust(None),
                KeyCode::Char('d') => self.outputs(),
                KeyCode::Char('s') => self.stop(),
                KeyCode::Char('q') | KeyCode::Esc => self.quit(),
                KeyCode::Char('?') | KeyCode::F(3) => self.modal = Some(Modal::Help),
                KeyCode::End => self.log_scroll = 0,
                KeyCode::Tab | KeyCode::Right | KeyCode::Down => {
                    self.action_focus = (self.action_focus + 1) % 3
                }
                KeyCode::BackTab | KeyCode::Left | KeyCode::Up => {
                    self.action_focus = (self.action_focus + 2) % 3
                }
                KeyCode::Enter => match self.action_focus {
                    0 => self.outputs(),
                    1 => self.stop(),
                    _ => self.quit(),
                },
                _ => {}
            }
        } else {
            match self.form.key(key) {
                Action::Start => {
                    if let Err(error) = self.start() {
                        self.message(format!("{error:#}"), true);
                    }
                }
                Action::Quit => self.quit(),
                Action::Help => self.modal = Some(Modal::Help),
                Action::None => {}
            }
        }
    }
    fn check_child(&mut self) -> Result<()> {
        let Some(managed) = &mut self.managed else {
            return Ok(());
        };
        let pid = managed.child.id();
        if let Some(exit) = managed.child.try_wait()? {
            // Let pipe readers drain the final error lines now that the process exited.
            managed.readers.clear();
            let intentional = self.stopping;
            // Do not clear a different session that won the local lock while starting.
            if self
                .status
                .as_ref()
                .is_some_and(|status| Some(status.pid) == pid)
            {
                self.set_status(None);
            }
            self.managed = None;
            self.stopping = false;
            if exit.success() || intentional {
                self.message("Session stopped", false);
            } else {
                self.message(
                    format!("Session could not continue ({exit}); see logs below"),
                    true,
                );
            }
        } else if let Some(started) = managed.stop_started {
            if started.elapsed() > Duration::from_secs(7) {
                let _ = managed.child.start_kill();
            } else if started.elapsed() > Duration::from_secs(4) {
                if let Some(pid) = pid {
                    unsafe {
                        libc::kill(pid as i32, libc::SIGTERM);
                    }
                }
            }
        }
        Ok(())
    }
    async fn cleanup(&mut self) {
        if let Some(mut managed) = self.managed.take() {
            let ours = self
                .status
                .as_ref()
                .is_some_and(|status| Some(status.pid) == managed.child.id());
            if ours {
                let _ = tokio::time::timeout(
                    Duration::from_secs(1),
                    session::local_command(&LocalCommand::Leave),
                )
                .await;
            } else if let Some(pid) = managed.child.id() {
                unsafe {
                    libc::kill(pid as i32, libc::SIGTERM);
                }
            }
            if tokio::time::timeout(Duration::from_secs(3), managed.child.wait())
                .await
                .is_err()
            {
                let _ = managed.child.kill().await;
            }
        }
    }
}

fn read_logs(
    reader: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    updates: mpsc::Sender<Update>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if updates.send(Update::Log(line)).await.is_err() {
                break;
            }
        }
    })
}
fn controller(
    mut requests: mpsc::Receiver<Request>,
    updates: mpsc::Sender<Update>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut poll = tokio::time::interval(Duration::from_millis(500));
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let update = tokio::select! {
                request = requests.recv() => match request {
                    Some(Request::Command { purpose, command }) => Update::Command { purpose, result: session::local_command(&command).await.map_err(|error| format!("{error:#}")) },
                    Some(Request::Devices) => Update::Devices(audio::devices().await.map_err(|error| format!("{error:#}"))),
                    None => break,
                },
                _ = poll.tick() => {
                    let result = match session::local_command(&LocalCommand::Status).await {
                        Ok(response) if response.ok => Ok(response.status),
                        Ok(response) => Err(response.message),
                        Err(error) if super::session_missing(&error) => Ok(None),
                        Err(error) => Err(format!("{error:#}")),
                    };
                    Update::Status(result)
                },
            };
            if updates.send(update).await.is_err() {
                break;
            }
        }
    })
}

struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            LeaveAlternateScreen,
            Show
        );
    }
}

pub async fn run() -> Result<()> {
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "The TUI needs an interactive terminal. Use oto --help for CLI commands."
    );
    let (update_tx, mut update_rx) = mpsc::channel(256);
    let (request_tx, request_rx) = mpsc::channel(32);
    let addresses = local_addresses();
    let mut app = App {
        form: Form::default(),
        status: None,
        managed: None,
        modal: None,
        logs: VecDeque::new(),
        log_scroll: 0,
        addresses,
        notice: "Choose Host or Join. Tab moves between fields; Enter selects.".into(),
        notice_error: false,
        action_focus: 0,
        stopping: false,
        quitting: false,
        checking: true,
        pending_commands: 0,
        device_pending: false,
        requested_role: "Starting",
        requests: request_tx,
        updates: update_tx.clone(),
    };
    enable_raw_mode()?;
    let guard = TerminalGuard;
    execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste)?;
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            LeaveAlternateScreen,
            Show
        );
        previous_hook(panic);
    }));
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let controller = controller(request_rx, update_tx);
    let mut refresh = tokio::time::interval(Duration::from_millis(33));
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut address_refresh = Instant::now();
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let result: Result<()> = async {
        loop {
            tokio::select! {
                _ = refresh.tick() => {},
                _ = terminate.recv() => break,
                _ = tokio::signal::ctrl_c() => { app.quit(); },
            }
            while let Ok(update) = update_rx.try_recv() {
                app.update(update);
            }
            app.check_child()?;
            if app.quitting && app.managed.is_none() && !app.stopping {
                break;
            }
            if address_refresh.elapsed() > Duration::from_secs(5) {
                app.addresses = local_addresses();
                address_refresh = Instant::now();
            }
            terminal.draw(|frame| view::draw(frame, &app))?;
            // poll/read stay on this task; zero timeout keeps session updates responsive.
            while event::poll(Duration::ZERO)? {
                match event::read()? {
                    Event::Key(key) => app.key(key),
                    Event::Paste(text) if !app.active() && app.modal.is_none() => {
                        app.form.paste(&text)
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
    .await;
    app.cleanup().await;
    controller.abort();
    drop(guard);
    result
}

fn local_addresses() -> Vec<String> {
    let mut addresses = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|interface| !interface.is_loopback() && !interface.is_link_local())
        .map(|interface| (interface.name.clone(), interface.ip()))
        .collect::<Vec<_>>();
    addresses.sort_by_key(|(name, ip)| (!ip.is_ipv4(), !name.starts_with("en"), name.clone(), *ip));
    addresses.dedup();
    addresses
        .into_iter()
        .map(|(name, ip)| format!("{ip} ({name})"))
        .collect()
}
