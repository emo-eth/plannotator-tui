use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use super::{Opened, Shapes, Tab};
use crate::theme::palette;

impl Shapes {
    pub(crate) fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let [header, pages, body, status, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(2),
            Constraint::Length(1),
        ])
        .areas(area);
        let title = match &self.opened {
            Some(Opened::Shape { loaded, .. }) => {
                format!("Shapes · {} · v{}", loaded.shape.title, loaded.version.n)
            }
            Some(Opened::System { drawing, .. }) => {
                format!("System · {} · {}", drawing.project.name, drawing.project.state)
            }
            None if self.tab == Tab::Shapes => format!(
                "Shapes · {} · {}",
                if self.filter_thread { "this thread" } else { "all threads" },
                if self.finished { "including finished" } else { "active" }
            ),
            None => "System · projects and drawings".to_owned(),
        };
        frame.render_widget(
            Paragraph::new(title).style(Style::new().fg(palette().accent).add_modifier(Modifier::BOLD)),
            header,
        );
        let tabs = self
            .page_names()
            .iter()
            .enumerate()
            .map(|(index, name)| {
                format!(
                    "{}{}:{}{}",
                    if index == self.page { "[" } else { "" },
                    index + 1,
                    name,
                    if index == self.page { "]" } else { "" }
                )
            })
            .collect::<Vec<_>>()
            .join("  ");
        frame.render_widget(
            Paragraph::new(if tabs.is_empty() {
                "Tab: Shapes / System · o: real graphical viewer".to_owned()
            } else {
                tabs
            }),
            pages,
        );
        let [list, detail] = if body.width >= 85 {
            Layout::horizontal([Constraint::Percentage(36), Constraint::Percentage(64)]).areas(body)
        } else {
            Layout::vertical([Constraint::Length(5.min(body.height / 3)), Constraint::Min(1)]).areas(body)
        };
        self.draw_list(frame, list);
        let block = Block::default().borders(Borders::ALL).title(if let Some((name, _)) = &self.file {
            format!(" File · {name} · Esc returns ")
        } else {
            " Text details · PgUp/PgDn scroll · o draws in browser ".to_owned()
        });
        let text = self.file.as_ref().map(|(_, content)| content.as_str())
            .or_else(|| self.rows.get(self.selected).map(|row| row.detail.as_str()))
            .unwrap_or("Nothing here. R refreshes. Tab switches Shapes/System.\nShapes can be disabled in Settings or absent in an older Inbox. f switches this-thread/all; A includes finished boards.\nGraphic diagrams are deliberately opened in the real upstream viewer, not simulated as a terminal canvas.");
        frame.render_widget(
            Paragraph::new(text).block(block).wrap(Wrap { trim: false }).scroll((self.detail_scroll, 0)),
            detail,
        );
        frame.render_widget(
            Paragraph::new(self.status.as_deref().unwrap_or(if self.worker.is_some() {
                "Feedback saved · live agent is answering; the pane remains usable"
            } else {
                "GET-only browsing · every write requires confirmation · no direct graphic editing"
            }))
            .wrap(Wrap { trim: false })
            .style(Style::new().fg(palette().comment)),
            status,
        );
        frame.render_widget(
            Paragraph::new(
                "j/k select · Enter open · o graphics · e edit request/setup · a ask · ? help · Esc back",
            ),
            footer,
        );
        if self.help {
            Self::draw_help(frame, body);
        }
        if self.form.is_some() {
            self.draw_form(frame, area);
        }
    }

    fn draw_list(&mut self, frame: &mut Frame, area: Rect) {
        let block = Block::default().borders(Borders::ALL).title(format!(" {} entries ", self.rows.len()));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let height = usize::from(inner.height);
        if self.selected < self.list_scroll {
            self.list_scroll = self.selected;
        } else if self.selected >= self.list_scroll.saturating_add(height) {
            self.list_scroll = self.selected.saturating_sub(height.saturating_sub(1));
        }
        for (index, row) in self.rows.iter().enumerate().skip(self.list_scroll).take(height) {
            let style = if index == self.selected {
                Style::new().fg(palette().accent).add_modifier(Modifier::BOLD | Modifier::REVERSED)
            } else {
                Style::default()
            };
            let line = format!("{}{}", if index == self.selected { "> " } else { "  " }, row.label);
            frame.render_widget(
                Paragraph::new(line).style(style),
                Rect { y: inner.y.saturating_add((index - self.list_scroll) as u16), height: 1, ..inner },
            );
        }
    }

    fn draw_form(&self, frame: &mut Frame, area: Rect) {
        let Some(form) = &self.form else {
            return;
        };
        let width = area.width.saturating_sub(4).min(100);
        let height = area.height.saturating_sub(4).min(24);
        let rect = Rect {
            x: area.x + area.width.saturating_sub(width) / 2,
            y: area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        };
        if width < 8 || height < 4 {
            return;
        }
        frame.render_widget(Clear, rect);
        let block = Block::default().borders(Borders::ALL).title(form.title.as_str());
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        let [hint, content] =
            Layout::vertical([Constraint::Length(if form.confirm { 3 } else { 2 }), Constraint::Min(1)])
                .areas(inner);
        let instructions = if form.confirm {
            if form.uncertain {
                "The earlier write may already be saved. y sends again (may duplicate a note); n/Esc edits.\nInspect Notes before retrying. This is a real server write."
            } else {
                "Send these exact changes to the Inbox? y confirms; n/Esc returns.\nA Shape change asks its drawing agent; it does not edit a canvas."
            }
        } else {
            "Enter previews/asks confirmation · Alt+Enter/Ctrl+J newline · Esc discards draft"
        };
        frame.render_widget(
            Paragraph::new(instructions).wrap(Wrap { trim: false }).style(Style::new().fg(palette().comment)),
            hint,
        );
        let (lines, cursor_row, cursor_col) = form.editor.wrapped(usize::from(content.width.max(1)));
        let height = usize::from(content.height);
        let scroll = cursor_row.saturating_sub(height.saturating_sub(1));
        let shown: Vec<Line<'_>> =
            lines.iter().skip(scroll).take(height).map(|line| Line::from(line.as_str())).collect();
        frame.render_widget(Paragraph::new(shown), content);
        if !form.confirm && height > 0 {
            frame.set_cursor_position((
                content.x.saturating_add(cursor_col as u16).min(content.right().saturating_sub(1)),
                content
                    .y
                    .saturating_add((cursor_row - scroll) as u16)
                    .min(content.bottom().saturating_sub(1)),
            ));
        }
    }

    fn draw_help(frame: &mut Frame, area: Rect) {
        frame.render_widget(Clear, area);
        frame.render_widget(Paragraph::new(
            "Shapes / System\n\nTab switches feature. j/k or arrows select. Home/End jump. PgUp/PgDn scroll text.\nEnter opens board/version/reference; sheet and PR entries open their genuine browser view.\n1–7 select detail pages; R refreshes; Esc returns one level; ? closes help.\n\nShapes: f this-thread/all; A active/all including finished. o graphical board.\ne/c writes a Change it request; a writes Ask. On Parts, the exact view/id/quote/version anchors it.\nv explicitly marks this version seen. x finishes/reopens a board. Every write asks y/n.\nFeedback is stored first. A live agent is contacted without blocking navigation.\nIf gone/unavailable, the durable note waits for `plannotator shape read ID --notes`.\nThere is no idempotent note route: after uncertain sends, inspect Notes before retrying.\n\nSystem: e edits repos/docs/agent/model as JSON and saves through the real setup API.\nSheets, parts, flows, revisions, source repos, wanted sheets and touching PRs are readable.\nSetup Save does NOT draw: the latest upstream first-draw launcher is not implemented.\n\nGraphic limitations (o opens the actual upstream UI): HTML/SVG canvas, pan/zoom,\nscene playback, simple/full diagram rendering, box capture and cropped image selection.\nNo terminal graphic editing or invented drawing API. Ask/Change it sends actual feedback.\nOld What's changed/What to know views stay hidden, matching latest Inbox.\n\nAny key returns."
        ).block(Block::default().borders(Borders::ALL).title(" Shapes / System keys and limits ")).wrap(Wrap { trim: false }), area);
    }
}
