//! Guided-review comments → forge review, never a working-tree or line-number guess.
//!
//! `run` selects a PR linked to an exact guide; `run_for_pull` selects that PR's linked
//! guide. Both GET the saved snapshot and annotations and the PR's **layer** diff. A
//! line goes inline only when its exact path, side, text, two surrounding lines and
//! hunk function context agree (`captureAnchor`/`anchorStillMatches` upstream).
//! Unverified coordinates remain explicitly labelled, quoted review-body comments.
//! Global comments stay global; GitHub file comments stay file-level when verified;
//! GitLab file comments ride the body. Suggestions retain their replacement text.
//!
//! Enter previews, y explicitly posts. A changed head/guide/comment cancels that
//! confirmation. Partial responses retain only the server-authorized retry; an
//! ambiguous result blocks further writes. In-process receipts survive closing the
//! pane and prevent accepted words from being transferred twice. Original guide
//! annotations are never deleted or rewritten by a forge transfer.

use super::client::{Client, HttpError};
use super::pulls::{
    ApprovalOutcome, DiffScope, DiffSide, FileComment, FileLevelComment, Platform, Pull, PullDiff,
    PullOptions, ReviewAction, ReviewPartial, ReviewPost, ReviewSubmission,
};
use super::reviews::diff::{self, File};
use crate::app::compose::{Compose, ComposeAction};
use anyhow::{Context, Result, bail, ensure};
use ratatui::{
    DefaultTerminal,
    crossterm::event::{self, Event, KeyCode, KeyEventKind},
    layout::{Constraint, Layout},
    text::Line,
    widgets::{Block, Borders, Paragraph, Wrap},
};
use serde_json::Value;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;

const MAX_ANCHOR_CHARS: usize = 20_000;

#[derive(Debug, Clone)]
struct Guide {
    title: String,
    patch: String,
    source: Value,
    annotations: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Anchor {
    text: String,
    before: [Option<String>; 2],
    after: [Option<String>; 2],
    hunk: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Placement {
    Inline(usize),
    File,
    Body,
}

#[derive(Debug, Clone)]
struct Binding {
    signature: String,
    placement: Placement,
}

#[derive(Debug, Clone)]
struct Plan {
    request: ReviewPost,
    bindings: Vec<Binding>,
    warnings: Vec<String>,
    // Retained inputs are the authority when rechecking a confirmation, not a hash.
    source_patch: String,
    source_annotations: Vec<Value>,
    pull_patch: String,
    pull_url: String,
}

#[derive(Debug, Default)]
struct Receipt {
    posted: BTreeSet<String>,
    pending: Option<Plan>,
    blocked: Option<String>,
    note: String,
    action: Option<ReviewAction>,
    partial: bool,
    last_result: String,
}

thread_local! {
    static RECEIPTS: RefCell<HashMap<String, Receipt>> = RefCell::new(HashMap::new());
}

/// Called from the guide's p action; only PRs linked to this thread are offered.
pub(crate) fn run(
    client: &Client,
    terminal: &mut DefaultTerminal,
    thread_id: &str,
    message_id: &str,
) -> Result<()> {
    let guide = load_guide(client, thread_id, message_id)?;
    let pulls = client.pulls(PullOptions::default())?;
    let candidates: Vec<&Pull> = pulls
        .pulls
        .iter()
        .filter(|pull| {
            pull.postable() && pulls.links.get(&pull.key).is_some_and(|link| link.thread_id == thread_id)
        })
        .collect();
    ensure!(!candidates.is_empty(), "No open PR with a known head is linked to this guide's thread");
    let labels: Vec<String> = candidates
        .iter()
        .map(|pull| format!("{} #{} — {} ({})", pull.repo, pull.number, pull.title, pull.platform.label()))
        .collect();
    let Some(index) = choose(terminal, &format!("Post guide: {}", guide.title), &labels)? else {
        return Ok(());
    };
    let pull = candidates.get(index).context("Selected pull is no longer available")?;
    run_target(client, terminal, thread_id, message_id, &pull.key)
}

/// Called from the PR pane's g action; the server link supplies the exact guide ID.
pub(crate) fn run_for_pull(client: &Client, terminal: &mut DefaultTerminal, pull_key: &str) -> Result<()> {
    let pulls = client.pulls(PullOptions::default())?;
    let link = pulls.links.get(pull_key).context("This PR has no linked agent thread")?;
    let guide = link.guide.as_ref().context("The linked thread has no guided review")?;
    run_target(client, terminal, &link.thread_id, &guide.message_id, pull_key)
}

fn segment(text: &str) -> String {
    url::form_urlencoded::byte_serialize(text.as_bytes()).collect()
}
fn text<'a>(value: &'a Value, field: &str) -> &'a str {
    value.get(field).and_then(Value::as_str).unwrap_or_default()
}
fn flag(value: &Value, field: &str) -> bool {
    value.get(field).and_then(Value::as_bool) == Some(true)
}

fn check_session(client: &Client, value: &Value) -> Result<()> {
    ensure!(
        value.get("serverSession").and_then(Value::as_str) == Some(client.server_session()),
        "Inbox server changed; reopen the review"
    );
    Ok(())
}

