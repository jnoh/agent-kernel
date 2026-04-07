//! Ratatui-based TUI frontend for the code-agent distribution.
//!
//! Provides a Claude-Code-style terminal interface: scrollable conversation pane,
//! bordered tool-call boxes, inline permission prompts, status bar, and a
//! multiline input area at the bottom.

use crossterm::{
    ExecutableCommand,
    event::{KeyCode, KeyEvent, KeyModifiers},
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    Frame, Terminal,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use std::io;

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

/// A single entry in the conversation log.
#[derive(Clone)]
pub enum ConversationEntry {
    /// User message.
    UserInput(String),
    /// Model text output.
    AssistantText(String),
    /// A tool call (may still be running).
    ToolCall {
        tool_name: String,
        input_summary: String,
        status: ToolCallStatus,
    },
    /// Permission request awaiting user decision.
    PermissionPrompt {
        tool_name: String,
        capabilities: Vec<String>,
        input_summary: String,
    },
    /// Informational line (compaction, errors, etc).
    Info(String),
    /// Error message.
    Error(String),
}

#[derive(Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ToolCallStatus {
    Running,
    Success,
    Failed(String),
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

/// Holds the entire TUI application state.
pub struct App {
    /// Conversation entries rendered in the main pane.
    pub entries: Vec<ConversationEntry>,
    /// Current input buffer.
    pub input: String,
    /// Cursor position within the input buffer.
    pub cursor: usize,
    /// Scroll offset (in rendered lines) for the conversation pane.
    pub scroll: u16,
    /// Whether the view is pinned to the bottom (auto-scroll).
    pub follow: bool,
    /// Total rendered lines in the conversation (computed each frame).
    rendered_lines: u16,

    // --- Status bar fields ---
    pub model_name: String,
    pub total_input_tokens: usize,
    pub total_output_tokens: usize,
    pub turn_count: usize,

    /// Whether a turn is currently running.
    pub turn_active: bool,
    /// Spinner frame counter.
    pub spinner_tick: usize,

    /// Whether we are waiting for a permission response (blocks input).
    pub awaiting_permission: bool,
    /// The request_id of the pending permission (if any).
    pub pending_permission_request_id: Option<u64>,
}

impl App {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            input: String::new(),
            cursor: 0,
            scroll: 0,
            follow: true,
            rendered_lines: 0,
            model_name: "anthropic".into(),
            total_input_tokens: 0,
            total_output_tokens: 0,
            turn_count: 0,
            turn_active: false,
            spinner_tick: 0,
            awaiting_permission: false,
            pending_permission_request_id: None,
        }
    }

    // --- Input editing helpers ---

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
        self.input.clear();
        self.cursor = 0;
        text
    }

    // --- Scroll helpers ---

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
        // actual scroll value is set during render
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

pub fn draw(frame: &mut Frame, app: &mut App) {
    let size = frame.area();

    // Layout: [status_bar | conversation | input_area]
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // status bar
            Constraint::Min(5),    // conversation
            Constraint::Length(3), // input area
        ])
        .split(size);

    draw_status_bar(frame, app, chunks[0]);
    draw_conversation(frame, app, chunks[1]);
    draw_input(frame, app, chunks[2]);
}

fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let left = " agent-kernel v0.1.0".to_string();
    let right = format!(
        "{} | tokens: {}in/{}out | turns: {} ",
        app.model_name, app.total_input_tokens, app.total_output_tokens, app.turn_count,
    );

    let padding = area.width as usize
        - left.len().min(area.width as usize)
        - right.len().min(area.width as usize);
    let bar_text = format!("{left}{:width$}{right}", "", width = padding.max(1));

    let bar = Paragraph::new(Line::from(bar_text))
        .style(Style::default().bg(Color::DarkGray).fg(Color::White));
    frame.render_widget(bar, area);
}

