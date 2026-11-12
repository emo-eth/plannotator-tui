use std::ops::Range;
use std::rc::Rc;

use anyhow::{Result, anyhow, ensure};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use super::operations::{MergeConfirmation, OwnerPane};
use super::risk::RiskPane;
use super::{
    Client, DiffScope, DiffSide, FileComment, FileLevelComment, GroupKind, Pull, PullChange, PullDetails,
    PullDiff, PullOptions, ReviewAction, ReviewPost, ReviewSubmission, ThreadPulls,
};
use crate::app::compose::{Compose, ComposeAction};
use crate::inbox::client::HttpError;
use crate::theme::palette;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PullAction {
    Stay,
    Close,
    Guide { key: String },
}

#[derive(Debug)]
pub(crate) struct ThreadPullsPane {
    thread_id: String,
    answer: ThreadPulls,
    selected: usize,
    add: Option<Compose>,
    undo: Option<String>,
    review: Option<PullReview>,
    status: Option<String>,
    remove_all_armed: bool,
    direct_review: bool,
}

impl ThreadPullsPane {
    pub(crate) fn load(client: &Client, thread_id: &str) -> Result<Self> {
        Ok(Self {
            thread_id: thread_id.to_owned(),
            answer: client.thread_pulls(thread_id)?,
            selected: 0,
            add: None,
            undo: None,
            review: None,
            status: None,
            remove_all_armed: false,
            direct_review: false,
        })
    }

    pub(crate) fn load_pull(client: &Client, key: &str) -> Result<Self> {
        Ok(Self {
            thread_id: String::new(),
            answer: ThreadPulls::default(),
            selected: 0,
            add: None,
            undo: None,
            review: Some(PullReview::load(client, key)?),
            status: None,
            remove_all_armed: false,
            direct_review: true,
        })
    }

    pub(crate) fn refresh(&mut self, client: &Client) -> Result<()> {
        if self.direct_review {
            if let Some(review) = &mut self.review {
                review.details = Some(client.pull_details(&review.pull.key)?);
            }
            return Ok(());
        }
        let selected = self.answer.ordered().nth(self.selected).map(|p| p.pull.clone());
        self.answer = client.thread_pulls(&self.thread_id)?;
        self.selected = selected
            .and_then(|key| self.answer.ordered().position(|p| p.pull == key))
            .unwrap_or_else(|| self.selected.min(self.answer.pulls.len().saturating_sub(1)));
        Ok(())
    }