fn load_guide(client: &Client, thread_id: &str, message_id: &str) -> Result<Guide> {
    let thread_answer: Value =
        client.get_json("GET", &format!("/api/inbox/threads/{}", segment(thread_id)))?;
    check_session(client, &thread_answer)?;
    let thread = thread_answer.get("thread").context("Inbox omitted guide thread")?;
    let message = thread
        .get("messages")
        .and_then(Value::as_array)
        .context("Inbox omitted thread messages")?
        .iter()
        .find(|message| text(message, "id") == message_id)
        .context("Guide is not in the linked thread")?;
    ensure!(message.get("guide").is_some_and(Value::is_object), "Message is not a saved guide");
    let archived = thread.get("resolved_at").is_some_and(|value| !value.is_null())
        && thread.get("archive_seq").and_then(Value::as_u64).is_none_or(|seq| {
            message.get("seq").and_then(Value::as_u64).is_none_or(|message_seq| message_seq <= seq)
        });
    ensure!(!archived, "Archived guide is read-only; unarchive the thread first");
    let answer: Value =
        client.get_json("GET", &format!("/api/inbox/messages/{}/guide", segment(message_id)))?;
    let snapshot = answer.get("snapshot").context("Inbox omitted saved guide snapshot")?;
    ensure!(text(snapshot, "kind") == "plannotator-guided-review", "Not a guided-review snapshot");
    ensure!(
        snapshot.get("version").and_then(Value::as_u64).is_some_and(|version| (1..=5).contains(&version)),
        "Unsupported guide snapshot version"
    );
    let patch = snapshot
        .pointer("/review/rawPatch")
        .and_then(Value::as_str)
        .context("Guide omitted its exact patch")?
        .to_owned();
    let attachments: Value =
        client.get_json("GET", &format!("/api/inbox/threads/{}/attachments", segment(thread_id)))?;
    check_session(client, &attachments)?;
    let mut annotations = Vec::new();
    let mut ids = BTreeSet::new();
    for record in
        attachments.get("annotations").and_then(Value::as_array).context("Inbox omitted guide annotations")?
    {
        if text(record, "message_id") != message_id || !record.get("guide").is_some_and(Value::is_object) {
            continue;
        }
        let annotation = record
            .get("annotation")
            .filter(|value| value.is_object())
            .context("Malformed guide annotation")?;
        let id = text(annotation, "id");
        ensure!(!id.is_empty() && ids.insert(id.to_owned()), "Guide has missing or duplicate annotation IDs");
        annotations.push(annotation.clone());
    }
    Ok(Guide {
        title: snapshot.pointer("/guide/title").and_then(Value::as_str).unwrap_or("Guided review").to_owned(),
        patch,
        source: snapshot.get("source").cloned().unwrap_or(Value::Null),
        annotations,
    })
}

fn read_target(client: &Client, thread: &str, message: &str, key: &str) -> Result<(Guide, Pull, PullDiff)> {
    let guide = load_guide(client, thread, message)?;
    let pulls = client.pulls(PullOptions::default())?;
    ensure!(
        pulls.links.get(key).is_some_and(|link| link.thread_id == thread),
        "This PR is no longer linked to the guide's thread"
    );
    let pull = pulls.pulls.into_iter().find(|pull| pull.key == key).context("Linked PR is unavailable")?;
    let diff = client.pull_diff(key, DiffScope::Layer)?;
    validate_target(&guide, &pull, &diff)?;
    Ok((guide, pull, diff))
}

fn validate_target(guide: &Guide, pull: &Pull, diff: &PullDiff) -> Result<()> {
    ensure!(pull.postable(), "PR is closed, merged, or its head is unknown");
    ensure!(
        diff.key == pull.key && diff.scope == DiffScope::Layer,
        "Server returned a different PR or whole-stack diff"
    );
    let head = pull.head_sha.as_deref().context("PR head is unknown")?;
    ensure!(
        !head.is_empty() && diff.head_sha.as_deref() == Some(head),
        "PR head changed while reading its diff; read it again"
    );
    if let Some(url) = guide.source.pointer("/pr/url").and_then(Value::as_str) {
        ensure!(url == pull.url, "This guide explicitly names a different PR; nothing can be transferred");
    }
    Ok(())
}

fn unique_files(patch: &str) -> Result<Vec<File>> {
    let files = diff::parse(patch)?;
    let mut paths = BTreeSet::new();
    for file in &files {
        ensure!(
            !file.path.is_empty() && file.path != "/dev/null" && paths.insert(&file.path),
            "Patch has missing or ambiguous file paths"
        );
        let mut old = BTreeSet::new();
        let mut new = BTreeSet::new();
        for row in &file.rows {
            if let Some(line) = row.old {
                ensure!(line > 0 && old.insert(line), "Patch has overlapping old-side hunks");
            }
            if let Some(line) = row.new {
                ensure!(line > 0 && new.insert(line), "Patch has overlapping new-side hunks");
            }
        }
    }
    Ok(files)
}

fn line_range(annotation: &Value) -> Option<(usize, usize, DiffSide)> {
    let start = usize::try_from(annotation.get("lineStart")?.as_u64()?).ok()?;
    let end = usize::try_from(annotation.get("lineEnd")?.as_u64()?).ok()?;
    let side = match text(annotation, "side") {
        "old" => DiffSide::Left,
        "new" => DiffSide::Right,
        _ => return None,
    };
    (start > 0 && end >= start && end - start <= MAX_ANCHOR_CHARS).then_some((start, end, side))
}

/// A null context line differs from an empty one. Function text is deliberately not trimmed.
fn capture_anchor(file: &File, annotation: &Value) -> Option<Anchor> {
    let (start, end, side) = line_range(annotation)?;
    let mut lines = BTreeMap::new();
    let mut headers = BTreeMap::new();
    for row in &file.rows {
        if row.kind == '@' {
            let (_, context) = row.text.get(3..)?.split_once(" @@")?;
            headers.insert(row.hunk, context.strip_prefix(' ').unwrap_or(context));
        }
        let number = if side == DiffSide::Left { row.old } else { row.new };
        if let Some(number) = number {
            lines.insert(number, row);
        }
    }
    let first = lines.get(&start)?;
    let mut selected = String::new();
    for number in start..=end {
        let row = lines.get(&number)?;
        if row.hunk != first.hunk {
            return None;
        }
        if number != start {
            selected.push('\n');
        }
        selected.push_str(&row.text);
        if selected.len() > MAX_ANCHOR_CHARS * 4 {
            return None;
        }
    }
    if selected.encode_utf16().count() > MAX_ANCHOR_CHARS {
        return None;
    }
    let at = |number: Option<usize>| number.and_then(|number| lines.get(&number)).map(|row| row.text.clone());
    Some(Anchor {
        text: selected,
        before: [at(start.checked_sub(2)), at(start.checked_sub(1))],
        after: [at(end.checked_add(1)), at(end.checked_add(2))],
        hunk: headers.get(&first.hunk)?.to_string(),
    })
}