fn draw_conversation(frame: &mut Frame, app: &mut App, area: Rect) {
    let inner_width = area.width.saturating_sub(2) as usize; // border padding

    let mut lines: Vec<Line<'_>> = Vec::new();

    for entry in &app.entries {
        match entry {
            ConversationEntry::UserInput(text) => {
                lines.push(Line::from(""));
                for l in text.lines() {
                    lines.push(Line::from(vec![
                        Span::styled(
                            "> ",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            l.to_string(),
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                }
            }

            ConversationEntry::AssistantText(text) => {
                lines.push(Line::from(""));
                for l in text.lines() {
                    lines.push(Line::from(Span::raw(l.to_string())));
                }
            }

            ConversationEntry::ToolCall {
                tool_name,
                input_summary,
                status,
            } => {
                lines.push(Line::from(""));
                let (indicator, style) = match status {
                    ToolCallStatus::Running => {
                        let ch = SPINNER[app.spinner_tick % SPINNER.len()];
                        (format!(" {ch}"), Style::default().fg(Color::Yellow))
                    }
                    ToolCallStatus::Success => (
                        " \u{2713}".to_string(), // checkmark
                        Style::default().fg(Color::Green),
                    ),
                    ToolCallStatus::Failed(msg) => {
                        (format!(" \u{2717} {msg}"), Style::default().fg(Color::Red))
                    }
                };

                // Top border
                let title = format!("\u{250c}\u{2500} {tool_name} ");
                let remaining = inner_width.saturating_sub(title.len());
                let top = format!("{title}{}\u{2510}", "\u{2500}".repeat(remaining));
                lines.push(Line::from(Span::styled(
                    top,
                    Style::default().fg(Color::DarkGray),
                )));

                // Content line(s)
                let summary = if input_summary.len() > inner_width.saturating_sub(6) {
                    format!(
                        "{}...",
                        &input_summary[..inner_width.saturating_sub(9).max(4)]
                    )
                } else {
                    input_summary.clone()
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        "\u{2502} ".to_string(),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::raw(summary),
                    Span::styled(indicator, style),
                ]));

                // Bottom border
                let bottom_inner = inner_width.saturating_sub(2);
                let bottom = format!("\u{2514}{}\u{2518}", "\u{2500}".repeat(bottom_inner));
                lines.push(Line::from(Span::styled(
                    bottom,
                    Style::default().fg(Color::DarkGray),
                )));
            }

            ConversationEntry::PermissionPrompt {
                tool_name,
                capabilities,
                input_summary,
            } => {
                lines.push(Line::from(""));
                let caps = capabilities.join(", ");
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {tool_name} "),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("requires [{caps}]"),
                        Style::default().fg(Color::Yellow),
                    ),
                ]));
                if !input_summary.is_empty() {
                    let summary = if input_summary.len() > inner_width.saturating_sub(4) {
                        format!(
                            "{}...",
                            &input_summary[..inner_width.saturating_sub(7).max(4)]
                        )
                    } else {
                        input_summary.clone()
                    };
                    lines.push(Line::from(Span::styled(
                        format!("  {summary}"),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                lines.push(Line::from(vec![
                    Span::styled(
                        "  allow? ",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        "[y/n]",
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
            }

            ConversationEntry::Info(text) => {
                lines.push(Line::from(Span::styled(
                    format!("  {text}"),
                    Style::default().fg(Color::DarkGray),
                )));
            }

            ConversationEntry::Error(text) => {
                lines.push(Line::from(Span::styled(
                    format!("  [error] {text}"),
                    Style::default().fg(Color::Red),
                )));
            }
        }
    }

    let total_lines = lines.len() as u16;
    app.rendered_lines = total_lines;

    let viewport_height = area.height.saturating_sub(2); // borders
    if app.follow {
        app.scroll = total_lines.saturating_sub(viewport_height);
    }

    let conversation = Paragraph::new(lines)
        .block(Block::default().borders(Borders::NONE))
        .wrap(Wrap { trim: false })
        .scroll((app.scroll, 0));

    frame.render_widget(conversation, area);
}

fn draw_input(frame: &mut Frame, app: &App, area: Rect) {
    let prompt = if app.awaiting_permission {
        " [y/n] "
    } else if app.turn_active {
        let ch = SPINNER[app.spinner_tick % SPINNER.len()];
        // Can't easily return owned string with lifetime, use a static prompt
        // for active turns
        Box::leak(format!(" {ch} ").into_boxed_str())
    } else {
        " > "
    };

    let input_paragraph = Paragraph::new(Line::from(vec![
        Span::styled(
            prompt.to_string(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(app.input.clone()),
    ]))
    .block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::DarkGray)),
    );

    frame.render_widget(input_paragraph, area);

    // Position cursor
    let cursor_x = area.x + prompt.len() as u16 + app.cursor as u16;
    let cursor_y = area.y + 1; // after top border
    frame.set_cursor_position((cursor_x, cursor_y));
}

// ---------------------------------------------------------------------------
// Terminal setup / teardown
// ---------------------------------------------------------------------------

pub fn init_terminal() -> io::Result<Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>> {
    terminal::enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(io::stdout());
    Terminal::new(backend)
}

pub fn restore_terminal() {
    let _ = terminal::disable_raw_mode();
    let _ = io::stdout().execute(LeaveAlternateScreen);
}

// ---------------------------------------------------------------------------
// Input actions
// ---------------------------------------------------------------------------

/// Actions that the main loop should handle after processing keyboard input.
pub enum InputAction {
    /// User submitted text (pressed Enter).
    Submit(String),
    /// User pressed y/n for a permission prompt.
    PermissionDecision(bool),
    /// User wants to cancel the current turn (Ctrl+C / Esc while turn active).
    Cancel,
    /// User wants to quit (Ctrl+C when idle, or /quit).
    Quit,
    /// No action needed (navigation, typing, etc — already handled).
    None,
}

/// Process a crossterm key event, mutate App state, and return any action
/// that the main loop needs to handle.
pub fn handle_key(app: &mut App, key: KeyEvent) -> InputAction {
    // --- Permission mode: only y/n ---
    if app.awaiting_permission {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                return InputAction::PermissionDecision(true);
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                return InputAction::PermissionDecision(false);
            }
            _ => return InputAction::None,
        }
    }

    // --- Ctrl+C ---
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        if app.turn_active {
            return InputAction::Cancel;
        } else {
            return InputAction::Quit;
        }
    }

    // --- Escape: cancel if turn active ---
    if key.code == KeyCode::Esc && app.turn_active {
        return InputAction::Cancel;
    }

    match key.code {
        KeyCode::Enter => {
            let text = app.take_input();
            if text.is_empty() {
                return InputAction::None;
            }
            if text == "/quit" || text == "/exit" {
                return InputAction::Quit;
            }
            InputAction::Submit(text)
        }
        KeyCode::Char(c) => {
            app.insert_char(c);
            InputAction::None
        }
        KeyCode::Backspace => {
            app.backspace();
            InputAction::None
        }
        KeyCode::Delete => {
            app.delete();
            InputAction::None
        }
        KeyCode::Left => {
            app.move_left();
            InputAction::None
        }
        KeyCode::Right => {
            app.move_right();
            InputAction::None
        }
        KeyCode::Home => {
            app.move_home();
            InputAction::None
        }
        KeyCode::End => {
            app.move_end();
            InputAction::None
        }
        KeyCode::Up => {
            app.scroll_up(3);
            InputAction::None
        }
        KeyCode::Down => {
            app.scroll_down(3, 20); // approximate; exact viewport set during draw
            InputAction::None
        }
        KeyCode::PageUp => {
            app.scroll_up(20);
            InputAction::None
        }
        KeyCode::PageDown => {
            app.scroll_down(20, 20);
            InputAction::None
        }
        _ => InputAction::None,
    }
}
