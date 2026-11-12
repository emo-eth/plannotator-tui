use super::model::{Contents, segment};
use super::{Mode, ReviewAction, ReviewPane, Side, View};
use crate::app::compose::{Compose, ComposeAction};
use crate::inbox::client::Client;
use anyhow::{Context, Result, ensure};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

impl ReviewPane {
    pub(crate) fn handle_key(&mut self, key: KeyEvent, client: &Client) -> Result<ReviewAction> {
        if matches!(self.mode, Mode::Comment(_) | Mode::Suggestion(_) | Mode::Note(_) | Mode::Jump) {
            match self.compose.handle_key(key) {
                ComposeAction::Cancel => self.mode = Mode::Browse,
                ComposeAction::Edited => {}
                ComposeAction::Save => match &self.mode {
                    Mode::Comment(annotation) => self.save_comment(client, annotation.clone(), false)?,
                    Mode::Suggestion(annotation) => self.save_comment(client, annotation.clone(), true)?,
                    Mode::Note(kind) => {
                        self.note = self.compose.value();
                        self.mode = Mode::Send(*kind);
                    }
                    Mode::Jump => {
                        let value = self.compose.value();
                        let (path, line) = value
                            .rsplit_once(':')
                            .filter(|(_, n)| n.parse::<usize>().is_ok())
                            .map_or((value.as_str(), 0), |(path, n)| (path, n.parse().unwrap_or_default()));
                        self.jump_to(path, line, self.side == Side::Old)?;
                        self.mode = Mode::Browse;
                    }
                    _ => {}
                },
            }
            return Ok(ReviewAction::Stay);
        }
        if let Mode::Remove(id) = &self.mode {
            match key.code {
                KeyCode::Enter => self.remove_comment(client, &id.clone())?,
                KeyCode::Esc => self.mode = Mode::Comments,
                _ => {}
            }
            return Ok(ReviewAction::Stay);
        }
        if let Mode::Send(kind) = self.mode {
            let options = self.send_options();
            let at = options.iter().position(|option| *option == kind).unwrap_or_default();
            match key.code {
                KeyCode::Esc => self.mode = Mode::Browse,
                KeyCode::Down | KeyCode::Char('j') => {
                    if let Some(next) = options.get((at + 1) % options.len().max(1)) {
                        self.mode = Mode::Send(*next);
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if let Some(next) =
                        options.get((at + options.len().saturating_sub(1)) % options.len().max(1))
                    {
                        self.mode = Mode::Send(*next);
                    }
                }
                KeyCode::Char('n') => {
                    self.compose = Compose::with_text(&self.note);
                    self.mode = Mode::Note(kind);
                }
                KeyCode::Enter => self.send(client, kind)?,
                _ => {}
            }
            return Ok(ReviewAction::Stay);
        }
        if matches!(self.mode, Mode::Comments) {
            match key.code {
                KeyCode::Esc | KeyCode::Char('m') => self.mode = Mode::Browse,
                KeyCode::Down | KeyCode::Char('j') => {
                    self.selected_comment =
                        self.selected_comment.saturating_add(1).min(self.records.len().saturating_sub(1));
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.selected_comment = self.selected_comment.saturating_sub(1);
                }
                KeyCode::Enter => self.show_comment()?,
                KeyCode::Char('e') => self.edit_comment(false)?,
                KeyCode::Char('S') => self.edit_comment(true)?,
                KeyCode::Char('d') => {
                    self.writable()?;
                    if let Some(record) = self.records.get(self.selected_comment) {
                        self.mode = Mode::Remove(record.id.clone());
                    }
                }
                KeyCode::Char('R') => self.refresh_comments(client)?,
                _ => {}
            }
            return Ok(ReviewAction::Stay);
        }
        self.browse_key(key, client)
    }

    fn browse_key(&mut self, key: KeyEvent, client: &Client) -> Result<ReviewAction> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                ensure!(
                    self.pending.is_none(),
                    "A send is unconfirmed; s retries it, D explicitly forgets it before closing"
                );
                if self.selection.take().is_none() {
                    return Ok(ReviewAction::Close);
                }
            }
            KeyCode::Char('c') => self.new_comment(false, false)?,
            KeyCode::Char('C') => self.new_comment(true, false)?,
            KeyCode::Char('S') => self.new_comment(false, true)?,
            KeyCode::Char('a') => return self.ask_selection(),
            KeyCode::Char('z') => return self.show_me(client, false),
            KeyCode::Char('Z') => return self.show_me(client, true),
            KeyCode::Char('O') => {
                let mut url = url::Url::parse(client.url())?;
                url.set_fragment(Some(&format!(
                    "thread={}&guide={}",
                    segment(&self.thread_id),
                    segment(&self.message.id)
                )));
                crate::inbox::attachments::open_browser(url.as_str())?;
            }
            KeyCode::Char('v') => {
                self.selection = if self.selection.is_some() { None } else { Some(self.cursor) };
            }
            KeyCode::Char('o') => {
                self.side = match self.side {
                    Side::Old => Side::New,
                    Side::New => Side::Old,
                };
                self.status = format!(
                    "Comment side: {}",
                    if self.side == Side::Old { "old" } else { "new (removed lines use old)" }
                );
            }
            KeyCode::Char('m') => {
                self.refresh_comments(client)?;
                self.mode = Mode::Comments;
            }
            KeyCode::Char('x') => self.toggle_reviewed(client)?,
            KeyCode::Char('V') => self.toggle_viewed(client)?,
            KeyCode::Char('s') => self.begin_send(client)?,
            KeyCode::Char('p') => {
                self.writable()?;
                ensure!(
                    self.code_review && self.message.guide.review,
                    "Forge posting requires a code review"
                );
                return Ok(ReviewAction::PostForge);
            }
            KeyCode::Char('D') if self.pending.is_some() => {
                self.pending = None;
                self.status = "Forgot the unconfirmed send; the server may have received it. Check the thread before sending again.".into();
            }
            KeyCode::Char('E') => self.expand(client)?,
            KeyCode::Char('/') => {
                self.compose = Compose::default();
                self.mode = Mode::Jump;
            }
            KeyCode::Tab => {
                let location = self.rows.get(self.cursor).and_then(|row| row.location.clone());
                self.view = if self.view == View::Walkthrough { View::Files } else { View::Walkthrough };
                self.selection = None;
                self.rebuild(self.width.max(1));
                if let Some(location) = location {
                    self.land(&location);
                } else {
                    self.cursor = 0;
                }
            }
            KeyCode::Char(']') => self.next_stop(true),
            KeyCode::Char('[') => self.next_stop(false),
            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1),
            KeyCode::PageDown => self.move_cursor(self.height as isize),
            KeyCode::PageUp => self.move_cursor(-(self.height as isize)),
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_cursor((self.height / 2) as isize);
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_cursor(-((self.height / 2) as isize));
            }
            KeyCode::Home | KeyCode::Char('g') => self.cursor = 0,
            KeyCode::End | KeyCode::Char('G') => self.cursor = self.rows.len().saturating_sub(1),
            KeyCode::Left | KeyCode::Char('h') => self.horizontal = self.horizontal.saturating_sub(8),
            KeyCode::Right | KeyCode::Char('l') => self.horizontal = self.horizontal.saturating_add(8),
            KeyCode::Char('R') => {
                ensure!(self.pending.is_none(), "Retry or explicitly forget the unconfirmed send first");
                let replacement = Self::load(client, &self.thread_id, &self.message.id)?;
                self.records = replacement.records;
                self.message = replacement.message;
                self.read_only = replacement.read_only;
                self.code_review = replacement.code_review;
                self.verdict = replacement.verdict;
                self.invalidate();
            }
            _ => {}
        }
        Ok(ReviewAction::Stay)
    }

    fn move_cursor(&mut self, delta: isize) {
        self.cursor = self.cursor.saturating_add_signed(delta).min(self.rows.len().saturating_sub(1));
    }

    fn next_stop(&mut self, forward: bool) {
        let mut previous = None;
        let stops: Vec<_> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let key = if self.view == View::Walkthrough { row.chapter } else { row.file };
                let changed = key.is_some() && key != previous;
                previous = key;
                changed.then_some(index)
            })
            .collect();
        if let Some(at) = if forward {
            stops.into_iter().find(|at| *at > self.cursor)
        } else {
            stops.into_iter().rev().find(|at| *at < self.cursor)
        } {
            self.cursor = at;
        }
    }

    fn expand(&mut self, client: &Client) -> Result<()> {
        let index =
            self.rows.get(self.cursor).and_then(|row| row.file).context("Move into a changed file first")?;
        if self.expanded.as_ref().is_some_and(|(file, _)| *file == index) {
            self.expanded = None;
        } else {
            let file = self.files.get(index).context("Review file is missing")?;
            let contents: Contents = client
                .get_json("GET", &format!("{}?path={}", self.guide_path("/file"), segment(&file.path)))?;
            ensure!(
                contents.old_content.is_some() || contents.new_content.is_some(),
                "Whole-file contents are unavailable for this snapshot"
            );
            self.expanded = Some((index, contents));
        }
        self.invalidate();
        Ok(())
    }

    pub(super) fn jump_to(&mut self, path: &str, line: usize, old: bool) -> Result<()> {
        let file = self
            .files
            .iter()
            .position(|file| file.path == path)
            .context("That path is not in this review")?;
        self.view = View::Files;
        self.rebuild(self.width.max(1));
        self.cursor = self
            .rows
            .iter()
            .position(|row| {
                row.file == Some(file)
                    && (line == 0
                        || row.location.as_ref().is_some_and(|location| {
                            if old { location.old == Some(line) } else { location.new == Some(line) }
                        }))
            })
            .context(
                "That line is outside the stored diff; move to the file and E expands exact snapshot context",
            )?;
        self.selection = None;
        Ok(())
    }

    fn land(&mut self, wanted: &super::Location) {
        if let Some(at) = self.rows.iter().position(|row| {
            row.location.as_ref().is_some_and(|location| {
                location.file == wanted.file && location.old == wanted.old && location.new == wanted.new
            })
        }) {
            self.cursor = at;
        }
    }
}