fn signature(annotation: &Value) -> String {
    let fields = [
        "id",
        "text",
        "suggestedCode",
        "conventionalLabel",
        "decorations",
        "filePath",
        "lineStart",
        "lineEnd",
        "side",
        "scope",
        "markdownPreview",
        "callFlowTargets",
    ];
    Value::Array(fields.iter().map(|field| annotation.get(field).cloned().unwrap_or(Value::Null)).collect())
        .to_string()
}

fn fence(code: &str, language: &str) -> String {
    let ticks = "`".repeat(code.split(|c| c != '`').map(str::len).max().unwrap_or(0).max(2) + 1);
    format!("{ticks}{language}\n{code}\n{ticks}")
}

fn comment_words(annotation: &Value) -> String {
    let mut words = String::new();
    let label = text(annotation, "conventionalLabel");
    if !label.is_empty() {
        let decorations: Vec<&str> = annotation
            .get("decorations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if decorations.is_empty() {
            let _ = write!(words, "**{label}:** ");
        } else {
            let _ = write!(words, "**{label} ({}):** ", decorations.join(", "));
        }
    }
    words.push_str(text(annotation, "text"));
    if let Some(targets) =
        annotation.get("callFlowTargets").and_then(Value::as_array).filter(|targets| !targets.is_empty())
    {
        words.push_str("\n\n**Selected call-flow steps:**");
        for target in targets {
            let path = text(target, "filePath");
            let source = if !path.is_empty() {
                match (
                    target.get("lineStart").and_then(Value::as_u64),
                    target.get("lineEnd").and_then(Value::as_u64),
                ) {
                    (Some(start), Some(end)) if start == end => format!("{path}:L{start}"),
                    (Some(start), Some(end)) => format!("{path}:L{start}-L{end}"),
                    _ => path.to_owned(),
                }
            } else if let Some(line) = target.get("rawLine").and_then(Value::as_u64) {
                format!("raw CallDiff line {line}")
            } else {
                "inferred step".into()
            };
            let _ = write!(words, "\n- {} → {} — {}", text(target, "entry"), text(target, "label"), source);
        }
    }
    words.trim().to_owned()
}

fn suggestion(annotation: &Value) -> Option<&str> {
    annotation.get("suggestedCode").and_then(Value::as_str)
}

fn fallback_body(annotation: &Value, anchor: Option<&Anchor>, reason: &str) -> String {
    let path = text(annotation, "filePath");
    let scope = text(annotation, "scope");
    let location = if scope == "general" {
        "Global comment".into()
    } else if scope == "file" {
        format!("{path} (file)")
    } else {
        format!(
            "{path} (L{}-L{}, {} side)",
            annotation.get("lineStart").unwrap_or(&Value::Null),
            annotation.get("lineEnd").unwrap_or(&Value::Null),
            text(annotation, "side")
        )
    };
    let mut body = format!("**{location}:** {}", comment_words(annotation));
    if !reason.is_empty() {
        let _ = write!(body, "\n\n_{reason}_");
    }
    if let Some(preview) = annotation.get("markdownPreview") {
        body.push_str("\n\nRendered Markdown selection:\n");
        body.push_str(&fence(text(preview, "originalText"), ""));
        if !text(preview, "diffContext").is_empty() {
            let _ = write!(body, "\n({} text)", text(preview, "diffContext"));
        }
    }
    let code = anchor
        .map(|anchor| anchor.text.as_str())
        .or_else(|| annotation.get("anchorText").and_then(Value::as_str))
        .or_else(|| annotation.get("originalCode").and_then(Value::as_str))
        .or_else(|| annotation.get("selectedText").and_then(Value::as_str));
    if scope != "general"
        && scope != "file"
        && let Some(code) = code
    {
        let _ = write!(body, "\n\nCommented on:\n{}", fence(code, ""));
    }
    if let Some(code) = suggestion(annotation) {
        let _ = write!(body, "\n\nSuggested code (not an inline replacement):\n{}", fence(code, ""));
    }
    body
}

