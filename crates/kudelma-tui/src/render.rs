use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Block as AppBlock, Overlay, ToolStatus};
use crate::theme::Theme;

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    let suggestion_height = app.suggestions.len().min(6) as u16;
    let input_lines = app.input.lines().count().max(1) as u16;
    let input_height = (input_lines + 2).clamp(3, 9);
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(suggestion_height),
        Constraint::Length(input_height),
        Constraint::Length(1),
    ])
    .split(area);

    draw_header(frame, app, chunks[0]);
    draw_transcript(frame, app, chunks[1]);
    if suggestion_height > 0 {
        draw_suggestions(frame, app, chunks[2]);
    }
    draw_input(frame, app, chunks[3]);
    draw_footer(frame, app, chunks[4]);

    if let Some(overlay) = &app.overlay {
        draw_overlay(frame, app, overlay, area);
    }
    if let Some(permission) = &app.permission {
        draw_permission(frame, app, permission, area);
    }
}

fn draw_header(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let mut spans = vec![
        Span::styled(
            "Kudelma",
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ", Style::default()),
        Span::styled(&app.model_spec, Style::default().fg(app.theme.assistant)),
        Span::styled("  ", Style::default()),
        Span::styled(
            app.mode_name.to_uppercase(),
            Style::default().fg(if app.mode_name == "plan" {
                app.theme.warning
            } else {
                app.theme.success
            }),
        ),
    ];
    if app.context_window > 0 {
        let percent = (app.context_tokens * 100 / u64::from(app.context_window)).min(100);
        spans.push(Span::styled(
            format!("  ctx {percent}%"),
            Style::default().fg(if percent > 85 {
                app.theme.warning
            } else {
                app.theme.dim
            }),
        ));
    }
    spans.push(Span::styled(
        format!("  ${:.4}", app.total_cost),
        Style::default().fg(app.theme.dim),
    ));
    if let Some(notice) = &app.notice {
        spans.push(Span::styled(
            format!("  · {}", clip(notice, 60)),
            Style::default().fg(app.theme.dim),
        ));
    }
    if app.busy {
        spans.push(Span::styled(
            format!(
                "  {} working",
                spinner[(app.spinner as usize / 2) % spinner.len()]
            ),
            Style::default().fg(app.theme.warning),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_transcript(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let width = area.width.saturating_sub(1).max(10) as usize;
    let raw = if app.blocks.is_empty() {
        welcome(app)
    } else {
        app.render_lines(width)
    };
    let wrapped = wrap_lines(raw, width);
    let total = wrapped.len();
    let height = area.height as usize;
    let offset = total.saturating_sub(height + app.scroll as usize);
    app.viewport = height as u16;
    let visible: Vec<Line<'static>> = wrapped.into_iter().skip(offset).take(height).collect();
    frame.render_widget(Paragraph::new(visible), area);
}

fn welcome(app: &App) -> Vec<Line<'static>> {
    vec![
        Line::default(),
        Line::from(Span::styled(
            "  Kudelma",
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "  A Rust coding agent for your terminal.",
            Style::default().fg(app.theme.dim),
        )),
        Line::default(),
        Line::from(Span::styled(
            "  Describe a task, or type /help for commands.",
            Style::default().fg(app.theme.assistant),
        )),
        Line::from(Span::styled(
            format!("  {}", app.cwd.display()),
            Style::default().fg(app.theme.dim),
        )),
    ]
}

fn draw_suggestions(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let lines: Vec<Line<'static>> = app
        .suggestions
        .iter()
        .enumerate()
        .take(6)
        .map(|(index, suggestion)| {
            let selected = index == app.suggestion_index;
            let style = if selected {
                Style::default()
                    .fg(app.theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.assistant)
            };
            Line::from(vec![
                Span::styled(if selected { "  › " } else { "    " }, style),
                Span::styled(suggestion.label.clone(), style),
                Span::styled(
                    if suggestion.detail.is_empty() {
                        String::new()
                    } else {
                        format!("  {}", suggestion.detail)
                    },
                    Style::default().fg(app.theme.dim),
                ),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_input(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(Span::styled(
            if app.busy { " working " } else { " message " },
            Style::default().fg(app.theme.dim),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let width = inner.width as usize;

    let mut display = String::new();
    let mut cursor_line = 0usize;
    let mut cursor_column = 0usize;
    let mut line_index = 0usize;
    let mut column = 0usize;
    for (index, ch) in app.input.char_indices() {
        if index == app.cursor {
            cursor_line = line_index;
            cursor_column = column;
        }
        if ch == '\n' {
            line_index += 1;
            column = 0;
            display.push('\n');
        } else {
            column += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
            display.push(ch);
        }
    }
    if app.cursor >= app.input.len() {
        cursor_line = line_index;
        cursor_column = column;
    }

    let all_lines: Vec<&str> = display.lines().collect();
    let capacity = inner.height.max(1) as usize;
    let start = cursor_line.saturating_sub(capacity.saturating_sub(1));
    let mut lines: Vec<Line<'static>> = Vec::new();
    if app.input.is_empty() {
        lines.push(Line::from(Span::styled(
            "Ask Kudelma…  (/help)",
            Style::default().fg(app.theme.dim),
        )));
    } else {
        for line in all_lines.iter().skip(start).take(capacity) {
            let clipped = clip_width(line, width);
            lines.push(Line::from(Span::styled(
                clipped,
                Style::default().fg(app.theme.assistant),
            )));
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);

    if app.overlay.is_none() && app.permission.is_none() {
        let row = inner.y + (cursor_line - start).min(capacity.saturating_sub(1)) as u16;
        let column = cursor_column.min(width.saturating_sub(1));
        frame.set_cursor_position(Position::new(inner.x + column as u16, row));
    }
}

fn draw_footer(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let hint = if app.busy {
        "Esc cancel · Ctrl-B background · Ctrl-X tasks · Ctrl-C quit"
    } else {
        "Enter send · Shift-Enter newline · @files · /commands · Ctrl-X tasks · Ctrl-C quit"
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {hint}"),
            Style::default().fg(app.theme.dim),
        )))
        .alignment(Alignment::Left),
        area,
    );
}

fn draw_overlay(frame: &mut Frame<'_>, app: &App, overlay: &Overlay, area: Rect) {
    let popup = centered(area, 76, 70);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.accent))
        .title(Span::styled(
            " picker ",
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let lines: Vec<Line<'static>> = match overlay {
        Overlay::Help => help_lines(app),
        Overlay::Sessions { items, selected } => items
            .iter()
            .enumerate()
            .map(|(index, meta)| {
                let selected = index == *selected;
                let style = if selected {
                    Style::default()
                        .fg(app.theme.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.theme.assistant)
                };
                Line::from(vec![
                    Span::styled(if selected { "› " } else { "  " }, style),
                    Span::styled(
                        format!("{} ", meta.id.chars().take(8).collect::<String>()),
                        Style::default().fg(app.theme.dim),
                    ),
                    Span::styled(clip(&meta.title, 44), style),
                    Span::styled(
                        format!(
                            "  {}  {}",
                            meta.model,
                            &meta.updated_at[..10.min(meta.updated_at.len())]
                        ),
                        Style::default().fg(app.theme.dim),
                    ),
                ])
            })
            .collect(),
        Overlay::Models { items, selected } => items
            .iter()
            .enumerate()
            .map(|(index, model)| {
                let selected = index == *selected;
                let style = if selected {
                    Style::default()
                        .fg(app.theme.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.theme.assistant)
                };
                Line::from(vec![
                    Span::styled(if selected { "› " } else { "  " }, style),
                    Span::styled(model.spec(), style),
                    Span::styled(
                        format!(
                            "  {:>7} ctx  ${:.2}/${:.2}",
                            model.context_window, model.input_cost, model.output_cost
                        ),
                        Style::default().fg(app.theme.dim),
                    ),
                ])
            })
            .collect(),
        Overlay::Tasks { selected } => {
            if app.tasks.is_empty() {
                vec![Line::from(Span::styled(
                    "No subagent tasks yet.",
                    Style::default().fg(app.theme.dim),
                ))]
            } else {
                app.tasks
                    .iter()
                    .enumerate()
                    .map(|(index, task)| {
                        let selected = index == *selected;
                        let color = match task.status {
                            kudelma_core::TaskStatus::Running => app.theme.warning,
                            kudelma_core::TaskStatus::Done => app.theme.success,
                            kudelma_core::TaskStatus::Failed => app.theme.error,
                        };
                        Line::from(vec![
                            Span::styled(
                                if selected { "› " } else { "  " },
                                Style::default().fg(app.theme.accent),
                            ),
                            Span::styled(
                                format!("[{}] ", task.status.as_str()),
                                Style::default().fg(color),
                            ),
                            Span::styled(
                                task.label.clone(),
                                Style::default().fg(if selected {
                                    app.theme.accent
                                } else {
                                    app.theme.assistant
                                }),
                            ),
                        ])
                    })
                    .collect()
            }
        }
        Overlay::Rewind { items, selected } => items
            .iter()
            .enumerate()
            .map(|(index, (_, text))| {
                let selected = index == *selected;
                let style = if selected {
                    Style::default()
                        .fg(app.theme.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.theme.assistant)
                };
                Line::from(vec![
                    Span::styled(if selected { "› " } else { "  " }, style),
                    Span::styled(text.clone(), style),
                ])
            })
            .collect(),
    };
    frame.render_widget(Paragraph::new(lines), inner);
}

fn help_lines(app: &App) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled(
            "Keys",
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("  Enter send · Shift/Alt-Enter newline · Tab complete"),
        Line::from("  Esc cancel a turn · twice on an empty prompt to rewind"),
        Line::from("  Ctrl-B move the running tool to the background"),
        Line::from("  Ctrl-X subagents/tasks · Ctrl-T thinking · Ctrl-O tool output"),
        Line::from("  PageUp/PageDown scroll · Ctrl-C quit"),
        Line::default(),
        Line::from(Span::styled(
            "Commands",
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        )),
    ];
    for (command, detail) in crate::app::COMMANDS {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {command:<18}"),
                Style::default().fg(app.theme.code),
            ),
            Span::styled(*detail, Style::default().fg(app.theme.dim)),
        ]));
    }
    lines
}

fn draw_permission(
    frame: &mut Frame<'_>,
    app: &App,
    ask: &kudelma_core::PermissionAsk,
    area: Rect,
) {
    let popup = centered(area, 78, 60);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.warning))
        .title(Span::styled(
            " permission required ",
            Style::default()
                .fg(app.theme.warning)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let mut lines = vec![Line::from(vec![
        Span::styled("Tool: ", Style::default().fg(app.theme.dim)),
        Span::styled(
            ask.tool.clone(),
            Style::default()
                .fg(app.theme.warning)
                .add_modifier(Modifier::BOLD),
        ),
    ])];
    if let Some(reason) = &ask.reason {
        lines.push(Line::from(Span::styled(
            reason.clone(),
            Style::default().fg(app.theme.dim),
        )));
    }
    lines.push(Line::default());
    for scope in ask.scopes.iter().take(6) {
        lines.push(Line::from(Span::styled(
            format!("  {}", clip(scope, 100)),
            Style::default().fg(app.theme.code),
        )));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "[y] allow once   [a] session   [p] project   [g] global   [n/Esc] deny",
        Style::default().fg(app.theme.accent),
    )));
    frame.render_widget(Paragraph::new(lines), inner);
}

fn centered(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(vertical[1])[1]
}

fn wrap_lines(lines: Vec<Line<'static>>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut output = Vec::new();
    for line in lines {
        if line.spans.is_empty() {
            output.push(Line::default());
            continue;
        }
        let mut current: Vec<Span<'static>> = Vec::new();
        let mut current_width = 0usize;
        for span in line.spans {
            let style = span.style;
            let text = span.content.to_string();
            let mut token = String::new();
            for ch in text.chars() {
                let char_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
                if ch == ' ' && current_width + char_width > width {
                    flush(&mut output, &mut current, &mut current_width);
                    continue;
                }
                if current_width + char_width > width && current_width > 0 {
                    flush(&mut output, &mut current, &mut current_width);
                    if ch == ' ' {
                        continue;
                    }
                }
                if current.is_empty() && ch == ' ' {
                    continue;
                }
                token.push(ch);
                current_width += char_width;
                if current_width >= width {
                    current.push(Span::styled(std::mem::take(&mut token), style));
                    flush(&mut output, &mut current, &mut current_width);
                }
            }
            if !token.is_empty() {
                current.push(Span::styled(token, style));
            }
        }
        flush(&mut output, &mut current, &mut current_width);
    }
    output
}

fn flush(
    output: &mut Vec<Line<'static>>,
    current: &mut Vec<Span<'static>>,
    current_width: &mut usize,
) {
    output.push(Line::from(std::mem::take(current)));
    *current_width = 0;
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

fn clip_width(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text.width() <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let char_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
        if used + char_width > width.saturating_sub(1) {
            break;
        }
        out.push(ch);
        used += char_width;
    }
    out.push('…');
    out
}

#[allow(dead_code)]
fn _block_type(_: &AppBlock) {}
#[allow(dead_code)]
fn _tool_status(_: ToolStatus) {}
#[allow(dead_code)]
fn _theme(_: &Theme) {}