    pub(crate) fn paste(&mut self, text: &str) {
        if let Some(compose) = &mut self.add {
            compose.insert_text(text);
        } else if let Some(review) = &mut self.review {
            review.paste(text);
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent, client: &Client) -> Result<PullAction> {
        if key.kind != KeyEventKind::Press {
            return Ok(PullAction::Stay);
        }
        let result = self.handle(key, client);
        if let Err(error) = &result {
            self.status = Some(format!("{error:#}"));
        }
        result
    }

    fn handle(&mut self, key: KeyEvent, client: &Client) -> Result<PullAction> {
        if let Some(review) = &mut self.review {
            let action = review.handle_key(key, client)?;
            if action == PullAction::Close {
                if self.direct_review {
                    return Ok(PullAction::Close);
                }
                self.review = None;
                self.refresh(client)?;
                self.status = None;
                return Ok(PullAction::Stay);
            }
            self.status = None;
            return Ok(action);
        }
        if let Some(compose) = &mut self.add {
            match compose.handle_key(key) {
                ComposeAction::Cancel => self.add = None,
                ComposeAction::Edited => self.status = None,
                ComposeAction::Save => {
                    let change = PullChange::add_text(&compose.value());
                    if !change.add.is_empty() {
                        self.answer = client.change_thread_pulls(&self.thread_id, &change)?;
                        self.undo = None;
                        self.add = None;
                        self.status = Some("Pull requests added to this thread.".to_owned());
                    }
                }
            }
            return Ok(PullAction::Stay);
        }
        if key.code != KeyCode::Char('D') {
            self.remove_all_armed = false;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(PullAction::Close),
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected =
                    self.selected.saturating_add(1).min(self.answer.pulls.len().saturating_sub(1));
            }
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = self.answer.pulls.len().saturating_sub(1),
            KeyCode::Char('a') => {
                self.add = Some(Compose::default());
                self.status = None;
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                let reference = self.answer.ordered().nth(self.selected).map(|pull| pull.reference.clone());
                if let Some(reference) = reference {
                    self.answer =
                        client.change_thread_pulls(&self.thread_id, &PullChange::remove(&reference))?;
                    self.selected = self.selected.min(self.answer.pulls.len().saturating_sub(1));
                    self.status = Some(format!("Removed {reference}. Press u to undo."));
                    self.undo = Some(reference);
                }
            }
            KeyCode::Char('D') => {
                if self.remove_all_armed {
                    self.answer = client.change_thread_pulls(&self.thread_id, &PullChange::remove_all())?;
                    self.selected = 0;
                    self.remove_all_armed = false;
                    self.undo = None;
                    self.status = Some(
                        "Removed all PRs from this thread. Recent removals remain listed below.".to_owned(),
                    );
                } else {
                    self.remove_all_armed = true;
                    self.status = Some("Press D again to remove every PR from this thread.".to_owned());
                }
            }
            KeyCode::Char('u') => {
                if let Some(reference) = &self.undo {
                    let change = PullChange { add: vec![reference.clone()], remove: Vec::new() };
                    self.answer = client.change_thread_pulls(&self.thread_id, &change)?;
                    self.undo = None;
                    self.status = Some("Removal undone.".to_owned());
                }
            }
            KeyCode::Char('r') => {
                self.refresh(client)?;
                self.status = None;
            }
            KeyCode::Enter => {
                if let Some(pull) = self.answer.ordered().nth(self.selected) {
                    self.review = Some(PullReview::load(client, &pull.pull)?);
                    self.status = None;
                }
            }
            _ => {}
        }
        Ok(PullAction::Stay)
    }