fn prepare(
    guide: &Guide,
    pull: &Pull,
    diff: &PullDiff,
    action: ReviewAction,
    note: &str,
    posted: &BTreeSet<String>,
) -> Result<Plan> {
    validate_target(guide, pull, diff)?;
    if let Some(reason) = pull.action_refusal(action) {
        bail!("{reason}");
    }
    let guide_files = unique_files(&guide.patch)?;
    let pull_files = unique_files(&diff.patch)?;
    let mut request = ReviewPost {
        key: pull.key.clone(),
        head_sha: diff.head_sha.clone().context("Missing PR head")?,
        action,
        body: String::new(),
        file_comments: Vec::new(),
        file_level_comments: Vec::new(),
    };
    let mut bodies = Vec::new();
    if !note.trim().is_empty() {
        bodies.push(note.to_owned());
    }
    let mut bindings = Vec::new();
    let mut warnings = Vec::new();
    for annotation in &guide.annotations {
        let signature = signature(annotation);
        if posted.contains(&signature) {
            continue;
        }
        let scope = annotation.get("scope").and_then(Value::as_str).unwrap_or("line");
        ensure!(matches!(scope, "general" | "file" | "line"), "Unknown annotation scope; nothing was sent");
        let words = comment_words(annotation);
        ensure!(
            !words.is_empty() || suggestion(annotation).is_some(),
            "Empty guide annotation cannot be transferred"
        );
        let path = text(annotation, "filePath");
        let from = guide_files.iter().find(|file| file.path == path);
        let to = pull_files.iter().find(|file| file.path == path);
        let captured = from.and_then(|file| capture_anchor(file, annotation));
        let target = to.and_then(|file| capture_anchor(file, annotation));
        let placement = if scope == "general" {
            if suggestion(annotation).is_some() {
                bodies.push(fallback_body(annotation, None, ""));
            } else {
                bodies.push(words);
            }
            Placement::Body
        } else if scope == "file"
            && from.is_some()
            && to.is_some()
            && pull.platform == Platform::Github
            && annotation.get("markdownPreview").is_none_or(Value::is_null)
            && suggestion(annotation).is_none()
        {
            request.file_level_comments.push(FileLevelComment { path: path.into(), body: words });
            Placement::File
        } else if scope == "line"
            && captured.is_some()
            && captured == target
            && !flag(annotation, "outdated")
            && !flag(annotation, "outsideDiff")
            && annotation.get("markdownPreview").is_none_or(Value::is_null)
        {
            let (start, end, side) = line_range(annotation).context("Invalid verified annotation range")?;
            let mut body = words;
            if let Some(code) = suggestion(annotation) {
                let _ = write!(body, "\n\n{}", fence(code, "suggestion"));
            }
            let index = request.file_comments.len();
            request.file_comments.push(FileComment {
                path: path.into(),
                line: end as u64,
                side,
                body: body.trim().into(),
                start_line: (start != end).then_some(start as u64),
                start_side: (start != end).then_some(side),
            });
            Placement::Inline(index)
        } else {
            let reason =
                if scope == "file" && from.is_some() && to.is_some() && pull.platform == Platform::Gitlab {
                    "File-wide comment; GitLab posts this in the review body."
                } else if scope == "file" {
                    "File scope preserved in the review body; no verified file-level target."
                } else {
                    "Comments outside the verified PR diff: guide coordinates only, not an inline PR anchor."
                };
            warnings.push(format!("{path}: {reason}"));
            bodies.push(fallback_body(annotation, captured.as_ref(), reason));
            Placement::Body
        };
        bindings.push(Binding { signature, placement });
    }
    ensure!(
        request.file_level_comments.len() <= 500,
        "At most 500 file-level comments can be posted at once"
    );
    request.body = bodies.join("\n\n");
    if request.body.is_empty()
        && action != ReviewAction::Approve
        && pull.platform == Platform::Github
        && (!request.file_comments.is_empty() || !request.file_level_comments.is_empty())
    {
        request.body = "See inline comments.".into();
    }
    ensure!(
        request.body.encode_utf16().count() <= 65_536,
        "Review body exceeds the forge limit; shorten guide comments before posting (nothing was truncated)"
    );
    Ok(Plan {
        request,
        bindings,
        warnings,
        source_patch: guide.patch.clone(),
        source_annotations: guide.annotations.clone(),
        pull_patch: diff.patch.clone(),
        pull_url: pull.url.clone(),
    })
}

/// Only a literal subset of this attempt may be retried; counts must account for all inline comments.
fn partial_retry(plan: &Plan, partial: &ReviewPartial) -> Result<(Plan, Vec<String>)> {
    ensure!(
        plan.request.file_level_comments.is_empty(),
        "Partial file-level result cannot be reconciled safely; inspect the PR"
    );
    let mut unclaimed = vec![true; plan.request.file_comments.len()];
    let mut retry_indices = Vec::new();
    for comment in &partial.retry.file_comments {
        let index = plan
            .request
            .file_comments
            .iter()
            .enumerate()
            .position(|(index, original)| unclaimed.get(index) == Some(&true) && original == comment)
            .context("Server retry contains an unknown or duplicate inline comment")?;
        if let Some(slot) = unclaimed.get_mut(index) {
            *slot = false;
        }
        retry_indices.push(index);
    }
    ensure!(
        partial.posted_file_comment_count == unclaimed.iter().filter(|value| **value).count(),
        "Partial inline count does not reconcile; inspect the PR before retrying"
    );
    ensure!(
        !(partial.review_body_posted && partial.retry.body.is_some()),
        "Partial response both posted and retries the review body"
    );
    if let Some(body) = &partial.retry.body {
        ensure!(body == &plan.request.body, "Server retry changed the review body");
    }
    ensure!(
        plan.request.body.is_empty() || partial.review_body_posted || partial.retry.body.is_some(),
        "Partial response omitted the unsent review body"
    );
    ensure!(
        partial.retry.action == plan.request.action || partial.retry.action == ReviewAction::Comment,
        "Partial retry escalated the review verdict"
    );
    ensure!(
        partial.retry.action != ReviewAction::Approve || partial.approval == ApprovalOutcome::Failed,
        "Partial retry would duplicate approval"
    );
    ensure!(
        partial
            .failed_file_comments
            .iter()
            .all(|failure| partial.retry.file_comments.contains(&failure.comment)),
        "Failed comment is absent from the authorized retry"
    );
    let mut went = Vec::new();
    let mut bindings = Vec::new();
    for binding in &plan.bindings {
        match binding.placement {
            Placement::Inline(index) => {
                if let Some(retry_index) = retry_indices.iter().position(|original| *original == index) {
                    bindings.push(Binding {
                        signature: binding.signature.clone(),
                        placement: Placement::Inline(retry_index),
                    });
                } else {
                    went.push(binding.signature.clone());
                }
            }
            Placement::Body if partial.review_body_posted && partial.retry.body.is_none() => {
                went.push(binding.signature.clone());
            }
            _ => bindings.push(binding.clone()),
        }
    }
    let mut retry = plan.clone();
    retry.request = plan.request.retry(partial);
    retry.bindings = bindings;
    Ok((retry, went))
}

