#![allow(clippy::all, clippy::pedantic)]

//! Inbox application state machine and event handling.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::Instant;

use anyhow::Result;
use plannotator_tui_schema::inbox::{
    InboxListRow, InboxSectionId, InboxServerEvent, ListModel, PickItem, ReplyQuestionRef, ThreadModel,
};
use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;

use super::client::InboxClient;
use super::discovery::InboxConnectionInfo;
use super::layout::{
    DEFAULT_SPLIT_RATIO, adjust_split_ratio, compute_layout, drag_to_ratio, load_split_ratio,
    save_split_ratio,
};
use super::questions::{QuestionCard, QuestionCardAction};
use super::sse::spawn_sse_listener;
use crate::app::editing::TextBuffer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActivePane {
    Threads,
    Detail,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct InboxGeometry {
    pub(crate) area: Rect,
    pub(crate) threads_rect: Rect,
    pub(crate) detail_rect: Rect,
    pub(crate) divider_col: u16,
    pub(crate) thread_rows: Vec<(Rect, usize)>,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct FlattenedThread {
    pub(crate) section_id: InboxSectionId,
    pub(crate) section_label: String,
    pub(crate) is_first_in_section: bool,
    pub(crate) summary: InboxListRow,
}

pub(crate) struct InboxApp {
    pub(crate) client: InboxClient,
    pub(crate) sse_rx: Receiver<InboxServerEvent>,
    pub(crate) sse_stop: Arc<AtomicBool>,
    pub(crate) sse_connected: bool,

    pub(crate) list: ListModel,
    pub(crate) flattened_threads: Vec<FlattenedThread>,
    pub(crate) selected_thread_idx: usize,

    pub(crate) current_thread: Option<ThreadModel>,
    pub(crate) question_cards: Vec<QuestionCard>,
    pub(crate) active_card_idx: usize,

    pub(crate) active_pane: ActivePane,
    pub(crate) scroll_offset: usize,

    pub(crate) split_ratio: f32,
    pub(crate) dragging_divider: bool,
    pub(crate) geometry: InboxGeometry,
    pub(crate) last_area: Rect,

    pub(crate) reply_mode: bool,
    pub(crate) reply_buffer: TextBuffer,

    pub(crate) show_help: bool,
    pub(crate) status_message: Option<(String, Instant)>,
    pub(crate) quit: bool,
}

impl InboxApp {
    pub(crate) fn new(info: InboxConnectionInfo) -> Result<Self> {
        let client = InboxClient::new(info.clone());

        // Initial fetch of thread list
        let list = client.list_threads(None).unwrap_or_else(|_| ListModel {
            server_session: client.current_session(),
            ..ListModel::default()
        });

        let (sse_tx, sse_rx) = channel();
        let sse_stop = Arc::new(AtomicBool::new(false));
        spawn_sse_listener(client.base_url.clone(), list.cursor, sse_tx, Arc::clone(&sse_stop));

        let split_ratio = load_split_ratio().unwrap_or(DEFAULT_SPLIT_RATIO);

        let mut app = Self {
            client,
            sse_rx,
            sse_stop,
            sse_connected: true,

            list,
            flattened_threads: Vec::new(),
            selected_thread_idx: 0,

            current_thread: None,
            question_cards: Vec::new(),
            active_card_idx: 0,

            active_pane: ActivePane::Threads,
            scroll_offset: 0,

            split_ratio,
            dragging_divider: false,
            geometry: InboxGeometry::default(),
            last_area: Rect::default(),

            reply_mode: false,
            reply_buffer: TextBuffer::default(),

            show_help: false,
            status_message: None,
            quit: false,
        };

        app.rebuild_flattened_threads();
        if !app.flattened_threads.is_empty() {
            app.load_thread(0);
        }

        Ok(app)
    }

    pub(crate) fn rebuild_flattened_threads(&mut self) {
        self.flattened_threads.clear();
        for section in &self.list.sections {
            for (idx, thread) in section.threads.iter().enumerate() {
                self.flattened_threads.push(FlattenedThread {
                    section_id: section.id,
                    section_label: section.label.clone(),
                    is_first_in_section: idx == 0,
                    summary: thread.clone(),
                });
            }
        }
        if self.selected_thread_idx >= self.flattened_threads.len() {
            self.selected_thread_idx = self.flattened_threads.len().saturating_sub(1);
        }
    }

    pub(crate) fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = Some((msg.into(), Instant::now()));
    }

    pub(crate) fn load_thread(&mut self, idx: usize) {
        if let Some(target) = self.flattened_threads.get(idx) {
            let thread_id = target.summary.thread_id.clone();
            if let Ok(detail) = self.client.get_thread(&thread_id) {
                // Mark thread seen
                let _ = self.client.mark_seen(&thread_id);

                // Build question cards from all messages
                let mut cards = Vec::new();
                for msg in &detail.thread.messages {
                    if let Some(qs) = &msg.questions {
                        for q in qs {
                            cards.push(QuestionCard::new(q.clone()));
                        }
                    }
                }

                self.current_thread = Some(detail);
                self.question_cards = cards;
                self.active_card_idx = 0;
                self.scroll_offset = 0;
            }
        }
    }

    pub(crate) fn refresh_list_and_current(&mut self) {
        if let Ok(fresh_list) = self.client.list_threads(None) {
            self.list = fresh_list;
            self.rebuild_flattened_threads();
            self.load_thread(self.selected_thread_idx);
        }
    }

    /// Process any queued SSE events without blocking.
    pub(crate) fn pump_sse_events(&mut self) {
        while let Ok(event) = self.sse_rx.try_recv() {
            match event {
                InboxServerEvent::Hello(h) => {
                    self.client.update_session(h.server_session);
                    self.sse_connected = true;
                }
                InboxServerEvent::Record(r) => {
                    self.sse_connected = true;
                    if r.kind == "message" || r.kind == "thread" || r.kind == "project" {
                        self.refresh_list_and_current();
                    }
                }
                InboxServerEvent::Status(_) => {
                    self.sse_connected = true;
                }
                InboxServerEvent::Ping => {
                    self.sse_connected = true;
                }
            }
        }
    }

    /// Handle unified crossterm event.
    pub(crate) fn handle_event(&mut self, event: &Event) -> Result<()> {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                self.handle_key(*key)?;
            }
            Event::Mouse(mouse) => {
                self.handle_mouse(*mouse)?;
            }
            Event::Resize(cols, rows) => {
                self.handle_resize(*cols, *rows);
            }
            Event::Paste(text) => {
                if self.reply_mode {
                    for c in text.chars() {
                        self.reply_buffer.insert(c);
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn handle_resize(&mut self, cols: u16, rows: u16) {
        self.last_area = Rect::new(0, 0, cols, rows);
        let layout = compute_layout(self.last_area, self.split_ratio, self.reply_mode);
        self.geometry.area = self.last_area;
        self.geometry.threads_rect = layout.threads;
        self.geometry.detail_rect = layout.detail;
        self.geometry.divider_col = layout.divider_col;
    }

    pub(crate) fn select_next_thread(&mut self) {
        if !self.flattened_threads.is_empty() && self.selected_thread_idx + 1 < self.flattened_threads.len() {
            self.selected_thread_idx += 1;
            self.load_thread(self.selected_thread_idx);
        }
    }

    pub(crate) fn select_prev_thread(&mut self) {
        if self.selected_thread_idx > 0 {
            self.selected_thread_idx -= 1;
            self.load_thread(self.selected_thread_idx);
        }
    }

    pub(crate) fn adjust_split(&mut self, delta_cols: i16) {
        let width = if self.geometry.area.width > 0 {
            self.geometry.area.width
        } else if self.last_area.width > 0 {
            self.last_area.width
        } else {
            160
        };
        self.split_ratio = adjust_split_ratio(self.split_ratio, delta_cols, width);
        save_split_ratio(self.split_ratio);
        let layout = compute_layout(self.geometry.area, self.split_ratio, self.reply_mode);
        self.geometry.threads_rect = layout.threads;
        self.geometry.detail_rect = layout.detail;
        self.geometry.divider_col = layout.divider_col;
    }

    pub(crate) fn reset_split(&mut self) {
        self.split_ratio = DEFAULT_SPLIT_RATIO;
        save_split_ratio(self.split_ratio);
        let layout = compute_layout(self.geometry.area, self.split_ratio, self.reply_mode);
        self.geometry.threads_rect = layout.threads;
        self.geometry.detail_rect = layout.detail;
        self.geometry.divider_col = layout.divider_col;
    }

    /// Handle keyboard input.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        // Clear old status message after 4s
        if let Some((_, time)) = &self.status_message {
            if time.elapsed().as_secs() > 4 {
                self.status_message = None;
            }
        }

        // Help overlay intercept
        if self.show_help {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')) {
                self.show_help = false;
            }
            return Ok(());
        }

        // Reply compose mode
        if self.reply_mode {
            match key.code {
                KeyCode::Esc => {
                    self.reply_mode = false;
                    self.set_status("Reply canceled");
                }
                KeyCode::Enter => {
                    self.submit_reply()?;
                }
                _ => {
                    self.reply_buffer.handle_key(key);
                }
            }
            return Ok(());
        }

        // Global split resizing
        if (key.code == KeyCode::Left && key.modifiers.contains(KeyModifiers::CONTROL))
            || key.code == KeyCode::Char('<')
        {
            self.adjust_split(-2);
            return Ok(());
        }
        if (key.code == KeyCode::Right && key.modifiers.contains(KeyModifiers::CONTROL))
            || key.code == KeyCode::Char('>')
        {
            self.adjust_split(2);
            return Ok(());
        }
        if key.code == KeyCode::Char('=') {
            self.reset_split();
            return Ok(());
        }

        // Global shortcuts
        match key.code {
            KeyCode::Char('q') => {
                self.quit = true;
                return Ok(());
            }
            KeyCode::Char('?') => {
                self.show_help = true;
                return Ok(());
            }
            KeyCode::Tab => {
                self.active_pane = match self.active_pane {
                    ActivePane::Threads => ActivePane::Detail,
                    ActivePane::Detail => ActivePane::Threads,
                };
                return Ok(());
            }
            KeyCode::BackTab => {
                self.active_pane = match self.active_pane {
                    ActivePane::Threads => ActivePane::Detail,
                    ActivePane::Detail => ActivePane::Threads,
                };
                return Ok(());
            }
            KeyCode::Char('r') => {
                if self.current_thread.is_some() {
                    self.reply_mode = true;
                    self.reply_buffer = TextBuffer::default();
                    self.set_status("Compose reply \u{b7} Enter/Ctrl+Enter to send, Esc to cancel");
                }
                return Ok(());
            }
            KeyCode::Char('x') => {
                self.toggle_resolve()?;
                return Ok(());
            }
            KeyCode::Char('a') => {
                self.open_first_attachment()?;
                return Ok(());
            }
            _ => {}
        }

        match self.active_pane {
            ActivePane::Threads => self.handle_threads_key(key),
            ActivePane::Detail => self.handle_detail_key(key),
        }

        Ok(())
    }

    fn handle_threads_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.select_next_thread();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_prev_thread();
            }
            KeyCode::Right | KeyCode::Enter | KeyCode::Char('l') => {
                self.active_pane = ActivePane::Detail;
            }
            _ => {}
        }
    }

    fn handle_detail_key(&mut self, key: KeyEvent) {
        // If there are question cards, deliver arrow/selection input to the active card
        if !self.question_cards.is_empty() {
            let active_idx = self.active_card_idx.min(self.question_cards.len() - 1);
            let action = self.question_cards[active_idx].handle_key(key);

            match action {
                QuestionCardAction::Changed => {
                    let (message_id, prompt, picks) = {
                        let card = &mut self.question_cards[active_idx];
                        if !card.is_dirty {
                            return;
                        }
                        card.is_dirty = false;
                        (
                            card.question.message_id.clone(),
                            card.question.prompt.clone(),
                            vec![PickItem {
                                key: card.question.key.clone(),
                                revision: card.question.revision,
                                answer: Some(card.build_answer()),
                            }],
                        )
                    };
                    match self.client.save_picks(&message_id, picks) {
                        Ok(resp) => {
                            let card = &mut self.question_cards[active_idx];
                            for q in resp.questions {
                                if q.key == card.question.key {
                                    card.question.revision = q.revision;
                                    card.question.answer = q.answer;
                                }
                            }
                            self.set_status(format!("Answer recorded for {prompt}"));
                        }
                        Err(e) => {
                            self.set_status(format!("Failed to record pick: {e}"));
                        }
                    }
                    return;
                }
                QuestionCardAction::NextCard => {
                    if self.active_card_idx + 1 < self.question_cards.len() {
                        self.active_card_idx += 1;
                        self.set_status(format!(
                            "Question {} of {}",
                            self.active_card_idx + 1,
                            self.question_cards.len()
                        ));
                    }
                    return;
                }
                QuestionCardAction::PrevCard => {
                    if self.active_card_idx > 0 {
                        self.active_card_idx -= 1;
                        self.set_status(format!(
                            "Question {} of {}",
                            self.active_card_idx + 1,
                            self.question_cards.len()
                        ));
                    }
                    return;
                }
                QuestionCardAction::Unhandled => {}
            }
        }

        // Scroll or pane movement
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => {
                self.active_pane = ActivePane::Threads;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll_offset = self.scroll_offset.saturating_add(2);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll_offset = self.scroll_offset.saturating_sub(2);
            }
            KeyCode::PageDown => {
                self.scroll_offset = self.scroll_offset.saturating_add(10);
            }
            KeyCode::PageUp => {
                self.scroll_offset = self.scroll_offset.saturating_sub(10);
            }
            _ => {}
        }
    }

    /// Handle mouse input.
    pub(crate) fn handle_mouse(&mut self, mouse: MouseEvent) -> Result<()> {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let div = self.geometry.divider_col;
                let within_div =
                    mouse.column >= div.saturating_sub(1) && mouse.column <= div.saturating_add(1);
                let in_main_rows = mouse.row >= self.geometry.threads_rect.y
                    && mouse.row < self.geometry.threads_rect.bottom();
                if within_div && in_main_rows {
                    self.dragging_divider = true;
                    return Ok(());
                }

                let hit = self.geometry.thread_rows.iter().find(|(rect, _)| {
                    mouse.column >= rect.x
                        && mouse.column < rect.right()
                        && mouse.row >= rect.y
                        && mouse.row < rect.bottom()
                });
                if let Some((_, idx)) = hit {
                    if *idx != self.selected_thread_idx {
                        self.selected_thread_idx = *idx;
                        self.load_thread(*idx);
                    }
                    self.active_pane = ActivePane::Threads;
                    return Ok(());
                }

                let in_detail = mouse.column >= self.geometry.detail_rect.x
                    && mouse.column < self.geometry.detail_rect.right()
                    && mouse.row >= self.geometry.detail_rect.y
                    && mouse.row < self.geometry.detail_rect.bottom();
                if in_detail {
                    self.active_pane = ActivePane::Detail;
                    return Ok(());
                }

                let in_threads = mouse.column >= self.geometry.threads_rect.x
                    && mouse.column < self.geometry.threads_rect.right()
                    && mouse.row >= self.geometry.threads_rect.y
                    && mouse.row < self.geometry.threads_rect.bottom();
                if in_threads {
                    self.active_pane = ActivePane::Threads;
                    return Ok(());
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.dragging_divider {
                    let total_width = if self.geometry.area.width > 0 {
                        self.geometry.area.width
                    } else if self.last_area.width > 0 {
                        self.last_area.width
                    } else {
                        160
                    };
                    self.split_ratio = drag_to_ratio(mouse.column, total_width);
                    let layout = compute_layout(self.geometry.area, self.split_ratio, self.reply_mode);
                    self.geometry.threads_rect = layout.threads;
                    self.geometry.detail_rect = layout.detail;
                    self.geometry.divider_col = layout.divider_col;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if self.dragging_divider {
                    self.dragging_divider = false;
                    save_split_ratio(self.split_ratio);
                }
            }
            MouseEventKind::ScrollDown => {
                if mouse.column < self.geometry.divider_col {
                    self.select_next_thread();
                } else {
                    self.scroll_offset = self.scroll_offset.saturating_add(3);
                }
            }
            MouseEventKind::ScrollUp => {
                if mouse.column < self.geometry.divider_col {
                    self.select_prev_thread();
                } else {
                    self.scroll_offset = self.scroll_offset.saturating_sub(3);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn submit_reply(&mut self) -> Result<()> {
        let Some(detail) = &self.current_thread else { return Ok(()) };
        let Some(last_msg) = detail.thread.messages.last() else { return Ok(()) };

        let words = self.reply_buffer.text().trim().to_owned();
        let question_refs: Vec<ReplyQuestionRef> = self
            .question_cards
            .iter()
            .filter(|c| c.is_answered())
            .map(|c| ReplyQuestionRef { key: c.question.key.clone(), revision: c.question.revision })
            .collect();
        if words.is_empty() && question_refs.is_empty() {
            self.set_status("Write a reply or answer a question first");
            return Ok(());
        }

        match self.client.send_reply(&last_msg.id, &words, question_refs, None, None) {
            Ok(_) => {
                self.reply_mode = false;
                self.reply_buffer = TextBuffer::default();
                self.set_status("\u{2713} Reply sent to agent");
                self.refresh_list_and_current();
            }
            Err(e) => {
                self.set_status(format!("Failed to send reply: {e}"));
            }
        }
        Ok(())
    }

    fn toggle_resolve(&mut self) -> Result<()> {
        let Some(detail) = &self.current_thread else { return Ok(()) };
        let Some(last_msg) = detail.thread.messages.last() else { return Ok(()) };

        let current_resolved = detail.thread.resolved_at.is_some();
        let new_resolved = !current_resolved;

        match self.client.resolve_thread(&last_msg.id, new_resolved) {
            Ok(_) => {
                let label = if new_resolved { "resolved" } else { "reopened" };
                self.set_status(format!("\u{2713} Thread {label}"));
                self.refresh_list_and_current();
            }
            Err(e) => {
                self.set_status(format!("Failed to update resolve state: {e}"));
            }
        }
        Ok(())
    }

    fn open_first_attachment(&mut self) -> Result<()> {
        let first_att = self.current_thread.as_ref().and_then(|detail| {
            for msg in &detail.thread.messages {
                if let Some(attachments) = &msg.attachments {
                    if let Some(att) = attachments.first() {
                        return Some((att.name.clone(), att.path.clone()));
                    }
                }
            }
            None
        });

        match first_att {
            Some((name, path_str)) => {
                let path = PathBuf::from(&path_str);
                if path.exists() {
                    self.set_status(format!("Opening attachment: {name}"));
                } else {
                    self.set_status(format!("Attachment file not found: {path_str}"));
                }
            }
            None => {
                self.set_status("No attachments in this thread");
            }
        }
        Ok(())
    }
}

impl Drop for InboxApp {
    fn drop(&mut self) {
        self.sse_stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn test_resize_event_triggers_relayout() -> Result<()> {
        let info = InboxConnectionInfo {
            url: "http://127.0.0.1:0".to_owned(),
            server_session: "test".to_owned(),
            config_path: PathBuf::new(),
        };
        let mut app = InboxApp::new(info)?;
        app.handle_event(&Event::Resize(160, 60))?;
        assert_eq!(app.geometry.area, Rect::new(0, 0, 160, 60));
        assert_eq!(app.geometry.threads_rect.width, 56);
        assert_eq!(app.geometry.detail_rect.width, 104);

        app.handle_event(&Event::Resize(80, 24))?;
        assert_eq!(app.geometry.area, Rect::new(0, 0, 80, 24));
        assert_eq!(app.geometry.threads_rect.width, 28);
        assert_eq!(app.geometry.detail_rect.width, 52);
        Ok(())
    }

    #[test]
    fn test_keyboard_resize_and_reset() -> Result<()> {
        let info = InboxConnectionInfo {
            url: "http://127.0.0.1:0".to_owned(),
            server_session: "test".to_owned(),
            config_path: PathBuf::new(),
        };
        let mut app = InboxApp::new(info)?;
        app.handle_event(&Event::Resize(160, 60))?;
        assert_eq!(app.geometry.threads_rect.width, 56);

        // Move right with '>'
        app.handle_key(KeyEvent::new(KeyCode::Char('>'), KeyModifiers::NONE))?;
        assert_eq!(app.geometry.threads_rect.width, 58);

        // Move left with '<'
        app.handle_key(KeyEvent::new(KeyCode::Char('<'), KeyModifiers::NONE))?;
        assert_eq!(app.geometry.threads_rect.width, 56);

        // Move with Ctrl+Left / Ctrl+Right
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL))?;
        assert_eq!(app.geometry.threads_rect.width, 58);
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL))?;
        assert_eq!(app.geometry.threads_rect.width, 56);

        // Reset with '='
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL))?;
        assert_eq!(app.geometry.threads_rect.width, 58);
        app.handle_key(KeyEvent::new(KeyCode::Char('='), KeyModifiers::NONE))?;
        assert_eq!(app.geometry.threads_rect.width, 56);
        Ok(())
    }

    #[test]
    fn test_live_proof_flow() -> Result<()> {
        let info = match super::super::discovery::discover_inbox(None) {
            Ok(info) => info,
            Err(_) => return Ok(()),
        };
        let mut app = InboxApp::new(info)?;
        // Switch to detail pane
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
        assert_eq!(app.active_pane, ActivePane::Detail);

        // Pick the choice
        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE))?;

        // Open reply mode
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE))?;
        assert!(app.reply_mode);

        // Type reply
        for c in "Approved from TUI proof!".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))?;
        }

        // Send reply
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

        // Resolve thread
        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))?;
        Ok(())
    }
}
