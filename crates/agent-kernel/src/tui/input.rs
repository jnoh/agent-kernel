use super::App;
use super::types::{InputAction, parse_slash_command};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

const SPINNER_PROMPTS: &[&str] = &[
    " ⠋ ", " ⠙ ", " ⠹ ", " ⠸ ", " ⠼ ", " ⠴ ", " ⠦ ", " ⠧ ", " ⠇ ", " ⠏ ",
];

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
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

    let (cursor_row, cursor_col) = cursor_position_in_input(&app.input, app.cursor);
    let cursor_x = area.x + prompt.len() as u16 + cursor_col as u16;
    let cursor_y = area.y + 1 + cursor_row as u16;
    frame.set_cursor_position((cursor_x, cursor_y));
}

fn cursor_position_in_input(input: &str, cursor: usize) -> (usize, usize) {
    let before_cursor = &input[..cursor.min(input.len())];
    let row = before_cursor.matches('\n').count();
    let col = match before_cursor.rfind('\n') {
        Some(pos) => cursor - pos - 1,
        None => cursor,
    };
    (row, col)
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> InputAction {
    if app.awaiting_permission {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                return InputAction::PermissionDecision(true);
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                return InputAction::PermissionDecision(false);
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                return InputAction::PermissionAlwaysAllow;
            }
            _ => return InputAction::None,
        }
    }

    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        if app.turn_active {
            return InputAction::Cancel;
        } else {
            return InputAction::Quit;
        }
    }

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
            if let Some(cmd) = parse_slash_command(&text) {
                return InputAction::SlashCommand(cmd);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_mode_y_and_n_unchanged() {
        let mut app = App::new();
        app.awaiting_permission = true;
        let action = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
        );
        assert!(matches!(action, InputAction::PermissionDecision(true)));
    }

    #[test]
    fn permission_mode_a_returns_always_allow() {
        let mut app = App::new();
        app.awaiting_permission = true;
        let action = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        assert!(matches!(action, InputAction::PermissionAlwaysAllow));
    }

    #[test]
    fn non_permission_mode_a_is_plain_input() {
        let mut app = App::new();
        let action = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        assert!(matches!(action, InputAction::None));
        assert_eq!(app.input, "a");
    }
}