fn same_inputs(plan: &Plan, guide: &Guide, pull: &Pull, diff: &PullDiff) -> bool {
    plan.source_patch == guide.patch
        && plan.source_annotations == guide.annotations
        && plan.pull_patch == diff.patch
        && plan.pull_url == pull.url
        && diff.head_sha.as_deref() == Some(plan.request.head_sha.as_str())
}

fn choose(terminal: &mut DefaultTerminal, title: &str, choices: &[String]) -> Result<Option<usize>> {
    let mut selected = 0usize;
    loop {
        terminal.draw(|frame| {
            let rows: Vec<Line<'_>> = choices
                .iter()
                .enumerate()
                .map(|(index, label)| {
                    Line::from(format!("{} {label}", if index == selected { ">" } else { " " }))
                })
                .collect();
            frame.render_widget(
                Paragraph::new(rows).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!("{title} — ↑/↓ choose · Enter continue · Esc cancel")),
                ),
                frame.area(),
            );
        })?;
        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Release {
                continue;
            }
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => return Ok(None),
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = selected.saturating_add(1).min(choices.len().saturating_sub(1));
                }
                KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
                KeyCode::Enter => return Ok(Some(selected)),
                _ => {}
            }
        }
    }
}

fn run_target(
    client: &Client,
    terminal: &mut DefaultTerminal,
    thread: &str,
    message: &str,
    key: &str,
) -> Result<()> {
    let cache_key =
        serde_json::json!([client.url(), client.server_session(), thread, message, key]).to_string();
    let mut receipt = RECEIPTS.with(|cache| cache.borrow_mut().remove(&cache_key)).unwrap_or_default();
    let result = run_dialog(client, terminal, thread, message, key, &mut receipt);
    RECEIPTS.with(|cache| {
        cache.borrow_mut().insert(cache_key, receipt);
    });
    result
}

fn preview(plan: &Plan, title: &str) -> String {
    let request = &plan.request;
    let mut output = format!(
        "Guide: {title}\nTarget: {}\nHead: {}\nAction: {:?}\n{} inline · {} file-level · {} body comments\n\n",
        plan.pull_url,
        request.head_sha,
        request.action,
        request.file_comments.len(),
        request.file_level_comments.len(),
        plan.bindings.iter().filter(|binding| binding.placement == Placement::Body).count()
    );
    for warning in &plan.warnings {
        let _ = writeln!(output, "NOTICE: {warning}");
    }
    if !request.body.is_empty() {
        let _ = writeln!(output, "\nREVIEW BODY\n{}", request.body);
    }
    for comment in &request.file_level_comments {
        let _ = writeln!(output, "\nFILE {}\n{}", comment.path, comment.body);
    }
    for comment in &request.file_comments {
        let _ = writeln!(
            output,
            "\nINLINE {} L{}–L{} {:?}\n{}",
            comment.path,
            comment.start_line.unwrap_or(comment.line),
            comment.line,
            comment.side,
            comment.body
        );
    }
    output
}

fn validate_post(
    client: &Client,
    thread: &str,
    message: &str,
    key: &str,
    plan: &Plan,
    receipt: &mut Receipt,
) -> Result<()> {
    let (guide, pull, diff) = read_target(client, thread, message, key)
        .map_err(|error| anyhow::anyhow!("Nothing sent: {error}"))?;
    if !same_inputs(plan, &guide, &pull, &diff) {
        let reason = if receipt.partial {
            "Guide comments or PR diff changed after a partial post. Retry blocked; inspect the PR before continuing."
        } else {
            "Guide comments or PR diff changed. Nothing was sent; R reloads and requires a new confirmation."
        };
        if receipt.partial {
            receipt.blocked = Some(reason.to_owned());
        }
        bail!(reason);
    }
    if let Some(reason) = pull.action_refusal(plan.request.action) {
        bail!(reason);
    }
    Ok(())
}

fn reload_plan(
    client: &Client,
    thread: &str,
    message: &str,
    key: &str,
    action: ReviewAction,
    receipt: &Receipt,
) -> Result<(Guide, Pull, PullDiff, Plan)> {
    let (guide, pull, diff) = read_target(client, thread, message, key)?;
    let plan = prepare(&guide, &pull, &diff, action, &receipt.note, &receipt.posted)?;
    Ok((guide, pull, diff, plan))
}

