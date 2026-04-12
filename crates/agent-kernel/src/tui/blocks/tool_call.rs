use ratatui::prelude::*;

use crate::tui::theme::Theme;
use crate::tui::types::ToolCallStatus;

const SPINNER: &[char] = &[
    '\u{280b}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283c}', '\u{2834}', '\u{2826}', '\u{2827}',
    '\u{2807}', '\u{280f}',
];

pub struct RenderCtx<'a> {
    pub tool_name: &'a str,
    pub input_summary: &'a str,
    pub status: &'a ToolCallStatus,
    pub result_summary: &'a Option<String>,
    pub expanded: bool,
    pub spinner_tick: usize,
    pub theme: &'a Theme,
    pub inner_width: usize,
}

pub fn render(ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
    let tool_name = ctx.tool_name;
    let input_summary = ctx.input_summary;
    let status = ctx.status;
    let result_summary = ctx.result_summary;
    let expanded = ctx.expanded;
    let spinner_tick = ctx.spinner_tick;
    let theme = ctx.theme;
    let inner_width = ctx.inner_width;
    let mut lines = Vec::new();
    let is_running = matches!(status, ToolCallStatus::Running(_));
    let show_box = is_running || expanded;

    let (indicator, ind_style) = match status {
        ToolCallStatus::Running(start) => {
            let ch = SPINNER[spinner_tick % SPINNER.len()];
            let elapsed = start.elapsed().as_secs_f32();
            let s = if elapsed >= 1.0 {
                format!(" {ch} {elapsed:.1}s")
            } else {
                format!(" {ch}")
            };
            (s, Style::default().fg(theme.tool_running))
        }
        ToolCallStatus::Success(d) => {
            let ms = d.as_millis();
            let dur = if ms < 1000 {
                format!("{ms}ms")
            } else {
                format!("{:.1}s", d.as_secs_f32())
            };
            (dur, Style::default().fg(theme.tool_border))
        }
        ToolCallStatus::Failed(msg) => (msg.clone(), Style::default().fg(theme.tool_failed)),
    };

    let status_icon = match status {
        ToolCallStatus::Running(_) => "",
        ToolCallStatus::Success(_) => "\u{2713} ",
        ToolCallStatus::Failed(_) => "\u{2717} ",
    };
    let icon_style = match status {
        ToolCallStatus::Running(_) => Style::default(),
        ToolCallStatus::Success(_) => Style::default().fg(theme.tool_success),
        ToolCallStatus::Failed(_) => Style::default().fg(theme.tool_failed),
    };

    if !show_box {
        lines.push(Line::from(vec![
            Span::styled(format!("  {status_icon}"), icon_style),
            Span::styled(
                tool_name.to_string(),
                Style::default().fg(theme.tool_border),
            ),
            Span::styled(
                format!(" {input_summary}"),
                Style::default().fg(theme.tool_border),
            ),
            Span::styled(format!(" {indicator}"), ind_style),
        ]));
        return lines;
    }

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
        input_summary.to_string()
    };
    lines.push(Line::from(vec![
        Span::styled(
            "\u{2502} ".to_string(),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(summary),
        Span::styled(format!(" {status_icon}{indicator}"), ind_style),
    ]));

    // Result lines
    if let Some(result) = result_summary {
        let max_result_lines = 20;
        let result_color = match tool_name {
            "file_read" | "grep" => theme.code_block,
            "shell" => Color::White,
            _ => theme.tool_result,
        };
        let result_color = if result.starts_with("[error]") || result.starts_with("[exit") {
            theme.error
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
                    Style::default().fg(theme.tool_border),
                ),
                Span::styled(content, Style::default().fg(result_color)),
            ]));
        }
        if truncated {
            lines.push(Line::from(vec![
                Span::styled(
                    "\u{2502} ".to_string(),
                    Style::default().fg(theme.tool_border),
                ),
                Span::styled(
                    format!("... ({} more lines)", result_lines.len() - max_result_lines),
                    Style::default().fg(theme.info),
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

    lines
}
