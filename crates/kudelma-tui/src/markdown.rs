use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::Theme;

/// Lightweight Markdown -> styled lines. It intentionally covers the subset
/// coding agents actually emit; code fences keep their indentation.
pub fn render_markdown(text: &str, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut in_code = false;
    for raw in text.replace('\t', "    ").lines() {
        if raw.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            lines.push(Line::from(vec![
                Span::styled("  │ ", Style::default().fg(theme.dim)),
                Span::styled(raw.to_string(), Style::default().fg(theme.code)),
            ]));
            continue;
        }
        let trimmed = raw.trim_end();
        if trimmed.trim().is_empty() {
            lines.push(Line::default());
            continue;
        }
        if let Some(heading) = trimmed.strip_prefix("### ") {
            lines.push(Line::from(Span::styled(
                heading.to_string(),
                Style::default()
                    .fg(theme.heading)
                    .add_modifier(Modifier::BOLD),
            )));
            continue;
        }
        if let Some(heading) = trimmed.strip_prefix("## ") {
            lines.push(Line::from(Span::styled(
                heading.to_string(),
                Style::default()
                    .fg(theme.heading)
                    .add_modifier(Modifier::BOLD),
            )));
            continue;
        }
        if let Some(heading) = trimmed.strip_prefix("# ") {
            lines.push(Line::from(Span::styled(
                heading.to_string(),
                Style::default()
                    .fg(theme.heading)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            )));
            continue;
        }
        if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("+ "))
        {
            let mut spans = vec![Span::styled("  • ", Style::default().fg(theme.accent))];
            spans.extend(inline(item, theme, Style::default().fg(theme.assistant)));
            lines.push(Line::from(spans));
            continue;
        }
        if let Some(quote) = trimmed.strip_prefix("> ") {
            lines.push(Line::from(vec![
                Span::styled("  │ ", Style::default().fg(theme.dim)),
                Span::styled(
                    quote.to_string(),
                    Style::default()
                        .fg(theme.dim)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]));
            continue;
        }
        lines.push(Line::from(inline(
            trimmed,
            theme,
            Style::default().fg(theme.assistant),
        )));
    }
    lines
}

fn inline(text: &str, theme: &Theme, base: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut buffer = String::new();
    let mut cursor = 0;
    while cursor < text.len() {
        let rest = &text[cursor..];
        let Some(offset) = rest.find(['*', '`', '[']) else {
            buffer.push_str(rest);
            break;
        };
        let marker = cursor + offset;
        buffer.push_str(&text[cursor..marker]);
        let rest = &text[marker..];
        if let Some(after) = rest.strip_prefix("**") {
            if let Some(end) = after.find("**") {
                push_buffer(&mut spans, &mut buffer, base);
                spans.push(Span::styled(
                    after[..end].to_string(),
                    base.add_modifier(Modifier::BOLD),
                ));
                cursor = marker + 2 + end + 2;
                continue;
            }
        }
        if let Some(after) = rest.strip_prefix('`') {
            if let Some(end) = after.find('`') {
                push_buffer(&mut spans, &mut buffer, base);
                spans.push(Span::styled(after[..end].to_string(), base.fg(theme.code)));
                cursor = marker + 1 + end + 1;
                continue;
            }
        }
        if let Some(after) = rest.strip_prefix('[') {
            if let Some(close) = after.find("](") {
                if let Some(end) = after[close + 2..].find(')') {
                    push_buffer(&mut spans, &mut buffer, base);
                    spans.push(Span::styled(
                        after[..close].to_string(),
                        base.fg(theme.accent),
                    ));
                    cursor = marker + 2 + close + 2 + end + 1;
                    continue;
                }
            }
        }
        let ch = rest.chars().next().unwrap_or_default();
        buffer.push(ch);
        cursor = marker + ch.len_utf8();
    }
    push_buffer(&mut spans, &mut buffer, base);
    spans
}

fn push_buffer(spans: &mut Vec<Span<'static>>, buffer: &mut String, style: Style) {
    if !buffer.is_empty() {
        spans.push(Span::styled(std::mem::take(buffer), style));
    }
}
