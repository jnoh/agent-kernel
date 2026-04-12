use ratatui::prelude::*;

use crate::tui::theme::Theme;

pub fn render(text: &str, theme: &Theme) -> Vec<Line<'static>> {
    let user_style = Style::default()
        .fg(theme.user_input)
        .add_modifier(Modifier::BOLD);
    text.lines()
        .enumerate()
        .map(|(i, l)| {
            let prefix = if i == 0 { "> " } else { "  " };
            Line::from(vec![
                Span::styled(prefix.to_string(), user_style),
                Span::styled(l.to_string(), user_style),
            ])
        })
        .collect()
}
