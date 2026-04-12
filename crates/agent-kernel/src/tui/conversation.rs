use super::App;
use super::blocks;
use super::types::ConversationEntry;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let inner_width = area.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line<'_>> = Vec::new();

    for (entry_idx, entry) in app.entries.iter().enumerate() {
        match entry {
            ConversationEntry::UserInput(text) => {
                if entry_idx > 0 {
                    lines.push(Line::from(""));
                }
                lines.extend(blocks::user::render(text, &app.theme));
            }

            ConversationEntry::AssistantText(text) => {
                lines.push(Line::from(""));
                lines.extend(blocks::assistant::render(text));
            }

            ConversationEntry::ToolCall {
                tool_name,
                input_summary,
                status,
                result_summary,
                expanded,
            } => {
                lines.extend(blocks::tool_call::render(&blocks::tool_call::RenderCtx {
                    tool_name,
                    input_summary,
                    status,
                    result_summary,
                    expanded: *expanded,
                    spinner_tick: app.spinner_tick,
                    theme: &app.theme,
                    inner_width,
                }));
            }

            ConversationEntry::PermissionPrompt {
                tool_name,
                capabilities,
                input_summary,
            } => {
                lines.push(Line::from(""));
                lines.extend(blocks::permission::render(
                    tool_name,
                    capabilities,
                    input_summary,
                    inner_width,
                ));
            }

            ConversationEntry::Info(text) => {
                lines.push(blocks::info::render_info(text));
            }

            ConversationEntry::Error(text) => {
                lines.push(blocks::info::render_error(text));
            }
        }
    }

    // Compute visual line count accounting for word wrap.
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
