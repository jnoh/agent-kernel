use ratatui::prelude::*;

pub fn render(
    tool_name: &str,
    capabilities: &[String],
    input_summary: &str,
    inner_width: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

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
            input_summary.to_string()
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
            "[y/n/a]",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    lines
}