    pub(crate) fn draw(&self, frame: &mut Frame, area: Rect) {
        if let Some(review) = &self.review {
            review.draw(frame, area, self.status.as_deref());
            return;
        }
        let [title, body, status, help] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(2),
            Constraint::Length(1),
        ])
        .areas(area);
        frame.render_widget(
            Paragraph::new(format!(
                "Pull requests · {} · {} linked",
                self.thread_id,
                self.answer.pulls.len()
            ))
            .style(Style::new().fg(palette().accent).add_modifier(Modifier::BOLD)),
            title,
        );
        let mut lines = Vec::new();
        let mut index = 0;
        let mut selection_line = 0;
        for group in &self.answer.groups {
            if group.kind == GroupKind::Stack {
                lines.push(Line::from(format!("Stack of {} (base first)", group.pulls.len())));
            }
            for key in &group.pulls {
                let Some(pull) = self.answer.pulls.iter().find(|p| &p.pull == key) else { continue };
                let selected = index == self.selected;
                if selected {
                    selection_line = lines.len();
                }
                let mut text = format!(
                    "{} {} · {} · {}",
                    if selected { ">" } else { " " },
                    pull.reference,
                    pull.state.label(),
                    pull.title.as_deref().unwrap_or(&pull.reference)
                );
                for extra in pull.extras() {
                    text.push_str(" · ");
                    text.push_str(extra);
                }
                let style =
                    if selected { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new() };
                lines.push(Line::styled(text, style));
                lines.push(Line::from(format!("  {}", pull.url)));
                index += 1;
            }
        }
        if self.answer.pulls.is_empty() {
            lines.push(Line::from("No pull requests on this thread. Press a to add a PR or MR."));
        }
        if !self.answer.removed.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from("Removed in the last seven days:"));
            for pull in &self.answer.removed {
                lines.push(Line::from(format!(
                    "  {} · {}",
                    pull.reference,
                    pull.removed_at.as_deref().unwrap_or("")
                )));
            }
        }
        let scroll = selection_line.saturating_sub(usize::from(body.height).saturating_sub(3));
        frame
            .render_widget(Paragraph::new(lines).scroll((scroll.min(usize::from(u16::MAX)) as u16, 0)), body);
        frame.render_widget(
            Paragraph::new(self.status.as_deref().unwrap_or("")).wrap(Wrap { trim: false }),
            status,
        );
        frame.render_widget(
            Paragraph::new(
                "j/k select · Enter review · a add · d remove · D remove all · u undo · r refresh · Esc back",
            ),
            help,
        );
        if let Some(compose) = &self.add {
            draw_compose(
                frame,
                area,
                compose,
                "Add PRs: owner/repo#123, group/project!45, or URLs · Enter add · Esc cancel",
            );
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PatchLine {
    text: Range<usize>,
    file: Option<Rc<str>>,
    line: Option<(u64, DiffSide)>,
}

fn patch_lines(patch: &str) -> Vec<PatchLine> {
    let mut file = None;
    let mut old_file = None;
    let mut old = 0;
    let mut new = 0;
    let mut in_hunk = false;
    let mut offset = 0;
    patch
        .split_inclusive('\n')
        .map(|raw| {
            let text = raw.trim_end_matches(['\r', '\n']);
            let range = offset..offset + text.len();
            offset += raw.len();
            let mut location = None;
            if text.starts_with("diff --git ") {
                file = None;
                old_file = None;
                in_hunk = false;
            } else if let Some(path) = text.strip_prefix("--- ").filter(|_| !in_hunk) {
                old_file = patch_path(path, "a/");
                in_hunk = false;
            } else if let Some(path) = text.strip_prefix("+++ ").filter(|_| !in_hunk) {
                file = if path == "/dev/null" { old_file.clone() } else { patch_path(path, "b/") };
                in_hunk = false;
            } else if let Some(hunk) = text.strip_prefix("@@ ") {
                let mut parts = hunk.split_whitespace();
                let start = |part: Option<&str>, prefix: char| {
                    part.and_then(|s| s.strip_prefix(prefix))
                        .and_then(|s| s.split(',').next())
                        .and_then(|s| s.parse::<u64>().ok())
                };
                match (start(parts.next(), '-'), start(parts.next(), '+')) {
                    (Some(a), Some(b)) => {
                        old = a;
                        new = b;
                        in_hunk = true;
                    }
                    _ => in_hunk = false,
                }
            } else if in_hunk {
                match text.as_bytes().first() {
                    Some(b'-') => {
                        location = Some((old, DiffSide::Left));
                        old += 1;
                    }
                    Some(b'+') => {
                        location = Some((new, DiffSide::Right));
                        new += 1;
                    }
                    Some(b' ') => {
                        location = Some((new, DiffSide::Right));
                        old += 1;
                        new += 1;
                    }
                    _ => {}
                }
            }
            PatchLine { text: range, file: file.clone(), line: location }
        })
        .collect()
}

fn patch_path(path: &str, prefix: &str) -> Option<Rc<str>> {
    let path = path.split('\t').next()?;
    if path == "/dev/null" || path.starts_with('"') {
        return None;
    }
    path.strip_prefix(prefix).map(Rc::from)
}

#[derive(Debug)]
enum Editing {
    Body,
    Line { path: String, line: u64, side: DiffSide },
    File { path: String },
}

#[derive(Debug)]
struct PullReview {
    pull: Pull,
    diff: PullDiff,
    lines: Vec<PatchLine>,
    details: Option<PullDetails>,
    selected: usize,
    details_scroll: usize,
    show_details: bool,
    body: String,
    comments: Vec<FileComment>,
    file_comments: Vec<FileLevelComment>,
    editor: Option<(Editing, Compose)>,
    pending: Option<ReviewPost>,
    status: Option<String>,
    discard_armed: bool,
    merge: Option<MergeConfirmation>,
    merge_uncertain: bool,
    risk: Option<RiskPane>,
    owner: Option<OwnerPane>,
}

impl PullReview {
    fn load(client: &Client, key: &str) -> Result<Self> {
        let list = client.pulls(PullOptions { others: true, everywhere: true, refresh: false })?;
        let pull = list.pulls.into_iter().find(|pull| pull.key == key)
            .ok_or_else(|| anyhow!("This PR is linked to the thread but is not in the Inbox's forge review list. Its URL remains available in the PR list."))?;
        let diff = client.pull_diff(key, DiffScope::Layer)?;
        let lines = patch_lines(&diff.patch);
        Ok(Self {
            pull,
            diff,
            lines,
            details: None,
            selected: 0,
            details_scroll: 0,
            show_details: false,
            body: String::new(),
            comments: Vec::new(),
            file_comments: Vec::new(),
            editor: None,
            pending: None,
            status: None,
            discard_armed: false,
            merge: None,
            merge_uncertain: false,
            risk: None,
            owner: None,
        })
    }

    fn paste(&mut self, text: &str) {
        if let Some(risk) = &mut self.risk {
            risk.paste(text);
        } else if let Some((_, compose)) = &mut self.editor {
            compose.insert_text(text);
        }
    }

    fn has_draft(&self) -> bool {
        !self.body.is_empty() || !self.comments.is_empty() || !self.file_comments.is_empty()
    }

    fn handle_key(&mut self, key: KeyEvent, client: &Client) -> Result<PullAction> {
        if key.kind != KeyEventKind::Press {
            return Ok(PullAction::Stay);
        }
        if let Some(risk) = &mut self.risk {
            if risk.handle(key, client)? {
                self.risk = None;
            }
            return Ok(PullAction::Stay);
        }
        if let Some(owner) = &mut self.owner {
            if owner.handle(key, client)? {
                self.status = Some(owner.status.clone());
                self.owner = None;
            }
            return Ok(PullAction::Stay);
        }
        if self.merge.is_some() {
            match key.code {
                KeyCode::Esc => self.merge = None,
                KeyCode::Enter | KeyCode::Char('y') => {
                    if let Some(merge) = self.merge.take() {
                        self.status = Some("Merge not confirmed. Refresh before deciding again; it may already have reached the forge.".into());
                        self.merge_uncertain = true;
                        let merged = client.merge_pull(&merge.key, &merge.head)?;
                        self.merge_uncertain = false;
                        self.pull.state = super::PullState::Merged;
                        self.status = Some(format!(
                            "Merged on {} · method {} · {}",
                            self.pull.platform.label(),
                            merged
                                .method
                                .map_or_else(|| "repository default".into(), |method| format!("{method:?}")),
                            merged.url
                        ));
                    }
                }
                _ => {}
            }
            return Ok(PullAction::Stay);
        }
        if let Some((editing, compose)) = &mut self.editor {
            match compose.handle_key(key) {
                ComposeAction::Edited => {}
                ComposeAction::Cancel => self.editor = None,
                ComposeAction::Save => {
                    let body = compose.value();
                    match editing {
                        Editing::Body => self.body = body,
                        Editing::Line { path, line, side } => {
                            if !body.trim().is_empty() {
                                self.comments.push(FileComment {
                                    path: path.clone(),
                                    line: *line,
                                    side: *side,
                                    body,
                                    start_line: None,
                                    start_side: None,
                                });
                            }
                        }
                        Editing::File { path } => {
                            if !body.trim().is_empty() {
                                self.file_comments.push(FileLevelComment { path: path.clone(), body });
                            }
                        }
                    }
                    self.editor = None;
                    self.status = Some("Draft saved locally. Nothing posted.".to_owned());
                }
            }
            return Ok(PullAction::Stay);
        }
        if self.pending.is_some() {
            match key.code {
                KeyCode::Esc => {
                    self.status = Some("Post cancelled; draft kept. If a prior attempt was unconfirmed, check the PR before posting again.".to_owned());
                    self.pending = None;
                }
                KeyCode::Enter | KeyCode::Char('y') => self.post(client)?,
                KeyCode::Down | KeyCode::Char('j') => {
                    self.details_scroll = self.details_scroll.saturating_add(1);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.details_scroll = self.details_scroll.saturating_sub(1);
                }
                KeyCode::PageDown => self.details_scroll = self.details_scroll.saturating_add(20),
                KeyCode::PageUp => self.details_scroll = self.details_scroll.saturating_sub(20),
                KeyCode::Home => self.details_scroll = 0,
                _ => {}
            }
            return Ok(PullAction::Stay);
        }
        self.handle_review_key(key, client)
    }

    fn handle_review_key(&mut self, key: KeyEvent, client: &Client) -> Result<PullAction> {
        if key.code != KeyCode::Char('D') && key.code != KeyCode::Esc && key.code != KeyCode::Char('q') {
            self.discard_armed = false;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if self.has_draft() && !self.discard_armed {
                    self.discard_armed = true;
                    self.status =
                        Some("Unposted draft: press Esc again to discard it and return.".to_owned());
                } else {
                    return Ok(PullAction::Close);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::PageDown => self.move_by(20),
            KeyCode::PageUp => self.move_by(-20),
            KeyCode::Home => {
                self.selected = 0;
                self.details_scroll = 0;
            }
            KeyCode::End if !self.show_details => self.selected = self.lines.len().saturating_sub(1),
            KeyCode::Char('v') => {
                if self.details.is_none() {
                    self.details = Some(client.pull_details(&self.pull.key)?);
                }
                self.show_details = !self.show_details;
            }
            KeyCode::Char('s') => {
                ensure!(!self.has_draft(), "Post or discard your draft before changing diff scope.");
                let scope =
                    if self.diff.scope == DiffScope::Layer { DiffScope::Stack } else { DiffScope::Layer };
                self.diff = client.pull_diff(&self.pull.key, scope)?;
                self.lines = patch_lines(&self.diff.patch);
                self.selected = 0;
                self.show_details = false;
            }
            KeyCode::Char('r') => {
                if self.show_details {
                    self.details = Some(client.pull_details(&self.pull.key)?);
                } else {
                    ensure!(!self.has_draft(), "Post or discard your draft before refreshing the diff.");
                    *self = Self::load(client, &self.pull.key)?;
                }
            }
            KeyCode::Char('b') => self.editor = Some((Editing::Body, Compose::with_text(&self.body))),
            KeyCode::Char('c' | 'f') if !self.show_details => {
                ensure!(
                    self.diff.scope == DiffScope::Layer,
                    "Stack lines do not map to one PR. Switch to its layer before adding inline comments."
                );
                let line =
                    self.lines.get(self.selected).ok_or_else(|| anyhow!("Select a changed file first."))?;
                let path = line.file.as_deref().map(str::to_owned).ok_or_else(|| {
                    anyhow!(
                        "This diff header has no unambiguous file path. Use the review body for this comment."
                    )
                })?;
                let editing = if key.code == KeyCode::Char('f') {
                    Editing::File { path }
                } else {
                    let (number, side) =
                        line.line.ok_or_else(|| anyhow!("Select a code line, not a diff header."))?;
                    Editing::Line { path, line: number, side }
                };
                self.editor = Some((editing, Compose::default()));
            }
            KeyCode::Char('u') => {
                if self.comments.pop().is_none() {
                    self.file_comments.pop();
                }
                self.status = Some(
                    "Removed the latest line comment, or file comment if no line comments remain.".to_owned(),
                );
            }
            KeyCode::Char('D') => {
                if self.discard_armed {
                    self.body.clear();
                    self.comments.clear();
                    self.file_comments.clear();
                    self.discard_armed = false;
                    self.status = Some("Draft discarded.".to_owned());
                } else {
                    self.discard_armed = true;
                    self.status = Some("Press D again to discard all draft comments.".to_owned());
                }
            }
            KeyCode::Char('p') => self.prepare(ReviewAction::Comment)?,
            KeyCode::Char('A') => self.prepare(ReviewAction::Approve)?,
            KeyCode::Char('X') => self.prepare(ReviewAction::RequestChanges)?,
            KeyCode::Char('M') => {
                ensure!(
                    !self.merge_uncertain,
                    "The last merge was not confirmed. Refresh the diff/list before deciding again."
                );
                self.merge = Some(MergeConfirmation::prepare(&self.pull, &self.diff, self.has_draft())?);
            }
            KeyCode::Char('R') => self.risk = Some(RiskPane::load(client)?),
            KeyCode::Char('H') => self.owner = Some(OwnerPane::load(client, &self.pull.key)?),
            KeyCode::Char('g') => return Ok(PullAction::Guide { key: self.pull.key.clone() }),
            _ => {}
        }
        Ok(PullAction::Stay)
    }

    fn move_by(&mut self, amount: isize) {
        if self.show_details {
            self.details_scroll = self.details_scroll.saturating_add_signed(amount);
        } else {
            self.selected =
                self.selected.saturating_add_signed(amount).min(self.lines.len().saturating_sub(1));
        }
    }

    fn prepare(&mut self, action: ReviewAction) -> Result<()> {
        if let Some(reason) = self.pull.action_refusal(action) {
            return Err(anyhow!(reason));
        }
        let head = self.diff.head_sha.as_ref().filter(|sha| !sha.is_empty()).ok_or_else(|| {
            anyhow!("The forge did not confirm the commit of this diff. Refresh before posting.")
        })?;
        ensure!(
            action != ReviewAction::Comment || self.has_draft(),
            "Write a review body or add comments before posting."
        );
        let body = if self.body.trim().is_empty()
            && self.pull.platform == super::Platform::Github
            && action != ReviewAction::Approve
        {
            ensure!(
                !self.comments.is_empty() || !self.file_comments.is_empty(),
                "Write a reason before requesting changes."
            );
            "See inline comments.".to_owned()
        } else {
            self.body.clone()
        };
        self.pending = Some(ReviewPost {
            key: self.pull.key.clone(),
            head_sha: head.clone(),
            action,
            body,
            file_comments: self.comments.clone(),
            file_level_comments: self.file_comments.clone(),
        });
        self.details_scroll = 0;
        self.status = None;
        Ok(())
    }

    fn post(&mut self, client: &Client) -> Result<()> {
        let Some(request) = &self.pending else { return Ok(()) };
        let response = client.post_pull_review(request);
        match response {
            Ok(posted) => {
                ensure!(posted.ok, "The Inbox did not confirm this post; the exact request is kept.");
                match posted.submission {
                    ReviewSubmission::Complete { posted_as } => {
                        self.complete(posted_as.or(Some(request.action)));
                    }
                    ReviewSubmission::Partial(partial) => {
                        let retry = request.retry(&partial);
                        self.body.clone_from(&retry.body);
                        self.comments.clone_from(&retry.file_comments);
                        self.file_comments.clear();
                        self.pending = Some(retry);
                        self.status = Some(format!(
                            "Partially posted: {} inline comments accepted; {} remain; decision {:?}{}. {}{}Press Enter to retry only what the forge reports missing.",
                            partial.posted_file_comment_count,
                            partial.retry.file_comments.len(),
                            partial.approval,
                            if partial.posted_as == Some(ReviewAction::Comment) {
                                " (posted as a comment; this GitLab cannot request changes)"
                            } else {
                                ""
                            },
                            partial.approval_error.map_or_else(String::new, |s| format!("{s} ")),
                            partial
                                .failed_file_comments
                                .first()
                                .map_or_else(String::new, |failure| format!("{} ", failure.error))
                        ));
                    }
                }
            }
            Err(error) => {
                if error.downcast_ref::<HttpError>().is_some_and(|error| {
                    error.status == 409 && error.code.as_deref() == Some("already_posted")
                }) {
                    self.complete(None);
                } else {
                    self.status = Some("Post not confirmed. The exact request is kept; Enter retries it, Esc keeps the draft. A stale commit must be reviewed again before a new post.".to_owned());
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    fn complete(&mut self, action: Option<ReviewAction>) {
        self.pending = None;
        self.body.clear();
        self.comments.clear();
        self.file_comments.clear();
        let verb = match action {
            Some(ReviewAction::Approve) => "Approved",
            Some(ReviewAction::RequestChanges) => "Changes requested",
            _ => "Review posted",
        };
        self.status = Some(format!("{verb} on {}: {}", self.pull.platform.label(), self.pull.url));
    }

    fn draw(&self, frame: &mut Frame, area: Rect, error: Option<&str>) {
        if let Some(risk) = &self.risk {
            risk.draw(frame, area, error);
            return;
        }
        if let Some(owner) = &self.owner {
            owner.draw(frame, area, error);
            return;
        }
        let [title, meta, body, status, help] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .areas(area);
        frame.render_widget(
            Paragraph::new(format!("{} · {}", self.pull.repo, self.pull.title))
                .style(Style::new().fg(palette().accent).add_modifier(Modifier::BOLD)),
            title,
        );
        frame.render_widget(
            Paragraph::new(format!(
                "{}...{} · {:?} · {} inline / {} file comments · {}",
                self.diff.base,
                self.diff.head,
                self.diff.scope,
                self.comments.len(),
                self.file_comments.len(),
                self.diff.head_sha.as_deref().unwrap_or("commit unknown; posting disabled")
            )),
            meta,
        );
        if let Some(merge) = &self.merge {
            merge.draw(frame, body);
        } else if let Some(request) = &self.pending {
            let mut preview = vec![
                Line::from(format!(
                    "Confirm {:?} on {} as your own forge account",
                    request.action, self.pull.url
                )),
                Line::from(format!("Commit: {}", request.head_sha)),
                Line::from(""),
                Line::from(request.body.clone()),
            ];
            for comment in &request.file_comments {
                preview.push(Line::from(format!(
                    "{}:{} {:?}: {}",
                    comment.path, comment.line, comment.side, comment.body
                )));
            }
            for comment in &request.file_level_comments {
                preview.push(Line::from(format!("{} (file): {}", comment.path, comment.body)));
            }
            preview.push(Line::from(""));
            preview.push(Line::from(
                "j/k/PgUp/PgDn: scroll · Enter/y: post this exact review · Esc: return to draft",
            ));
            frame.render_widget(
                Paragraph::new(preview)
                    .wrap(Wrap { trim: false })
                    .scroll((self.details_scroll.min(usize::from(u16::MAX)) as u16, 0)),
                body,
            );
        } else if self.show_details {
            self.draw_details(frame, body);
        } else {
            let scroll = self.selected.saturating_sub(usize::from(body.height).saturating_sub(2));
            let lines: Vec<_> = self
                .lines
                .iter()
                .enumerate()
                .skip(scroll)
                .take(usize::from(body.height))
                .filter_map(|(index, line)| {
                    let text = self.diff.patch.get(line.text.clone())?;
                    let mut style = Style::new();
                    if text.starts_with('+') {
                        style = style.fg(palette().looks_good);
                    }
                    if text.starts_with('-') {
                        style = style.fg(palette().delete);
                    }
                    if index == self.selected {
                        style = style.add_modifier(Modifier::REVERSED);
                    }
                    Some(Line::from(Span::styled(text, style)))
                })
                .collect();
            frame.render_widget(Paragraph::new(lines), body);
        }
        frame.render_widget(
            Paragraph::new(error.or(self.status.as_deref()).unwrap_or("")).wrap(Wrap { trim: false }),
            status,
        );
        frame.render_widget(Paragraph::new("j/k/PgUp/PgDn scroll · v details · s layer/stack · r refresh · c inline · f file · b body\np post comment · A approve · X request changes · u undo · D discard · Esc back\nM confirm merge · R risk rankings/rules/model · H link owner thread · g guide-to-forge"), help);
        if let Some((editing, compose)) = &self.editor {
            draw_compose(
                frame,
                area,
                compose,
                &format!("{editing:?} · Enter save draft · Alt+Enter newline · Esc cancel"),
            );
        }
    }

    fn draw_details(&self, frame: &mut Frame, area: Rect) {
        let Some(details) = &self.details else {
            return;
        };
        let mut lines = vec![
            Line::from(self.pull.url.clone()),
            Line::from(details.description.clone().unwrap_or_default()),
            Line::from(format!("Labels: {}", details.labels.join(", "))),
        ];
        for reviewer in &details.reviewers {
            lines.push(Line::from(format!("Reviewer {}: {}", reviewer.login, reviewer.state)));
        }
        for check in &details.checks {
            lines.push(Line::from(format!(
                "Check {}: {} {}",
                check.name,
                check.state,
                check.note.as_deref().unwrap_or("")
            )));
        }
        if details.threads_incomplete {
            lines.push(Line::styled(
                "Some code discussions could not be read. Refresh to ask again.",
                Style::new().fg(palette().delete),
            ));
        }
        for comment in &details.comments {
            lines.push(Line::from(format!(
                "{} · {} {}{}",
                comment.author,
                comment.at,
                comment.file.as_deref().unwrap_or(""),
                comment.line.map_or_else(String::new, |n| format!(":{n}"))
            )));
            lines.push(Line::from(comment.body.clone()));
            if let Some(reason) = &comment.hidden {
                lines.push(Line::from(format!("  Hidden by the forge: {reason}")));
            }
        }
        for thread in &details.threads {
            lines.push(Line::from(format!(
                "Discussion {} · {}{} · {}{}",
                thread.id,
                thread.file,
                thread.line.map_or_else(String::new, |line| format!(":{line}")),
                if thread.resolved { "resolved" } else { "open" },
                if thread.outdated { " · outdated" } else { "" }
            )));
        }
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((self.details_scroll.min(usize::from(u16::MAX)) as u16, 0)),
            area,
        );
    }
}

pub(super) fn draw_compose(frame: &mut Frame, area: Rect, compose: &Compose, title: &str) {
    let height = area.height.min(12);
    let area = Rect { y: area.y + area.height.saturating_sub(height), height, ..area };
    frame.render_widget(Clear, area);
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let (lines, row, column) = compose.wrapped(usize::from(inner.width.max(1)));
    let scroll = row.saturating_sub(usize::from(inner.height).saturating_sub(1));
    let lines: Vec<_> = lines.into_iter().skip(scroll).map(Line::from).collect();
    frame.render_widget(Paragraph::new(lines), inner);
    if inner.width > 0 && inner.height > 0 {
        frame.set_cursor_position((
            inner.x + column.min(usize::from(inner.width - 1)) as u16,
            inner.y + row.saturating_sub(scroll).min(usize::from(inner.height - 1)) as u16,
        ));
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing, reason = "tests assert by panicking")]
mod tests {
    use super::*;

    #[test]
    fn patch_line_positions_follow_old_and_new_sides() {
        let lines = patch_lines(
            "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -8,2 +8,3 @@\n old\n-gone\n+new\n+extra\n\\ No newline at end of file",
        );
        assert_eq!(lines[4].line, Some((8, DiffSide::Right)));
        assert_eq!(lines[5].line, Some((9, DiffSide::Left)));
        assert_eq!(lines[6].line, Some((9, DiffSide::Right)));
        assert_eq!(lines[7].line, Some((10, DiffSide::Right)));
        assert_eq!(lines[8].line, None);
        assert_eq!(lines[5].file.as_deref(), Some("src/a.rs"));
    }

    #[test]
    fn code_that_resembles_file_headers_keeps_the_hunk_location() {
        let lines = patch_lines(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n--- deleted code\n+++ added code",
        );
        assert_eq!(lines[4].line, Some((1, DiffSide::Left)));
        assert_eq!(lines[5].line, Some((1, DiffSide::Right)));
        assert_eq!(lines[5].file.as_deref(), Some("a"));
    }

    #[test]
    fn patch_storage_references_original_unicode_text_without_copying_it() {
        let patch = "diff --git a/a b/a\r\n--- a/a\r\n+++ b/a\r\n@@ -1 +1 @@\r\n+héllo\r\n";
        let lines = patch_lines(patch);
        assert_eq!(patch.get(lines[4].text.clone()), Some("+héllo"));
        assert_eq!(lines[4].line, Some((1, DiffSide::Right)));
    }

    #[test]
    fn deleted_files_retain_the_old_path_and_unknown_headers_do_not_invent_anchors() {
        let deleted =
            patch_lines("diff --git a/old b/old\n--- a/old\n+++ /dev/null\n@@ -1 +0,0 @@\n-removed");
        assert_eq!(deleted[4].file.as_deref(), Some("old"));
        assert_eq!(deleted[4].line, Some((1, DiffSide::Left)));
        let quoted =
            patch_lines("diff --git a/x b/x\n--- \"a/\\303\\244\"\n+++ \"b/\\303\\244\"\n@@ -1 +1 @@\n+x");
        assert_eq!(quoted[4].file, None);
    }
}

#[cfg(test)]
#[path = "pane_tests.rs"]
mod behavior_tests;
