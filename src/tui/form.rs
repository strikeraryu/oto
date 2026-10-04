use anyhow::{ensure, Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use oto::{protocol::DEFAULT_PORT, session::normalize_code};
use std::net::{IpAddr, SocketAddr};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Host,
    Join,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Field {
    CodeMode,
    HostPort,
    Buffer,
    Ip,
    JoinPort,
    Code,
    Start,
}

pub enum Action {
    None,
    Start,
    Quit,
    Help,
}

pub struct Input {
    pub value: String,
    pub cursor: usize,
}
impl Input {
    fn new(value: &str) -> Self {
        Self {
            value: value.into(),
            cursor: value.len(),
        }
    }
    pub fn edit(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.value.len()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.value.len(),
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                self.value.remove(self.cursor);
            }
            KeyCode::Delete if self.cursor < self.value.len() => {
                self.value.remove(self.cursor);
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.value.clear();
                self.cursor = 0;
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert(c)
            }
            _ => {}
        }
    }
    fn insert(&mut self, c: char) {
        if c.is_ascii_graphic() && self.value.len() < 64 {
            self.value.insert(self.cursor, c);
            self.cursor += 1;
        }
    }
    pub fn paste(&mut self, value: &str) {
        for c in value.trim().chars() {
            self.insert(c);
        }
    }
}

pub struct Form {
    pub mode: Mode,
    pub focus: usize,
    pub host_no_code: bool,
    pub join_no_code: bool,
    pub host_port: Input,
    pub buffer: Input,
    pub ip: Input,
    pub join_port: Input,
    pub code: Input,
}
impl Default for Form {
    fn default() -> Self {
        Self {
            mode: Mode::Host,
            focus: 0,
            host_no_code: false,
            join_no_code: false,
            host_port: Input::new(&DEFAULT_PORT.to_string()),
            buffer: Input::new("200"),
            ip: Input::new(""),
            join_port: Input::new(&DEFAULT_PORT.to_string()),
            code: Input::new(""),
        }
    }
}
impl Form {
    pub fn fields(&self) -> Vec<Field> {
        match self.mode {
            Mode::Host => vec![
                Field::CodeMode,
                Field::HostPort,
                Field::Buffer,
                Field::Start,
            ],
            Mode::Join => {
                let mut fields = vec![Field::CodeMode, Field::Ip];
                // Discovery supplies the host's port; a port field is only relevant for direct joins.
                if self.join_no_code || !self.ip.value.is_empty() {
                    fields.push(Field::JoinPort);
                }
                if !self.join_no_code {
                    fields.push(Field::Code);
                }
                fields.push(Field::Start);
                fields
            }
        }
    }
    pub fn input(&self, field: Field) -> Option<&Input> {
        match field {
            Field::HostPort => Some(&self.host_port),
            Field::Buffer => Some(&self.buffer),
            Field::Ip => Some(&self.ip),
            Field::JoinPort => Some(&self.join_port),
            Field::Code => Some(&self.code),
            _ => None,
        }
    }
    fn input_mut(&mut self, field: Field) -> Option<&mut Input> {
        match field {
            Field::HostPort => Some(&mut self.host_port),
            Field::Buffer => Some(&mut self.buffer),
            Field::Ip => Some(&mut self.ip),
            Field::JoinPort => Some(&mut self.join_port),
            Field::Code => Some(&mut self.code),
            _ => None,
        }
    }
    fn focused_field(&self) -> Option<Field> {
        self.focus
            .checked_sub(1)
            .and_then(|index| self.fields().get(index).copied())
    }
    pub fn paste(&mut self, value: &str) {
        if let Some(field) = self.focused_field() {
            if let Some(input) = self.input_mut(field) {
                input.paste(value);
            }
        }
    }
    pub fn key(&mut self, key: KeyEvent) -> Action {
        let count = self.fields().len() + 1;
        match key.code {
            KeyCode::Char('?') | KeyCode::F(3) => return Action::Help,
            KeyCode::F(1) => {
                self.mode = Mode::Host;
                self.focus = 0;
            }
            KeyCode::F(2) => {
                self.mode = Mode::Join;
                self.focus = 0;
            }
            KeyCode::Tab | KeyCode::Down => self.focus = (self.focus + 1) % count,
            KeyCode::BackTab | KeyCode::Up => self.focus = (self.focus + count - 1) % count,
            KeyCode::Esc => self.focus = 0,
            KeyCode::Char('q') if self.focus == 0 => return Action::Quit,
            KeyCode::Char('h') if self.focus == 0 => self.mode = Mode::Host,
            KeyCode::Char('j') if self.focus == 0 => self.mode = Mode::Join,
            KeyCode::Left | KeyCode::Right if self.focus == 0 => {
                self.mode = if self.mode == Mode::Host {
                    Mode::Join
                } else {
                    Mode::Host
                };
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Action::Start
            }
            _ => match self.focused_field() {
                Some(Field::CodeMode)
                    if matches!(
                        key.code,
                        KeyCode::Char(' ') | KeyCode::Enter | KeyCode::Left | KeyCode::Right
                    ) =>
                {
                    match self.mode {
                        Mode::Host => self.host_no_code = !self.host_no_code,
                        Mode::Join => self.join_no_code = !self.join_no_code,
                    }
                }
                Some(Field::Start) if key.code == KeyCode::Enter => return Action::Start,
                _ if key.code == KeyCode::Enter => self.focus = (self.focus + 1) % count,
                Some(field) => {
                    if let Some(input) = self.input_mut(field) {
                        input.edit(key);
                    }
                }
                _ => {}
            },
        }
        self.focus = self.focus.min(self.fields().len());
        Action::None
    }
    pub fn arguments(&self) -> Result<Vec<String>> {
        let port = |input: &Input| {
            input
                .value
                .parse::<u16>()
                .context("Port must be a number from 1 to 65535")
        };
        match self.mode {
            Mode::Host => {
                let port = port(&self.host_port)?;
                ensure!(port > 0, "Choose a host port from 1 to 65535");
                let buffer = self
                    .buffer
                    .value
                    .parse::<u32>()
                    .context("Buffer must be a number in milliseconds")?;
                ensure!(
                    (50..=500).contains(&buffer),
                    "Buffer must be between 50 and 500 ms"
                );
                let mut args = vec![
                    "host".into(),
                    "--port".into(),
                    port.to_string(),
                    "--buffer-ms".into(),
                    buffer.to_string(),
                ];
                if self.host_no_code {
                    args.push("--no-code".into());
                }
                Ok(args)
            }
            Mode::Join => {
                let mut args = vec!["join".into()];
                if !self.join_no_code {
                    args.push(normalize_code(&self.code.value)?);
                }
                let ip = self.ip.value.trim();
                if self.join_no_code {
                    ensure!(!ip.is_empty(), "Enter the host IP for a no-code connection");
                }
                if !ip.is_empty() {
                    let ip = ip
                        .trim_start_matches('[')
                        .trim_end_matches(']')
                        .parse::<IpAddr>()
                        .context("Enter an IP address; put its port in the separate port field")?;
                    let port = port(&self.join_port)?;
                    ensure!(port > 0, "Choose a join port from 1 to 65535");
                    args.extend(["--host".into(), SocketAddr::new(ip, port).to_string()]);
                }
                Ok(args)
            }
        }
    }
}
