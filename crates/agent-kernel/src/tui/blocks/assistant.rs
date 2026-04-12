use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::prelude::*;

pub fn render(text: &str) -> Vec<Line<'static>> {
    markdown_to_lines(text)
}

fn markdown_to_lines(md: &str) -> Vec<Line<'static>> {
    let parser = Parser::new_ext(md, Options::all());
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();

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
                for code_line in code_block_buf.lines() {
                    lines.push(Line::from(Span::styled(
                        format!("  {code_line}"),
                        Style::default().fg(Color::Green),
                    )));
                }
                in_code_block = false;
                code_block_buf.clear();
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
                    format!("{indent}\u{2022} "),
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
                    "\u{2500}".repeat(40),
                    Style::default().fg(Color::DarkGray),
                )));
            }

            _ => {}
        }
    }

    flush_line(&mut current_spans, &mut lines);

    // Collapse consecutive blank lines
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

    while lines.first().is_some_and(&is_blank) {
        lines.remove(0);
    }
    while lines.last().is_some_and(&is_blank) {
        lines.pop();
    }

    lines
}
