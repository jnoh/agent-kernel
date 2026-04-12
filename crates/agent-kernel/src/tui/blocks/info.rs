use ratatui::prelude::*;

pub fn render_info(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!("  {text}"),
        Style::default().fg(Color::DarkGray),
    ))
}

pub fn render_error(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!("  [error] {text}"),
        Style::default().fg(Color::Red),
    ))
}
