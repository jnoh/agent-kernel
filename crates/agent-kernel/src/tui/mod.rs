mod blocks;
mod conversation;
mod input;
mod status_bar;
pub mod theme;
pub mod types;

pub use theme::Theme;
pub use types::{ConversationEntry, InputAction, SlashCommand, ToolCallStatus};

use crossterm::{
    ExecutableCommand,
    event::{DisableMouseCapture, EnableMouseCapture},
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    Frame, Terminal,
    layout::{Constraint, Direction, Layout},
};

use std::io;

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

pub struct App {
    pub theme: Theme,
    pub entries: Vec<ConversationEntry>,
    pub input: String,
    pub cursor: usize,
    pub scroll: u16,
    pub follow: bool,
    pub(crate) rendered_lines: u16,
    pub(crate) viewport_height: u16,
    pub dirty: bool,

    pub model_name: String,
    pub total_input_tokens: usize,
    pub total_output_tokens: usize,
    pub turn_count: usize,
    pub context_tokens: usize,
    pub context_window: usize,

    pub turn_active: bool,
    pub spinner_tick: usize,

    pub awaiting_permission: bool,
    pub pending_permission_request_id: Option<u64>,
    pub pending_permission_capabilities: Option<Vec<String>>,
    pub status_requested: bool,

    history: Vec<String>,
    history_index: Option<usize>,
    history_stash: String,
}

impl App {
    pub fn new() -> Self {
        Self {
            theme: Theme::default(),
            entries: Vec::new(),
            input: String::new(),
            cursor: 0,
            scroll: 0,
            follow: true,
            rendered_lines: 0,
            viewport_height: 20,
            dirty: true,
            model_name: "anthropic".into(),
            total_input_tokens: 0,
            total_output_tokens: 0,
            turn_count: 0,
            context_tokens: 0,
            context_window: 200_000,
            turn_active: false,
            spinner_tick: 0,
            awaiting_permission: false,
            pending_permission_request_id: None,
            pending_permission_capabilities: None,
            status_requested: false,
            history: Vec::new(),
            history_index: None,
            history_stash: String::new(),
        }
    }

    pub fn insert_char(&mut self, c: char) {
        self.input.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            let prev = self.input[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.input.drain(prev..self.cursor);
            self.cursor = prev;
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.input.len() {
            let next = self.input[self.cursor..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| self.cursor + i)
                .unwrap_or(self.input.len());
            self.input.drain(self.cursor..next);
        }
    }

    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.input[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.input.len() {
            self.cursor = self.input[self.cursor..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| self.cursor + i)
                .unwrap_or(self.input.len());
        }
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.input.len();
    }

    pub fn take_input(&mut self) -> String {
        let text = self.input.clone();
        if !text.is_empty() {
            self.history.push(text.clone());
        }
        self.input.clear();
        self.cursor = 0;
        self.history_index = None;
        self.history_stash.clear();
        text
    }

    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        match self.history_index {
            None => {
                self.history_stash = self.input.clone();
                let idx = self.history.len() - 1;
                self.history_index = Some(idx);
                self.input = self.history[idx].clone();
            }
            Some(idx) if idx > 0 => {
                let idx = idx - 1;
                self.history_index = Some(idx);
                self.input = self.history[idx].clone();
            }
            _ => {}
        }
        self.cursor = self.input.len();
    }

    pub fn history_next(&mut self) {
        match self.history_index {
            Some(idx) if idx + 1 < self.history.len() => {
                let idx = idx + 1;
                self.history_index = Some(idx);
                self.input = self.history[idx].clone();
            }
            Some(_) => {
                self.history_index = None;
                self.input = self.history_stash.clone();
                self.history_stash.clear();
            }
            None => {}
        }
        self.cursor = self.input.len();
    }

    pub fn scroll_up(&mut self, lines: u16) {
        self.scroll = self.scroll.saturating_sub(lines);
        self.follow = false;
    }

    pub fn scroll_down(&mut self, lines: u16, viewport_height: u16) {
        let max = self.rendered_lines.saturating_sub(viewport_height);
        self.scroll = (self.scroll + lines).min(max);
        if self.scroll >= max {
            self.follow = true;
        }
    }

    pub fn scroll_to_bottom(&mut self) {
        self.follow = true;
    }
}

// ---------------------------------------------------------------------------
// Top-level draw + terminal lifecycle
// ---------------------------------------------------------------------------

pub fn draw(frame: &mut Frame, app: &mut App) {
    let size = frame.area();
    let input_line_count = app.input.lines().count().clamp(1, 8) as u16;
    let input_height = input_line_count + 1;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(input_height),
        ])
        .split(size);

    status_bar::draw(frame, app, chunks[0]);
    conversation::draw(frame, app, chunks[1]);
    input::draw(frame, app, chunks[2]);
}

pub fn init_terminal() -> io::Result<Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>> {
    terminal::enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    io::stdout().execute(EnableMouseCapture)?;
    let backend = ratatui::backend::CrosstermBackend::new(io::stdout());
    Terminal::new(backend)
}

pub fn restore_terminal() {
    let _ = io::stdout().execute(DisableMouseCapture);
    let _ = terminal::disable_raw_mode();
    let _ = io::stdout().execute(LeaveAlternateScreen);
}

pub fn handle_key(app: &mut App, key: crossterm::event::KeyEvent) -> InputAction {
    input::handle_key(app, key)
}

pub fn handle_mouse(app: &mut App, mouse: crossterm::event::MouseEvent) -> InputAction {
    input::handle_mouse(app, mouse)
}