fn post_plan(
    client: &Client,
    plan: &mut Plan,
    action: &mut ReviewAction,
    receipt: &mut Receipt,
    status: &mut String,
) -> bool {
    receipt.pending = Some(plan.clone());
    match client.post_pull_review(&plan.request) {
        Ok(answer) if answer.ok && answer.pr_url == plan.pull_url => match answer.submission {
            ReviewSubmission::Complete { posted_as } => {
                for binding in &plan.bindings {
                    receipt.posted.insert(binding.signature.clone());
                }
                receipt.pending = None;
                receipt.note.clear();
                receipt.partial = false;
                receipt.last_result = format!(
                    "Posted {:?} to {}. Original guide comments remain; accepted words will not be sent again.",
                    posted_as.unwrap_or(plan.request.action),
                    plan.pull_url
                );
                return true;
            }
            ReviewSubmission::Partial(partial) => match partial_retry(plan, &partial) {
                Ok((retry, went)) => {
                    receipt.posted.extend(went);
                    if partial.review_body_posted {
                        receipt.note.clear();
                    }
                    *action = retry.request.action;
                    receipt.action = Some(*action);
                    *plan = retry;
                    receipt.pending = Some(plan.clone());
                    receipt.partial = true;
                    *status = format!(
                        "PARTIAL: {} inline comments accepted. Only this server-authorized remainder may be retried. {} {}",
                        partial.posted_file_comment_count,
                        partial.approval_error.as_deref().unwrap_or(""),
                        partial.review_body_error.as_deref().unwrap_or("")
                    );
                    receipt.last_result.clone_from(status);
                }
                Err(error) => {
                    *status = error.to_string();
                    receipt.blocked = Some(status.clone());
                }
            },
        },
        Ok(_) => {
            *status =
                "Unrecognized forge response; outcome is unknown. Inspect the PR; replay is blocked.".into();
            receipt.blocked = Some(status.clone());
        }
        Err(error) => {
            if let Some(http) = error.downcast_ref::<HttpError>() {
                if http.status == 409 && http.code.as_deref() == Some("already_posted") {
                    for binding in &plan.bindings {
                        receipt.posted.insert(binding.signature.clone());
                    }
                    receipt.pending = None;
                    receipt.note.clear();
                    receipt.partial = false;
                    receipt.last_result = "The Inbox confirmed these exact words were already posted.".into();
                    return true;
                }
                *status = format!(
                    "Nothing confirmed: {error}. Read the error before retrying; a moved head requires R and a new decision."
                );
            } else {
                *status = format!(
                    "Unknown result: {error}. The forge may have accepted the review. Inspect the PR; replay is blocked."
                );
                receipt.blocked = Some(status.clone());
            }
        }
    }
    false
}

