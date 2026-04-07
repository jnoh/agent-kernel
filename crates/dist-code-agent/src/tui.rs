//! Ratatui-based TUI frontend for the code-agent distribution.
//!
//! Provides a Claude-Code-style terminal interface: scrollable conversation pane,
//! bordered tool-call boxes, inline permission prompts, status bar, and a
//! multiline input area at the bottom.

use crossterm::{
    ExecutableCommand,
    event::{
        DisableMouseCapture, EnableMouseCapture, KeyCode, KeyEvent, KeyModifiers, MouseEvent,
        MouseEventKind,
    },
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
// Theme
// ---------------------------------------------------------------------------

/// Color theme for the TUI. Centralizes all styling so it can be swapped.
#[derive(Clone)]
#[allow(dead_code)]
pub struct Theme {
    pub user_input: Color,
    pub assistant_text: Color,
    pub tool_border: Color,
    pub tool_running: Color,
    pub tool_success: Color,
    pub tool_failed: Color,
    pub tool_result: Color,
    pub permission: Color,
    pub info: Color,
    pub error: Color,
    pub status_bar_bg: Color,
    pub status_bar_fg: Color,
    pub input_prompt: Color,
    pub timestamp: Color,
    pub code_block: Color,
}

impl Theme {
    /// Default dark terminal theme.
    pub fn dark() -> Self {
        Self {
            user_input: Color::Cyan,
            assistant_text: Color::Reset,
            tool_border: Color::DarkGray,
            tool_running: Color::Yellow,
            tool_success: Color::Green,
            tool_failed: Color::Red,
            tool_result: Color::DarkGray,
            permission: Color::Yellow,
            info: Color::DarkGray,
            error: Color::Red,
            status_bar_bg: Color::DarkGray,
            status_bar_fg: Color::White,
            input_prompt: Color::Cyan,
            timestamp: Color::DarkGray,
            code_block: Color::Green,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

/// Format a SystemTime as HH:MM for display.
fn format_time(t: std::time::SystemTime) -> String {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Local time approximation: just use UTC offset from environment
    // (proper timezone handling would need chrono, not worth the dep)
    let hours = (secs / 3600) % 24;
    let minutes = (secs / 60) % 60;
    format!("{hours:02}:{minutes:02}")
}

/// A single entry in the conversation log.
#[derive(Clone)]
pub enum ConversationEntry {
    /// User message.
    UserInput(String, std::time::SystemTime),
    /// Model text output.
    AssistantText(String, std::time::SystemTime),
    /// A tool call (may still be running).
    ToolCall {
        tool_name: String,
        input_summary: String,
        status: ToolCallStatus,
        /// Tool result (populated after execution).
        result_summary: Option<String>,
        /// Whether to show compact (single-line) or expanded (box) view.
        collapsed: bool,
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

#[derive(Clone)]
#[allow(dead_code)]
pub enum ToolCallStatus {
    Running(std::time::Instant),
    Success,
    Failed(String),
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

/// Holds the entire TUI application state.
pub struct App {
    /// Color theme.
    pub theme: Theme,
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
    /// Viewport height of the conversation pane (computed each frame).
    viewport_height: u16,
    /// Whether the UI needs redrawing.
    pub dirty: bool,

    // --- Status bar fields ---
    pub model_name: String,
    pub total_input_tokens: usize,
    pub total_output_tokens: usize,
    pub turn_count: usize,
    /// Context tokens used (from SessionStatus).
    pub context_tokens: usize,
    /// Context window size.
    pub context_window: usize,

    /// Whether a turn is currently running.
    pub turn_active: bool,
    /// Spinner frame counter.
    pub spinner_tick: usize,

    /// Whether we are waiting for a permission response (blocks input).
    pub awaiting_permission: bool,
    /// The request_id of the pending permission (if any).
    pub pending_permission_request_id: Option<u64>,

    // --- Input history ---
    /// Past submitted inputs (oldest first).
    history: Vec<String>,
    /// Current position in history (None = editing new input, Some(i) = browsing).
    history_index: Option<usize>,
    /// Stash of the in-progress input when browsing history.
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
            history: Vec::new(),
            history_index: None,
            history_stash: String::new(),
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
        if !text.is_empty() {
            self.history.push(text.clone());
        }
        self.input.clear();
        self.cursor = 0;
        self.history_index = None;
        self.history_stash.clear();
        text
    }

    // --- History navigation ---

    /// Navigate to the previous (older) history entry.
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        match self.history_index {
            None => {
                // Entering history mode — stash current input
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
            _ => {} // Already at oldest entry
        }
        self.cursor = self.input.len();
    }

    /// Navigate to the next (newer) history entry, or back to the stashed input.
    pub fn history_next(&mut self) {
        match self.history_index {
            Some(idx) if idx + 1 < self.history.len() => {
                let idx = idx + 1;
                self.history_index = Some(idx);
                self.input = self.history[idx].clone();
            }
            Some(_) => {
                // Past the newest entry — restore stash
                self.history_index = None;
                self.input = self.history_stash.clone();
                self.history_stash.clear();
            }
            None => {} // Not in history mode
        }
        self.cursor = self.input.len();
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

    // Input area height: 1 border + number of lines in input (min 1, max 8)
    let input_line_count = app.input.lines().count().clamp(1, 8) as u16;
    let input_height = input_line_count + 1; // +1 for top border

    // Layout: [status_bar | conversation | input_area]
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),            // status bar
            Constraint::Min(5),               // conversation
            Constraint::Length(input_height), // input area (dynamic)
        ])
        .split(size);

    draw_status_bar(frame, app, chunks[0]);
    draw_conversation(frame, app, chunks[1]);
    draw_input(frame, app, chunks[2]);
}

fn format_tokens(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let left = " agent-kernel v0.1.0".to_string();
    let ctx = format!(
        "ctx: {}/{}",
        format_tokens(app.context_tokens),
        format_tokens(app.context_window)
    );
    let right = format!(
        "{} | {} | tokens: {}in/{}out | turns: {} ",
        app.model_name,
        ctx,
        format_tokens(app.total_input_tokens),
        format_tokens(app.total_output_tokens),
        app.turn_count,
    );

    let padding = area.width as usize
        - left.len().min(area.width as usize)
        - right.len().min(area.width as usize);
    let bar_text = format!("{left}{:width$}{right}", "", width = padding.max(1));

    let bar = Paragraph::new(Line::from(bar_text)).style(
        Style::default()
            .bg(app.theme.status_bar_bg)
            .fg(app.theme.status_bar_fg),
    );
    frame.render_widget(bar, area);
}

fn draw_conversation(frame: &mut Frame, app: &mut App, area: Rect) {
    let inner_width = area.width.saturating_sub(2) as usize; // border padding

    let mut lines: Vec<Line<'_>> = Vec::new();

    for entry in &app.entries {
        match entry {
            ConversationEntry::UserInput(text, time) => {
                let ts = format_time(*time);
                lines.push(Line::from(""));
                for (i, l) in text.lines().enumerate() {
                    let prefix = if i == 0 {
                        format!("{ts} > ")
                    } else {
                        " ".repeat(ts.len() + 3)
                    };
                    lines.push(Line::from(vec![
                        Span::styled(
                            prefix,
                            Style::default()
                                .fg(app.theme.user_input)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            l.to_string(),
                            Style::default()
                                .fg(app.theme.user_input)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                }
            }

            ConversationEntry::AssistantText(text, time) => {
                let ts = format_time(*time);
                lines.push(Line::from(""));
                let rendered = markdown_to_lines(text);
                for (i, line) in rendered.into_iter().enumerate() {
                    if i == 0 {
                        // Prepend timestamp to first line
                        let mut spans = vec![Span::styled(
                            format!("{ts} "),
                            Style::default().fg(app.theme.timestamp),
                        )];
                        spans.extend(line.spans);
                        lines.push(Line::from(spans));
                    } else {
                        lines.push(line);
                    }
                }
            }

            ConversationEntry::ToolCall {
                tool_name,
                input_summary,
                status,
                result_summary,
                collapsed,
            } => {
                lines.push(Line::from(""));

                // Compact single-line view for collapsed tool calls
                if *collapsed {
                    let (indicator, style) = match status {
                        ToolCallStatus::Success => {
                            ("\u{2713}", Style::default().fg(app.theme.tool_success))
                        }
                        ToolCallStatus::Failed(_) => {
                            ("\u{2717}", Style::default().fg(app.theme.tool_failed))
                        }
                        _ => ("", Style::default()),
                    };
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {indicator} "), style),
                        Span::styled(
                            tool_name.clone(),
                            Style::default()
                                .fg(app.theme.tool_border)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!(" {input_summary}"),
                            Style::default().fg(app.theme.info),
                        ),
                    ]));
                    continue;
                }
                let (indicator, style) = match status {
                    ToolCallStatus::Running(start) => {
                        let ch = SPINNER[app.spinner_tick % SPINNER.len()];
                        let elapsed = start.elapsed().as_secs_f32();
                        let time_str = if elapsed >= 1.0 {
                            format!(" {ch} {elapsed:.1}s")
                        } else {
                            format!(" {ch}")
                        };
                        (time_str, Style::default().fg(app.theme.tool_running))
                    }
                    ToolCallStatus::Success => (
                        " \u{2713}".to_string(),
                        Style::default().fg(app.theme.tool_success),
                    ),
                    ToolCallStatus::Failed(msg) => (
                        format!(" \u{2717} {msg}"),
                        Style::default().fg(app.theme.tool_failed),
                    ),
                };

                // Top border
                let title = format!("\u{250c}\u{2500} {tool_name} ");
                let remaining = inner_width.saturating_sub(title.len());
                let top = format!("{title}{}\u{2510}", "\u{2500}".repeat(remaining));
                lines.push(Line::from(Span::styled(
                    top,
                    Style::default().fg(Color::DarkGray),
                )));

                // Input line
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

                // Result lines (inside the box, truncated)
                if let Some(result) = result_summary {
                    let max_result_lines = 10;
                    let result_color = match tool_name.as_str() {
                        "file_read" | "grep" => app.theme.code_block,
                        "shell" => Color::White,
                        _ => app.theme.tool_result,
                    };
                    // Show error results in red
                    let result_color =
                        if result.starts_with("[error]") || result.starts_with("[exit") {
                            app.theme.error
                        } else {
                            result_color
                        };
                    let result_lines: Vec<&str> = result.lines().collect();
                    let truncated = result_lines.len() > max_result_lines;
                    for line in result_lines.iter().take(max_result_lines) {
                        let content = if line.len() > inner_width.saturating_sub(4) {
                            format!("{}...", &line[..inner_width.saturating_sub(7).max(4)])
                        } else {
                            line.to_string()
                        };
                        lines.push(Line::from(vec![
                            Span::styled(
                                "\u{2502} ".to_string(),
                                Style::default().fg(app.theme.tool_border),
                            ),
                            Span::styled(content, Style::default().fg(result_color)),
                        ]));
                    }
                    if truncated {
                        lines.push(Line::from(vec![
                            Span::styled(
                                "\u{2502} ".to_string(),
                                Style::default().fg(app.theme.tool_border),
                            ),
                            Span::styled(
                                format!(
                                    "... ({} more lines)",
                                    result_lines.len() - max_result_lines
                                ),
                                Style::default().fg(app.theme.info),
                            ),
                        ]));
                    }
                }

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

    // Compute visual line count accounting for word wrap.
    // Use unicode display width to match ratatui's wrapping behavior.
    use unicode_width::UnicodeWidthStr;
    let viewport_width = area.width.max(1) as usize;
    let total_visual_lines: u16 = lines
        .iter()
        .map(|line| {
            let line_width: usize = line
                .spans
                .iter()
                .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                .sum();
            if line_width == 0 {
                1
            } else {
                line_width.div_ceil(viewport_width) as u16
            }
        })
        .sum();
    app.rendered_lines = total_visual_lines;

    let viewport_height = area.height;
    app.viewport_height = viewport_height;
    if app.follow {
        app.scroll = total_visual_lines.saturating_sub(viewport_height);
    }

    let conversation = Paragraph::new(lines)
        .block(Block::default().borders(Borders::NONE))
        .wrap(Wrap { trim: false })
        .scroll((app.scroll, 0));

    frame.render_widget(conversation, area);
}

const SPINNER_PROMPTS: &[&str] = &[
    " ⠋ ", " ⠙ ", " ⠹ ", " ⠸ ", " ⠼ ", " ⠴ ", " ⠦ ", " ⠧ ", " ⠇ ", " ⠏ ",
];

fn draw_input(frame: &mut Frame, app: &App, area: Rect) {
    let prompt = if app.awaiting_permission {
        " [y/n] "
    } else if app.turn_active {
        SPINNER_PROMPTS[app.spinner_tick % SPINNER_PROMPTS.len()]
    } else {
        " > "
    };

    let prompt_style = Style::default()
        .fg(app.theme.input_prompt)
        .add_modifier(Modifier::BOLD);

    // Build lines: first line gets the prompt, subsequent lines get indent
    let input_lines: Vec<&str> = if app.input.is_empty() {
        vec![""]
    } else {
        app.input.split('\n').collect()
    };

    let mut text_lines: Vec<Line<'_>> = Vec::new();
    for (i, line) in input_lines.iter().enumerate() {
        if i == 0 {
            text_lines.push(Line::from(vec![
                Span::styled(prompt.to_string(), prompt_style),
                Span::raw(line.to_string()),
            ]));
        } else {
            // Indent continuation lines to align with first line content
            let indent = " ".repeat(prompt.len());
            text_lines.push(Line::from(vec![
                Span::styled(indent, Style::default()),
                Span::raw(line.to_string()),
            ]));
        }
    }

    let input_paragraph = Paragraph::new(text_lines).block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::DarkGray)),
    );

    frame.render_widget(input_paragraph, area);

    // Position cursor: find which line and column the cursor is on
    let (cursor_row, cursor_col) = cursor_position_in_input(&app.input, app.cursor);
    let cursor_x = area.x + prompt.len() as u16 + cursor_col as u16;
    let cursor_y = area.y + 1 + cursor_row as u16; // +1 for top border
    frame.set_cursor_position((cursor_x, cursor_y));
}

/// Compute (row, col) of the byte cursor within a multiline input string.
fn cursor_position_in_input(input: &str, cursor: usize) -> (usize, usize) {
    let before_cursor = &input[..cursor.min(input.len())];
    let row = before_cursor.matches('\n').count();
    let col = match before_cursor.rfind('\n') {
        Some(pos) => cursor - pos - 1,
        None => cursor,
    };
    (row, col)
}

// ---------------------------------------------------------------------------
// Terminal setup / teardown
// ---------------------------------------------------------------------------

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
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
            app.insert_char('\n');
            InputAction::None
        }
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
        KeyCode::Up if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.scroll_up(3);
            InputAction::None
        }
        KeyCode::Down if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let vh = app.viewport_height;
            app.scroll_down(3, vh);
            InputAction::None
        }
        KeyCode::Up => {
            app.history_prev();
            InputAction::None
        }
        KeyCode::Down => {
            app.history_next();
            InputAction::None
        }
        KeyCode::PageUp => {
            let vh = app.viewport_height;
            app.scroll_up(vh);
            InputAction::None
        }
        KeyCode::PageDown => {
            let vh = app.viewport_height;
            app.scroll_down(vh, vh);
            InputAction::None
        }
        _ => InputAction::None,
    }
}

