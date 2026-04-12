use super::App;
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

pub fn format_tokens(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
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
