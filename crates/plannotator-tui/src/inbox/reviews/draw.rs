use super::model::Diagram;
use super::{Location, Mode, RenderRow, ReviewPane, View, feedback};
use crate::{doc::Document, layout::DocLayout, theme::palette};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

impl ReviewPane {
    pub(crate) fn draw(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let bands =
            Layout::vertical([Constraint::Length(2), Constraint::Min(1), Constraint::Length(3)]).split(area);
        let [header, body, footer] = bands.as_ref() else { return };
        if self.width != usize::from(body.width) {
            self.rebuild(usize::from(body.width));
        }
        self.height = usize::from(body.height);
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        }
        if self.cursor >= self.scroll.saturating_add(self.height) {
            self.scroll = self.cursor.saturating_sub(self.height.saturating_sub(1));
        }
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(
                    format!(
                        " {}  ·  {}{}",
                        self.snapshot.guide.title,
                        if self.view == View::Walkthrough { "Walkthrough" } else { "All files" },
                        if self.read_only { " · archived/read-only" } else { "" }
                    ),
                    Style::default().fg(palette().accent).add_modifier(Modifier::BOLD),
                ),
                Line::from(format!(
                    " {} · {} files · {} comments · {}",
                    self.snapshot.review.git_ref,
                    self.files.len(),
                    self.records.len(),
                    if self.verdict { "Review verdict available" } else { "Feedback only" }
                )),
            ]),
            *header,
        );
        let selection = self.selection.map(|start| (start.min(self.cursor), start.max(self.cursor)));
        let lines: Vec<_> = self
            .rows
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(self.height)
            .map(|(at, row)| {
                let mut line = row.line.clone();
                if selection.is_some_and(|(start, end)| (start..=end).contains(&at)) {
                    line = line.style(palette().selection);
                } else if at == self.cursor {
                    line = line.style(Style::default().bg(palette().block_bg));
                }
                line
            })
            .collect();
        frame.render_widget(Paragraph::new(lines).scroll((0, self.horizontal)), *body);
        let help = if self.pending.is_some() {
            "s retry identical send · D forget unconfirmed send (may already be delivered)"
        } else {
            "Tab view · [] jump · / path:line · j/k move · v select · c/C line/global · S suggest · o side · E context"
        };
        frame.render_widget(Paragraph::new(vec![Line::from(help), Line::from("m comments · x/V reviewed/viewed · s send · p forge · a Ask · z/Z Show me lines/overview · O web · R refresh · Esc back"), Line::styled(self.status.as_str(), Style::default().fg(palette().comment))]), *footer);
        if !matches!(self.mode, Mode::Browse) {
            self.draw_modal(frame, *body);
        }
    }

    pub(super) fn rebuild(&mut self, width: usize) {
        self.rows.clear();
        self.width = width.max(1);
        if self.view == View::Walkthrough {
            self.markdown(&self.snapshot.guide.intent.clone(), None, None);
            if let Some(overview) = self.snapshot.guide.overview.clone() {
                self.markdown(&overview.text, None, None);
                if let Some(diagram) = overview.diagram {
                    self.diagram(&diagram, None);
                }
            }
            let chapters = self.snapshot.guide.sections.clone();
            let mut placed = std::collections::HashSet::new();
            for (index, chapter) in chapters.iter().enumerate() {
                let checked = self.message.guide_reviewed.get(index).copied().unwrap_or(false);
                let tick = if self.code_review && self.message.guide.review && !self.message.guide.walkthrough
                {
                    if checked { "[x] " } else { "[ ] " }
                } else {
                    ""
                };
                self.heading(format!("{tick}Chapter {}: {}", index + 1, chapter.title), Some(index), None);
                self.markdown(&chapter.overview, Some(index), None);
                if let Some(diagram) = &chapter.diagram {
                    self.diagram(diagram, Some(index));
                }
                for reference in &chapter.diffs {
                    if let Some(summary) = &reference.summary {
                        self.markdown(summary, Some(index), None);
                    }
                    if let Some(file) = self.files.iter().position(|file| file.path == reference.file) {
                        placed.insert(file);
                        self.file_rows(file, Some(index));
                        for note in &reference.notes {
                            self.markdown(
                                &format!(
                                    "**Author note — {}:{}–{} ({}):** {}",
                                    reference.file,
                                    note.line_start,
                                    note.line_end,
                                    note.side.as_deref().unwrap_or("new"),
                                    note.text
                                ),
                                Some(index),
                                Some(file),
                            );
                        }
                    }
                }
            }
            let rest: Vec<_> = (0..self.files.len()).filter(|index| !placed.contains(index)).collect();
            if !rest.is_empty() {
                self.heading("Everything else".into(), None, None);
            }
            for index in rest {
                self.file_rows(index, None);
            }
        } else {
            for index in 0..self.files.len() {
                self.file_rows(index, None);
            }
        }
        if self.rows.is_empty() {
            self.heading("This review has no changed files.".into(), None, None);
        }
    }

    fn heading(&mut self, text: String, chapter: Option<usize>, file: Option<usize>) {
        self.rows.push(RenderRow {
            line: Line::styled(text, Style::default().fg(palette().heading).add_modifier(Modifier::BOLD)),
            chapter,
            file,
            location: None,
        });
    }

    fn markdown(&mut self, text: &str, chapter: Option<usize>, file: Option<usize>) {
        let document = Document::parse(text.into());
        let layout = DocLayout::build(&document, self.width);
        for block in &layout.blocks {
            for row in &block.rows {
                self.rows.push(RenderRow { line: row.line.clone(), chapter, file, location: None });
            }
            self.rows.push(RenderRow { line: Line::default(), chapter, file, location: None });
        }
    }

    fn diagram(&mut self, diagram: &Diagram, chapter: Option<usize>) {
        self.heading(
            format!("Diagram ({}, source — graphical rendering needs the web Inbox)", diagram.kind),
            chapter,
            None,
        );
        for line in diagram.source.lines() {
            self.rows.push(RenderRow {
                line: Line::from(line.to_owned()),
                chapter,
                file: None,
                location: None,
            });
        }
    }

    fn file_rows(&mut self, index: usize, chapter: Option<usize>) {
        let Some(file) = self.files.get(index) else { return };
        let path = file.path.clone();
        let rows = file.rows.clone();
        let viewed = self.message.guide_viewed.contains(&path);
        let tick = if self.code_review && self.message.guide.review && !self.message.guide.walkthrough {
            if viewed { "[Viewed] " } else { "[ ] " }
        } else {
            ""
        };
        self.heading(format!("{tick}{path}"), chapter, Some(index));
        for row in rows {
            let location = (row.old.is_some() || row.new.is_some()).then(|| Location {
                file: index,
                old: row.old,
                new: row.new,
                hunk: row.hunk,
                text: row.text.clone(),
                outside: false,
            });
            let style = Style::default().fg(match row.kind {
                '+' => palette().looks_good,
                '-' => palette().delete,
                '@' => palette().accent,
                _ => palette().idle_fg,
            });
            let line = if location.is_some() {
                Line::from(vec![
                    Span::styled(
                        format!(
                            "{:>5} {:>5} {} ",
                            row.old.map_or(String::new(), |n| n.to_string()),
                            row.new.map_or(String::new(), |n| n.to_string()),
                            row.kind
                        ),
                        style,
                    ),
                    Span::styled(row.text, style),
                ])
            } else {
                Line::styled(row.text, style)
            };
            self.rows.push(RenderRow { line, chapter, file: Some(index), location });
        }
        if let Some((_, contents)) = self.expanded.as_ref().filter(|(file, _)| *file == index).cloned() {
            for (side, text) in [("old", contents.old_content), ("new", contents.new_content)] {
                let Some(text) = text else { continue };
                self.heading(format!("Whole file — {side} side (snapshot content)"), chapter, Some(index));
                for (number, text) in text.lines().enumerate() {
                    let number = number + 1;
                    let old = (side == "old").then_some(number);
                    let new = (side == "new").then_some(number);
                    self.rows.push(RenderRow {
                        line: Line::from(format!("{number:>5} {side}  {text}")),
                        chapter,
                        file: Some(index),
                        location: Some(Location {
                            file: index,
                            old,
                            new,
                            hunk: usize::MAX,
                            text: text.into(),
                            outside: true,
                        }),
                    });
                }
            }
        }
    }

    fn draw_modal(&self, frame: &mut Frame<'_>, area: Rect) {
        let area = Rect {
            x: area.x + area.width / 10,
            y: area.y + area.height / 10,
            width: area.width * 8 / 10,
            height: area.height * 8 / 10,
        };
        frame.render_widget(Clear, area);
        let (title, lines) = match &self.mode {
            Mode::Comments => {
                let mut lines = vec![Line::from(
                    "j/k select · Enter go to code · e edit · S edit suggestion · d remove · R reload · Esc back",
                )];
                for (index, record) in self.records.iter().enumerate() {
                    let a = &record.annotation;
                    let location = if feedback::field(a, "scope") == "general" {
                        "Whole review".into()
                    } else {
                        format!(
                            "{}:{} ({})",
                            feedback::field(a, "filePath"),
                            a.get("lineStart").unwrap_or(&serde_json::Value::Null),
                            feedback::field(a, "side")
                        )
                    };
                    let style =
                        if index == self.selected_comment { palette().selection } else { Style::default() };
                    lines.push(Line::styled(
                        format!(
                            "{} {location}: {}",
                            if index == self.selected_comment { ">" } else { " " },
                            feedback::field(a, "text")
                        ),
                        style,
                    ));
                    let suggested = feedback::field(a, "suggestedCode");
                    if !suggested.is_empty() {
                        lines.push(Line::styled(format!("  Suggestion: {suggested}"), style));
                    }
                }
                ("Comments", lines)
            }
            Mode::Send(kind) => {
                let mut lines = vec![Line::from(format!(
                    "{} comments saved · j/k choose · n note · Enter CONFIRMS SEND · Esc cancel",
                    self.records.len()
                ))];
                lines.extend(self.send_options().iter().map(|option| {
                    Line::styled(
                        format!("{} {}", if option == kind { ">" } else { " " }, option.label()),
                        if option == kind { palette().selection } else { Style::default() },
                    )
                }));
                lines.push(Line::from(format!("Note: {}", self.note)));
                ("Send review reply", lines)
            }
            Mode::Remove(_) => (
                "Remove comment",
                vec![Line::from("Enter removes this saved comment permanently. Esc cancels.")],
            ),
            _ => {
                let title = match &self.mode {
                    Mode::Comment(_) => "Comment",
                    Mode::Suggestion(_) => "Suggested replacement code",
                    Mode::Note(_) => "Review note",
                    Mode::Jump => "Jump to exact path[:line]",
                    _ => "Review",
                };
                let (lines, _, _) = self.compose.wrapped(usize::from(area.width.saturating_sub(2)).max(1));
                let mut lines: Vec<_> = lines.into_iter().map(Line::from).collect();
                lines.push(Line::styled(
                    "Enter save · Alt+Enter newline · Esc cancel",
                    Style::default().fg(palette().idle_fg),
                ));
                (title, lines)
            }
        };
        let scroll = if matches!(self.mode, Mode::Comments) {
            self.selected_comment.saturating_sub(usize::from(area.height.saturating_sub(4)))
        } else {
            0
        };
        frame.render_widget(
            Paragraph::new(lines)
                .block(Block::default().borders(Borders::ALL).title(title))
                .wrap(Wrap { trim: false })
                .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)),
            area,
        );
    }
}
