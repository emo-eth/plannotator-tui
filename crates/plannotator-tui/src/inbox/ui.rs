#![allow(clippy::all, clippy::pedantic)]

//! Ratatui UI drawing for Plannotator Inbox.

use plannotator_tui_schema::inbox::{InboxSectionId, QuestionKind};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};

use super::app::{ActivePane, InboxApp};

pub(crate) fn draw(frame: &mut Frame, app: &InboxApp) {
    let area = frame.area();

    // Vertical split: Header (1), Main (Min 5), Bottom (1 or 5)
    let bottom_height = if app.reply_mode { 5 } else { 1 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(5), Constraint::Length(bottom_height)])
        .split(area);

    draw_header(frame, chunks[0], app);
    draw_main(frame, chunks[1], app);
    draw_bottom(frame, chunks[2], app);

    if app.show_help {
        draw_help_overlay(frame, area);
    }
}

fn draw_header(frame: &mut Frame, area: Rect, app: &InboxApp) {
    let conn_dot = if app.sse_connected {
        Span::styled(" \u{25cf} Live ", Style::default().fg(Color::Green))
    } else {
        Span::styled(" \u{25cb} Reconnecting ", Style::default().fg(Color::Yellow))
    };

    let title =
        Span::styled(" Plannotator Inbox ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));

    let session = app.client.current_session();
    let session_short = if session.len() > 8 { &session[..8] } else { &session };
    let server_info = Span::styled(
        format!("{} ({})", app.client.base_url, session_short),
        Style::default().fg(Color::DarkGray),
    );

    let help_hint = Span::styled(" [?] Help  [q] Quit ", Style::default().fg(Color::Gray));

    let line = Line::from(vec![
        title,
        conn_dot,
        server_info,
        Span::raw(" ".repeat(
            area.width.saturating_sub(35 + app.client.base_url.len() as u16 + help_hint.width() as u16)
                as usize,
        )),
        help_hint,
    ]);

    frame.render_widget(Paragraph::new(line), area);
}

fn draw_main(frame: &mut Frame, area: Rect, app: &InboxApp) {
    let horizontal_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(area);

    draw_threads_pane(frame, horizontal_chunks[0], app);
    draw_detail_pane(frame, horizontal_chunks[1], app);
}

fn section_style(id: InboxSectionId) -> Style {
    match id {
        InboxSectionId::New => Style::default().fg(Color::LightBlue).add_modifier(Modifier::BOLD),
        InboxSectionId::Waiting => Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        InboxSectionId::Sent => Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        InboxSectionId::Holding => Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        InboxSectionId::Stopped => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        InboxSectionId::Quiet => Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
    }
}

fn draw_threads_pane(frame: &mut Frame, area: Rect, app: &InboxApp) {
    let is_focused = app.active_pane == ActivePane::Threads;
    let border_style =
        if is_focused { Style::default().fg(Color::Cyan) } else { Style::default().fg(Color::DarkGray) };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .title(Span::styled(" Threads ", Style::default().add_modifier(Modifier::BOLD)));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.flattened_threads.is_empty() {
        let empty_msg = Paragraph::new(Line::from(vec![Span::styled(
            " No threads in inbox",
            Style::default().fg(Color::DarkGray),
        )]));
        frame.render_widget(empty_msg, inner);
        return;
    }

    let mut lines: Vec<Line> = Vec::new();
    let mut current_section: Option<InboxSectionId> = None;

    for (idx, item) in app.flattened_threads.iter().enumerate() {
        // Section header
        if current_section != Some(item.section_id) {
            current_section = Some(item.section_id);
            if !lines.is_empty() {
                lines.push(Line::raw(""));
            }
            let sec_count = app.flattened_threads.iter().filter(|t| t.section_id == item.section_id).count();
            lines.push(Line::from(vec![
                Span::styled(" \u{25be} ", section_style(item.section_id)),
                Span::styled(
                    format!("{} ({sec_count})", item.section_label.to_uppercase()),
                    section_style(item.section_id),
                ),
            ]));
        }

        let is_selected = idx == app.selected_thread_idx;
        let prefix = if is_selected {
            Span::styled(" \u{25b6} ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
        } else {
            Span::raw("   ")
        };

        let project_tag =
            Span::styled(format!("[{}] ", item.summary.project.name), Style::default().fg(Color::Magenta));
        let is_unread = item.summary.unread;
        let subject_style = if is_selected {
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
        } else if is_unread {
            Style::default().fg(Color::White)
        } else {
            Style::default().fg(Color::Gray)
        };

        let unread_dot = if is_unread {
            Span::styled("\u{25cf} ", Style::default().fg(Color::LightCyan))
        } else {
            Span::raw("  ")
        };

        let subj_text = item.summary.subject.as_deref().unwrap_or("Untitled");
        let subject = Span::styled(subj_text, subject_style);

        lines.push(Line::from(vec![prefix, unread_dot, project_tag, subject]));

        // Subline: Author · decisions/questions status
        let q_info = if item.summary.questions.open > 0 {
            format!("\u{23f3} {} open", item.summary.questions.open)
        } else {
            "\u{2713} answered".to_owned()
        };
        let subline = Line::from(vec![
            Span::raw("      "),
            Span::styled(
                format!("{} \u{b7} {q_info}", item.summary.last_author),
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        lines.push(subline);
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

fn draw_detail_pane(frame: &mut Frame, area: Rect, app: &InboxApp) {
    let is_focused = app.active_pane == ActivePane::Detail;
    let border_style =
        if is_focused { Style::default().fg(Color::Cyan) } else { Style::default().fg(Color::DarkGray) };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .title(Span::styled(" Thread Detail ", Style::default().add_modifier(Modifier::BOLD)));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(detail) = &app.current_thread else {
        let msg = Paragraph::new(Line::from(vec![Span::styled(
            " Select a thread on the left to read.",
            Style::default().fg(Color::DarkGray),
        )]));
        frame.render_widget(msg, inner);
        return;
    };

    let mut content_lines: Vec<Line> = Vec::new();

    // Subject & Project header
    let subj_display = detail.thread.subject.as_deref().unwrap_or("Untitled");
    content_lines.push(Line::from(vec![
        Span::styled("Subject: ", Style::default().fg(Color::DarkGray)),
        Span::styled(subj_display, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
    ]));

    let status_str = if detail.thread.resolved_at.is_some() {
        Span::styled(" \u{2713} Resolved ", Style::default().bg(Color::DarkGray).fg(Color::White))
    } else {
        Span::styled(" Open ", Style::default().bg(Color::Blue).fg(Color::White))
    };

    content_lines.push(Line::from(vec![
        Span::styled("Project: ", Style::default().fg(Color::DarkGray)),
        Span::styled(&detail.thread.project.name, Style::default().fg(Color::Magenta)),
        Span::raw("   "),
        Span::styled("Status: ", Style::default().fg(Color::DarkGray)),
        status_str,
    ]));

    content_lines.push(Line::from(vec![Span::styled(
        "\u{2500}".repeat(inner.width.saturating_sub(2) as usize),
        Style::default().fg(Color::DarkGray),
    )]));
    content_lines.push(Line::raw(""));

    // Render messages
    for msg in &detail.thread.messages {
        let author_tag = Span::styled(
            format!(" {} ", msg.author.display_name()),
            Style::default().bg(Color::DarkGray).fg(Color::White).add_modifier(Modifier::BOLD),
        );
        let host_info = msg.author.host().map(|h| format!("({h}) ")).unwrap_or_default();
        let time_info =
            Span::styled(format!(" {host_info}{}", msg.created_at), Style::default().fg(Color::DarkGray));

        content_lines.push(Line::from(vec![
            Span::styled("\u{2500}\u{2500} ", Style::default().fg(Color::DarkGray)),
            author_tag,
            time_info,
            Span::styled(
                " \u{2500}".repeat(inner.width.saturating_sub(40) as usize),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        content_lines.push(Line::raw(""));

        // Render message body lines (stripping question block lines since they become interactive cards)
        let mut in_question_block = false;
        for line in msg.body.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with(":::question") {
                in_question_block = true;
                continue;
            }
            if in_question_block {
                if trimmed == ":::" {
                    in_question_block = false;
                }
                continue;
            }

            // Regular markdown body line
            if line.starts_with('#') {
                content_lines.push(Line::from(vec![Span::styled(
                    line,
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                )]));
            } else {
                content_lines.push(Line::from(vec![Span::styled(line, Style::default().fg(Color::White))]));
            }
        }
        content_lines.push(Line::raw(""));

        // Render question cards for this message
        for (q_idx, card) in app.question_cards.iter().enumerate() {
            if card.question.message_id != msg.id {
                continue;
            }

            let is_card_focused = is_focused && app.active_card_idx == q_idx;
            let card_border = if is_card_focused {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Blue)
            };

            content_lines.push(Line::from(vec![
                Span::styled("\u{250c}\u{2500} Question: ", card_border),
                Span::styled(
                    &card.question.prompt,
                    Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    " \u{2500}"
                        .repeat(inner.width.saturating_sub(card.question.prompt.len() as u16 + 20) as usize),
                    card_border,
                ),
                Span::styled("\u{2510}", card_border),
            ]));

            for (c_idx, choice) in card.question.choices.iter().enumerate() {
                let choice_focused = is_card_focused && card.focused_choice == c_idx;
                let is_checked = card.is_choice_selected(&choice.label);

                let marker = match card.question.kind {
                    QuestionKind::Single => {
                        if is_checked {
                            "(*)"
                        } else {
                            "( )"
                        }
                    }
                    QuestionKind::Multi => {
                        if is_checked {
                            "[x]"
                        } else {
                            "[ ]"
                        }
                    }
                    QuestionKind::Text => "[text]",
                };

                let choice_style = if choice_focused {
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                } else if is_checked {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::White)
                };

                let rec_badge = if choice.recommended {
                    Span::styled(" (Recommended)", Style::default().fg(Color::Cyan))
                } else {
                    Span::raw("")
                };

                let pointer = if choice_focused { "\u{25b6} " } else { "  " };

                content_lines.push(Line::from(vec![
                    Span::styled("\u{2502} ", card_border),
                    Span::styled(pointer, Style::default().fg(Color::Yellow)),
                    Span::styled(format!("{marker} "), choice_style),
                    Span::styled(&choice.label, choice_style),
                    rec_badge,
                ]));
            }

            // Other... option
            let other_idx = card.question.choices.len();
            let other_focused = is_card_focused && card.focused_choice == other_idx;
            let other_pointer = if other_focused { "\u{25b6} " } else { "  " };
            let other_val = card.other_text.text();
            let other_display = if other_val.is_empty() { "[ Type other... ]".to_owned() } else { other_val };

            content_lines.push(Line::from(vec![
                Span::styled("\u{2502} ", card_border),
                Span::styled(other_pointer, Style::default().fg(Color::Yellow)),
                Span::styled(
                    "Other: ",
                    if other_focused {
                        Style::default().fg(Color::Yellow)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    },
                ),
                Span::styled(
                    other_display,
                    if card.other_active {
                        Style::default().fg(Color::White).bg(Color::DarkGray)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    },
                ),
            ]));

            content_lines.push(Line::from(vec![
                Span::styled("\u{2514}", card_border),
                Span::styled("\u{2500}".repeat(inner.width.saturating_sub(4) as usize), card_border),
                Span::styled("\u{2518}", card_border),
            ]));
            content_lines.push(Line::raw(""));
        }

        // Render attachments if any
        if let Some(attachments) = &msg.attachments {
            for att in attachments {
                content_lines.push(Line::from(vec![
                    Span::styled("  \u{1f4ce} Attachment: ", Style::default().fg(Color::LightBlue)),
                    Span::styled(
                        &att.name,
                        Style::default().fg(Color::White).add_modifier(Modifier::UNDERLINED),
                    ),
                    Span::styled(format!(" ({}) ", att.kind), Style::default().fg(Color::DarkGray)),
                    Span::styled("[Press 'a' to annotate]", Style::default().fg(Color::Yellow)),
                ]));
            }
            content_lines.push(Line::raw(""));
        }
    }

    // Apply scroll offset
    let visible_lines =
        if app.scroll_offset < content_lines.len() { &content_lines[app.scroll_offset..] } else { &[][..] };

    let paragraph = Paragraph::new(visible_lines.to_vec()).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
}

fn draw_bottom(frame: &mut Frame, area: Rect, app: &InboxApp) {
    if app.reply_mode {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Yellow))
            .title(Span::styled(
                " Reply [Enter / Ctrl+Enter: Send \u{b7} Esc: Cancel] ",
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let input_text = app.reply_buffer.text();
        let display_text = if input_text.is_empty() {
            Span::styled("Type your reply here...", Style::default().fg(Color::DarkGray))
        } else {
            Span::styled(&input_text, Style::default().fg(Color::White))
        };
        let p = Paragraph::new(Line::from(vec![display_text]));
        frame.render_widget(p, inner);
    } else {
        let hints = Span::styled(
            " [j/k] Navigate  [Tab] Pane  [Space/Enter] Pick  [r] Reply  [x] Resolve  [?] Help ",
            Style::default().fg(Color::Gray),
        );

        let status_span = if let Some((msg, _)) = &app.status_message {
            Span::styled(format!(" {msg} "), Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
        } else {
            Span::raw("")
        };

        let line = Line::from(vec![hints, status_span]);
        frame.render_widget(Paragraph::new(line), area);
    }
}

fn draw_help_overlay(frame: &mut Frame, area: Rect) {
    let popup_width = 62.min(area.width.saturating_sub(4));
    let popup_height = 18.min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(popup_width)) / 2;
    let y = (area.height.saturating_sub(popup_height)) / 2;
    let popup_rect = Rect::new(x, y, popup_width, popup_height);

    frame.render_widget(Clear, popup_rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            " Plannotator Inbox Help ",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ))
        .title_alignment(Alignment::Center);

    let inner = block.inner(popup_rect);
    frame.render_widget(block, popup_rect);

    let help_items = vec![
        ("j / k, \u{2193} / \u{2191}", "Navigate threads (left) or choices (right)"),
        ("Tab / Shift-Tab", "Switch focus between threads and detail pane"),
        ("Space / Enter", "Select / toggle choice on active question card"),
        ("r", "Compose free-text reply to asking agent"),
        ("x", "Toggle resolve status on current thread"),
        ("a", "Open attachment in annotator for line comments"),
        ("?", "Toggle this help overlay"),
        ("q / Esc", "Close overlay / Quit Inbox TUI"),
    ];

    let mut lines = vec![Line::raw("")];
    for (key, desc) in help_items {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {key:<18}"),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ),
            Span::styled(desc, Style::default().fg(Color::White)),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![Span::styled(
        "  Press Esc or ? to return",
        Style::default().fg(Color::DarkGray),
    )]));

    let p = Paragraph::new(lines);
    frame.render_widget(p, inner);
}