/// Process a mouse event, mutate App state, and return any action.
pub fn handle_mouse(app: &mut App, mouse: MouseEvent) -> InputAction {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            app.scroll_up(3);
            InputAction::None
        }
        MouseEventKind::ScrollDown => {
            let vh = app.viewport_height;
            app.scroll_down(3, vh);
            InputAction::None
        }
        _ => InputAction::None,
    }
}

// ---------------------------------------------------------------------------
// Markdown → ratatui Lines
// ---------------------------------------------------------------------------

/// Convert a markdown string into styled ratatui Lines.
/// Supports: headers, bold, italic, inline code, fenced code blocks, lists.
fn markdown_to_lines(md: &str) -> Vec<Line<'static>> {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

    let parser = Parser::new_ext(md, Options::all());
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();

    // Style stack for nested formatting (bold inside italic, etc.)
    let mut bold = false;
    let mut italic = false;
    let mut in_code_block = false;
    let mut code_block_buf = String::new();
    let mut heading_level: Option<u8> = None;
    let mut list_depth: usize = 0;

    let flush_line = |spans: &mut Vec<Span<'static>>, lines: &mut Vec<Line<'static>>| {
        if !spans.is_empty() {
            lines.push(Line::from(std::mem::take(spans)));
        }
    };

    for event in parser {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                flush_line(&mut current_spans, &mut lines);
                heading_level = Some(level as u8);
            }
            Event::End(TagEnd::Heading(_)) => {
                flush_line(&mut current_spans, &mut lines);
                heading_level = None;
            }

            Event::Start(Tag::Emphasis) => italic = true,
            Event::End(TagEnd::Emphasis) => italic = false,

            Event::Start(Tag::Strong) => bold = true,
            Event::End(TagEnd::Strong) => bold = false,

            Event::Start(Tag::CodeBlock(_)) => {
                flush_line(&mut current_spans, &mut lines);
                in_code_block = true;
                code_block_buf.clear();
            }
            Event::End(TagEnd::CodeBlock) => {
                // Render code block with background styling
                for code_line in code_block_buf.lines() {
                    lines.push(Line::from(Span::styled(
                        format!("  {code_line}"),
                        Style::default().fg(Color::Green),
                    )));
                }
                in_code_block = false;
                code_block_buf.clear();
                lines.push(Line::from(""));
            }

            Event::Start(Tag::List(_)) => list_depth += 1,
            Event::End(TagEnd::List(_)) => {
                list_depth = list_depth.saturating_sub(1);
                if list_depth == 0 {
                    lines.push(Line::from(""));
                }
            }

            Event::Start(Tag::Item) => {
                flush_line(&mut current_spans, &mut lines);
                let indent = "  ".repeat(list_depth);
                current_spans.push(Span::styled(
                    format!("{indent}• "),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            Event::End(TagEnd::Item) => {
                flush_line(&mut current_spans, &mut lines);
            }

            Event::Start(Tag::Paragraph) => {
                flush_line(&mut current_spans, &mut lines);
            }
            Event::End(TagEnd::Paragraph) => {
                flush_line(&mut current_spans, &mut lines);
                lines.push(Line::from(""));
            }

            Event::Code(text) => {
                current_spans.push(Span::styled(
                    format!("`{text}`"),
                    Style::default().fg(Color::Green),
                ));
            }

            Event::Text(text) => {
                if in_code_block {
                    code_block_buf.push_str(&text);
                } else {
                    let style = if let Some(level) = heading_level {
                        match level {
                            1 => Style::default()
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                            2 => Style::default()
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD),
                            _ => Style::default()
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        }
                    } else if bold && italic {
                        Style::default().add_modifier(Modifier::BOLD | Modifier::ITALIC)
                    } else if bold {
                        Style::default().add_modifier(Modifier::BOLD)
                    } else if italic {
                        Style::default().add_modifier(Modifier::ITALIC)
                    } else {
                        Style::default()
                    };

                    // Split on newlines to handle multi-line text events
                    let text_str = text.to_string();
                    let mut line_iter = text_str.split('\n');
                    if let Some(first) = line_iter.next()
                        && !first.is_empty()
                    {
                        current_spans.push(Span::styled(first.to_string(), style));
                    }
                    for subsequent in line_iter {
                        flush_line(&mut current_spans, &mut lines);
                        if !subsequent.is_empty() {
                            current_spans.push(Span::styled(subsequent.to_string(), style));
                        }
                    }
                }
            }

            Event::SoftBreak => {
                current_spans.push(Span::raw(" "));
            }
            Event::HardBreak => {
                flush_line(&mut current_spans, &mut lines);
            }

            Event::Rule => {
                flush_line(&mut current_spans, &mut lines);
                lines.push(Line::from(Span::styled(
                    "─".repeat(40),
                    Style::default().fg(Color::DarkGray),
                )));
            }

            _ => {}
        }
    }

    // Flush any remaining spans
    flush_line(&mut current_spans, &mut lines);

    // Collapse consecutive blank lines into single blank lines
    let is_blank =
        |l: &Line<'_>| l.spans.is_empty() || l.spans.iter().all(|s| s.content.is_empty());
    let mut deduped = Vec::with_capacity(lines.len());
    let mut prev_blank = false;
    for line in lines {
        let blank = is_blank(&line);
        if blank && prev_blank {
            continue;
        }
        prev_blank = blank;
        deduped.push(line);
    }
    let mut lines = deduped;

    // Strip leading and trailing empty lines — the caller handles entry spacing
    while lines.first().is_some_and(&is_blank) {
        lines.remove(0);
    }
    while lines.last().is_some_and(&is_blank) {
        lines.pop();
    }

    lines
}