fn run_dialog(
    client: &Client,
    terminal: &mut DefaultTerminal,
    thread: &str,
    message: &str,
    key: &str,
    receipt: &mut Receipt,
) -> Result<()> {
    let (mut guide, mut pull, mut diff) = read_target(client, thread, message, key)?;
    let mut action = receipt.action.unwrap_or(ReviewAction::Comment);
    let mut plan = match &receipt.pending {
        Some(pending) => pending.clone(),
        None => prepare(&guide, &pull, &diff, action, &receipt.note, &receipt.posted)?,
    };
    let mut confirm = false;
    let mut note_editor: Option<Compose> = None;
    let mut scroll = 0u16;
    let mut status = receipt.blocked.clone().unwrap_or_else(|| receipt.last_result.clone());
    loop {
        let contents = if let Some(editor) = &note_editor {
            format!("Edit review note (Enter saves, Alt+Enter newline, Esc cancels):\n\n{}", editor.value())
        } else {
            preview(&plan, &guide.title)
        };
        terminal.draw(|frame| {
            let areas = Layout::vertical([Constraint::Min(3), Constraint::Length(3)]).split(frame.area());
            if let Some(area) = areas.first() { frame.render_widget(Paragraph::new(contents.as_str()).wrap(Wrap { trim: false }).scroll((scroll, 0)).block(Block::default().borders(Borders::ALL).title("Guide → forge (original guide comments are kept)")), *area); }
            let keys = if receipt.blocked.is_some() { "BLOCKED: inspect the PR; no retry is permitted · Esc close" }
                else if confirm { "y POST exactly this preview to the forge · Esc cancel confirmation · ↑/↓ scroll" }
                else if receipt.partial { "Enter preview exact narrowed retry · ↑/↓ scroll · Esc close (retry retained)" }
                else { "c Comment · a Approve · r Request changes · n Note · Enter preview · R reload · ↑/↓ scroll · Esc close" };
            if let Some(area) = areas.get(1) { frame.render_widget(Paragraph::new(format!("{keys}\n{status}")).wrap(Wrap { trim: false }), *area); }
        })?;
        let input = event::read()?;
        if let Event::Paste(words) = &input {
            if let Some(editor) = &mut note_editor {
                editor.insert_text(words);
            }
            continue;
        }
        let Event::Key(key_event) = input else { continue };
        if key_event.kind == KeyEventKind::Release {
            continue;
        }
        if let Some(editor) = &mut note_editor {
            match editor.handle_key(key_event) {
                ComposeAction::Save => {
                    receipt.note = editor.value();
                    note_editor = None;
                    plan = prepare(&guide, &pull, &diff, action, &receipt.note, &receipt.posted)?;
                }
                ComposeAction::Cancel => note_editor = None,
                ComposeAction::Edited => {}
            }
            continue;
        }
        match key_event.code {
            KeyCode::Esc if confirm => confirm = false,
            KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
            KeyCode::Down | KeyCode::Char('j') => scroll = scroll.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => scroll = scroll.saturating_sub(1),
            KeyCode::PageDown => scroll = scroll.saturating_add(15),
            KeyCode::PageUp => scroll = scroll.saturating_sub(15),
            _ if receipt.blocked.is_some() => {}
            KeyCode::Char('n') if !confirm && !receipt.partial => {
                note_editor = Some(Compose::with_text(&receipt.note));
                scroll = 0;
            }
            KeyCode::Char(choice @ ('a' | 'c' | 'r')) if !confirm && !receipt.partial => {
                let next = match choice {
                    'a' => ReviewAction::Approve,
                    'r' => ReviewAction::RequestChanges,
                    _ => ReviewAction::Comment,
                };
                match prepare(&guide, &pull, &diff, next, &receipt.note, &receipt.posted) {
                    Ok(next_plan) => {
                        action = next;
                        receipt.action = Some(next);
                        plan = next_plan;
                        status.clear();
                    }
                    Err(error) => status = error.to_string(),
                }
            }
            KeyCode::Char('R') if !confirm && !receipt.partial => {
                match reload_plan(client, thread, message, key, plan.request.action, receipt) {
                    Ok((next_guide, next_pull, next_diff, next)) => {
                        guide = next_guide;
                        pull = next_pull;
                        diff = next_diff;
                        action = next.request.action;
                        plan = next;
                        receipt.pending = None;
                        receipt.partial = false;
                        status = "Reloaded unsent words; read and confirm this new preview.".into();
                        scroll = 0;
                    }
                    Err(error) => status = error.to_string(),
                }
            }
            KeyCode::Enter if !confirm => {
                if plan.request.action == ReviewAction::Comment
                    && plan.request.body.trim().is_empty()
                    && plan.request.file_comments.is_empty()
                    && plan.request.file_level_comments.is_empty()
                {
                    status = "Nothing remains to post; add a note or choose a verdict.".into();
                } else {
                    confirm = true;
                    status = "This writes to the remote forge, not to your agent. Press y only after reading the preview.".into();
                }
            }
            KeyCode::Char('y') if confirm => {
                confirm = false;
                if let Err(error) = validate_post(client, thread, message, key, &plan, receipt) {
                    status = error.to_string();
                    continue;
                }
                if post_plan(client, &mut plan, &mut action, receipt, &mut status) {
                    return Ok(());
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing, reason = "tests assert exact transfer behavior")]
mod tests {
    use super::*;
    use serde_json::json;

    const PATCH: &str = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,5 +1,5 @@ fn first()\n before\n-old\n+new\n after\n more\n last\n";

    fn fixture() -> (Guide, Pull, PullDiff) {
        let guide = Guide {
            title: "Guide".into(),
            patch: PATCH.into(),
            source: Value::Null,
            annotations: vec![
                json!({"id":"a","scope":"line","filePath":"src/a.rs","lineStart":2,"lineEnd":2,"side":"new","text":"Check this"}),
            ],
        };
        let pull: Pull = serde_json::from_value(json!({"key":"github:example/repo:1","platform":"github","host":"github.com","repo":"example/repo","number":1,"url":"https://github.com/example/repo/pull/1","title":"PR","mine":false,"requested":false,"state":"open","base":"main","head":"topic","head_sha":"abc","additions":1,"deletions":1,"files":1,"reviewers":[],"updated_at":"now"})).expect("pull");
        let diff = PullDiff {
            key: pull.key.clone(),
            scope: DiffScope::Layer,
            base: "main".into(),
            head: "topic".into(),
            patch: PATCH.into(),
            head_sha: Some("abc".into()),
        };
        (guide, pull, diff)
    }

    fn plan(guide: &Guide, pull: &Pull, diff: &PullDiff) -> Plan {
        prepare(guide, pull, diff, ReviewAction::Comment, "", &BTreeSet::new()).expect("plan")
    }

    #[test]
    fn exact_anchor_posts_inline_with_neutral_github_body() {
        let (guide, pull, diff) = fixture();
        let plan = plan(&guide, &pull, &diff);
        assert_eq!(plan.request.file_comments.len(), 1);
        assert_eq!(plan.request.file_comments[0].line, 2);
        assert_eq!(plan.request.file_comments[0].side, DiffSide::Right);
        assert_eq!(plan.request.body, "See inline comments.");
        assert_eq!(plan.request.head_sha, "abc");
    }

    #[test]
    fn equal_line_text_without_context_or_function_match_is_body_only() {
        let (guide, pull, diff) = fixture();
        for patch in [
            PATCH.replace(" before", " changed"),
            PATCH.replace("fn first()", "fn second()"),
            PATCH.replace(" after", " other"),
        ] {
            let mut diff = diff.clone();
            diff.patch = patch;
            let plan = plan(&guide, &pull, &diff);
            assert!(plan.request.file_comments.is_empty());
            assert!(plan.request.body.contains("Commented on:\n```\nnew\n```"));
        }
    }

    #[test]
    fn exact_path_is_required_and_suffix_matching_is_forbidden() {
        let (guide, pull, mut diff) = fixture();
        diff.patch = PATCH.replace("src/a.rs", "other/a.rs");
        let plan = plan(&guide, &pull, &diff);
        assert!(plan.request.file_comments.is_empty());
        assert!(plan.request.body.contains("src/a.rs"));
    }

    #[test]
    fn old_side_and_multiline_suggestion_are_preserved() {
        let (mut guide, pull, diff) = fixture();
        guide.annotations[0]["side"] = json!("old");
        guide.annotations[0]["lineEnd"] = json!(3);
        guide.annotations[0]["suggestedCode"] = json!("replacement\nnext");
        let plan = plan(&guide, &pull, &diff);
        let comment = &plan.request.file_comments[0];
        assert_eq!(comment.side, DiffSide::Left);
        assert_eq!(comment.start_side, Some(DiffSide::Left));
        assert_eq!(comment.start_line, Some(2));
        assert_eq!(comment.line, 3);
        assert!(comment.body.contains("```suggestion\nreplacement\nnext\n```"));
    }

    #[test]
    fn deleting_suggestion_is_not_dropped() {
        let (mut guide, pull, diff) = fixture();
        guide.annotations[0]["text"] = json!("");
        guide.annotations[0]["suggestedCode"] = json!("");
        assert_eq!(plan(&guide, &pull, &diff).request.file_comments[0].body, "```suggestion\n\n```");
    }

    #[test]
    fn general_and_file_comments_retain_their_scopes() {
        let (mut guide, mut pull, diff) = fixture();
        guide.annotations = vec![
            json!({"id":"g","scope":"general","filePath":"","text":"Whole review"}),
            json!({"id":"f","scope":"file","filePath":"src/a.rs","text":"Whole file"}),
        ];
        let github = plan(&guide, &pull, &diff);
        assert_eq!(github.request.file_level_comments.len(), 1);
        assert_eq!(github.request.body, "Whole review");
        pull.platform = Platform::Gitlab;
        let gitlab = plan(&guide, &pull, &diff);
        assert!(gitlab.request.file_level_comments.is_empty());
        assert!(gitlab.request.body.contains("Whole review"));
        assert!(gitlab.request.body.contains("src/a.rs (file)"));
    }

    #[test]
    fn stale_outside_and_unavailable_ranges_are_never_inline() {
        let (guide, pull, diff) = fixture();
        for field in ["outdated", "outsideDiff"] {
            let mut guide = guide.clone();
            guide.annotations[0][field] = json!(true);
            assert!(plan(&guide, &pull, &diff).request.file_comments.is_empty());
        }
        let mut guide = guide;
        guide.annotations[0]["lineEnd"] = json!(90);
        assert!(plan(&guide, &pull, &diff).request.file_comments.is_empty());
    }

    #[test]
    fn moved_head_stack_scope_and_foreign_pr_fail_closed() {
        let (mut guide, pull, diff) = fixture();
        let mut moved = diff.clone();
        moved.head_sha = Some("def".into());
        assert!(validate_target(&guide, &pull, &moved).is_err());
        let mut stack = diff.clone();
        stack.scope = DiffScope::Stack;
        assert!(validate_target(&guide, &pull, &stack).is_err());
        guide.source = json!({"pr":{"url":"https://github.com/other/repo/pull/9"}});
        assert!(validate_target(&guide, &pull, &diff).is_err());
    }

    #[test]
    fn duplicate_paths_and_overlapping_hunks_fail_closed() {
        assert!(unique_files(&format!("{PATCH}{PATCH}")).is_err());
        assert!(unique_files(&format!("{PATCH}@@ -2 +2 @@\n-old\n+new\n")).is_err());
    }

    #[test]
    fn quoted_paths_are_exact_and_empty_context_is_not_missing_context() {
        let patch = "diff --git \"a/a\\040b.rs\" \"b/a\\040b.rs\"\n--- \"a/a\\040b.rs\"\n+++ \"b/a\\040b.rs\"\n@@ -1,2 +1,2 @@\n \n-x\n+y\n";
        let files = unique_files(patch).expect("quoted files");
        assert_eq!(files[0].path, "a b.rs");
        let anchor =
            capture_anchor(&files[0], &json!({"lineStart":2,"lineEnd":2,"side":"new"})).expect("anchor");
        assert_eq!(anchor.before, [None, Some(String::new())]);
        assert_eq!(anchor.after, [None, None]);
    }

    #[test]
    fn partial_retry_keeps_only_explicitly_unposted_words() {
        let (mut guide, pull, diff) = fixture();
        guide.annotations.push(json!({"id":"b","scope":"line","filePath":"src/a.rs","lineStart":3,"lineEnd":3,"side":"new","text":"Second"}));
        guide.annotations.push(json!({"id":"g","scope":"general","text":"Global"}));
        let initial = plan(&guide, &pull, &diff);
        let partial: ReviewPartial = serde_json::from_value(json!({"postedFileCommentCount":1,"failedFileComments":[],"reviewBodyPosted":true,"approval":"not-requested","retry":{"action":"comment","fileComments":[initial.request.file_comments[1]]}})).expect("partial");
        let (retry, went) = partial_retry(&initial, &partial).expect("retry");
        assert_eq!(retry.request.file_comments.len(), 1);
        assert_eq!(retry.request.file_comments[0].body, "Second");
        assert!(retry.request.body.is_empty());
        assert_eq!(went.len(), 2);
        assert_eq!(retry.request.head_sha, initial.request.head_sha);
        let remaining = prepare(&guide, &pull, &diff, ReviewAction::Comment, "", &went.into_iter().collect())
            .expect("remaining");
        assert_eq!(remaining.request.file_comments.len(), 1);
    }

    #[test]
    fn partial_unknown_comment_or_wrong_count_blocks_retry() {
        let (guide, pull, diff) = fixture();
        let initial = plan(&guide, &pull, &diff);
        let mut partial: ReviewPartial = serde_json::from_value(json!({"postedFileCommentCount":0,"failedFileComments":[],"reviewBodyPosted":true,"approval":"not-requested","retry":{"action":"comment","fileComments":[]}})).expect("partial");
        assert!(partial_retry(&initial, &partial).is_err());
        partial.retry.file_comments.push(initial.request.file_comments[0].clone());
        partial.retry.file_comments[0].path = "wrong.rs".into();
        assert!(partial_retry(&initial, &partial).is_err());
    }

    #[test]
    fn edits_are_new_words_and_invalidate_confirmation() {
        let (mut guide, pull, diff) = fixture();
        let initial = plan(&guide, &pull, &diff);
        let posted = BTreeSet::from([signature(&guide.annotations[0])]);
        assert!(
            prepare(&guide, &pull, &diff, ReviewAction::Comment, "", &posted)
                .expect("posted")
                .request
                .file_comments
                .is_empty()
        );
        guide.annotations[0]["text"] = json!("Edited words");
        assert!(!same_inputs(&initial, &guide, &pull, &diff));
        assert_eq!(
            prepare(&guide, &pull, &diff, ReviewAction::Comment, "", &posted)
                .expect("edited")
                .request
                .file_comments
                .len(),
            1
        );
    }

    #[test]
    fn oversized_anchors_stay_in_body_and_unknown_scopes_are_refused() {
        let (mut guide, pull, mut diff) = fixture();
        guide.patch = PATCH.replace("+new", &format!("+{}", "x".repeat(MAX_ANCHOR_CHARS + 1)));
        diff.patch.clone_from(&guide.patch);
        assert!(plan(&guide, &pull, &diff).request.file_comments.is_empty());
        guide.annotations[0]["scope"] = json!("future-scope");
        assert!(prepare(&guide, &pull, &diff, ReviewAction::Comment, "", &BTreeSet::new()).is_err());
    }
}
