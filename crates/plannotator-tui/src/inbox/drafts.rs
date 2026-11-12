//! Reply drafts use the Inbox's compare-and-set protocol, never a local shadow file.
//!
//! Integration: load before opening Reply, keep this state while editing, and save after
//! a 500 ms typing pause and before leaving the thread. `prepare_send` flushes the draft
//! and freezes it; put its fields on the words-carrying `actions::ReplyItems` only. Keep
//! that exact payload and idempotency key after an unanswered send. An HTTP success means
//! accepted by the Inbox, not delivered to an agent; a held outbox item is neither. Call
//! `after_send` only for the words-carrying result, or `release_send` after a definite
//! refusal. Refresh on draft events. Conflicts require Keep mine or Use saved, never an
//! automatic overwrite. `edit_draft` opens the save-only editor with recipients.
//! Ctrl-U reads one explicitly selected regular file (at most 100 MiB), previews it,
//! and requires `y` before uploading bytes to this Inbox. Only its basename travels;
//! no local path is kept in the draft. Ctrl-F previews/removes files. Pending uploads
//! have no general download API, so local text/hex previews last only in this state;
//! reopened drafts show server metadata. Removing a file does not send a message or
//! delete local bytes; the server sweeps unreferenced uploads after 24 hours.
//! Linked-machine uploads use the thread's pinned owner and one retained retry key.
//! Remote reply drafts stay in memory only: no draft endpoint exists in the machine
//! door. They survive refreshes but not quitting; remote pending uploads may expire
//! after 24 hours without Send. No local draft is ever written on their behalf.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read as _;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::actions::{ReplyResult, ReplyTag, TagMode};
use super::client::{Addressee, Client, HttpError, Thread, path_id};
use super::send;
use crate::app::compose::{Compose, ComposeAction};

pub(crate) const SAVE_DELAY: Duration = Duration::from_millis(500);
const BODY_MAX: usize = 1_000_000;
const RECIPIENTS_MAX: usize = 9;
const UPLOAD_MAX_BYTES: u64 = 100 * 1024 * 1024;
const UPLOAD_PREVIEW_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DraftPasted {
    /// JavaScript UTF-16 offset, not a UTF-8 byte or Rust char offset.
    pub(crate) at: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) lead: Option<String>,
    pub(crate) preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DraftFile {
    pub(crate) upload_id: String,
    pub(crate) name: String,
    pub(crate) size: u64,
    pub(crate) kind: String,
    pub(crate) sha256: String,
    pub(crate) created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) width: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) height: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) shown_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pasted: Option<DraftPasted>,
}

struct SelectedUpload {
    name: String,
    bytes: Vec<u8>,
    preview: String,
    key: String,
}

impl SelectedUpload {
    fn read(path: &Path) -> Result<Self> {
        let name =
            path.file_name().and_then(|name| name.to_str()).context("Choose a file with a UTF-8 name")?;
        validate_upload_name(name)?;
        let metadata = std::fs::symlink_metadata(path).with_context(|| format!("Checking {name}"))?;
        ensure!(metadata.is_file(), "Choose a regular file, not a directory, link or device");
        ensure!(metadata.len() <= UPLOAD_MAX_BYTES, "Not attached: a file is at most 100 MiB");
        // Check the resolved name too: an alias through a directory must not hide .env.
        let resolved = path.canonicalize().with_context(|| format!("Resolving {name}"))?;
        validate_upload_name(
            resolved.file_name().and_then(|name| name.to_str()).context("File has no UTF-8 name")?,
        )?;
        let file = File::open(&resolved).with_context(|| format!("Opening {name}"))?;
        let metadata = file.metadata().with_context(|| format!("Checking {name}"))?;
        ensure!(
            metadata.is_file() && metadata.len() <= UPLOAD_MAX_BYTES,
            "Choose a regular file no larger than 100 MiB"
        );
        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len())?);
        file.take(UPLOAD_MAX_BYTES + 1).read_to_end(&mut bytes).with_context(|| format!("Reading {name}"))?;
        ensure!(bytes.len() as u64 <= UPLOAD_MAX_BYTES, "Not attached: file grew beyond 100 MiB");
        let preview = upload_preview(&bytes);
        Ok(Self { name: name.to_owned(), bytes, preview, key: send::new_key() })
    }
}

fn validate_upload_name(name: &str) -> Result<()> {
    ensure!(
        !name.trim().is_empty() && name != "." && name != ".." && name.len() <= 255,
        "File name must be 1–255 UTF-8 bytes"
    );
    ensure!(!name.contains(['/', '\\']) && !name.chars().any(|ch| ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}' | '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')), "File name cannot contain path separators, controls or bidirectional controls");
    let basename = name.split(':').next().unwrap_or(name).trim_end_matches(['.', ' ']);
    let extension = basename.rsplit('.').next().unwrap_or_default();
    let doc = [
        "md",
        "mdx",
        "txt",
        "adoc",
        "asciidoc",
        "mmd",
        "mermaid",
        "dot",
        "gv",
        "htm",
        "html",
        "yaml",
        "yml",
        "json",
        "jsonc",
        "json5",
        "toml",
        "ini",
        "cfg",
        "conf",
        "properties",
        "csv",
        "tsv",
        "log",
        "xml",
    ]
    .iter()
    .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        || basename.eq_ignore_ascii_case(".env.example");
    let dotenv = basename.eq_ignore_ascii_case("env")
        || extension.eq_ignore_ascii_case("env")
        || basename.eq_ignore_ascii_case(".envrc")
        || (basename.get(..5).is_some_and(|prefix| prefix.eq_ignore_ascii_case(".env.")) && !doc);
    ensure!(!dotenv, "Not attached: .env files may contain secrets");
    Ok(())
}

fn upload_preview(bytes: &[u8]) -> String {
    let prefix = bytes.get(..bytes.len().min(UPLOAD_PREVIEW_BYTES)).unwrap_or_default();
    let text = match std::str::from_utf8(prefix) {
        Ok(text) => Some(text),
        Err(error) if error.error_len().is_none() => {
            std::str::from_utf8(prefix.get(..error.valid_up_to()).unwrap_or_default()).ok()
        }
        Err(_) => None,
    };
    if let Some(text) = text.filter(|text| !text.contains('\0')) {
        let visible = crate::terminal::clean(text).chars().filter(|ch| !matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')).collect::<String>();
        format!(
            "Text preview (first {} of {} bytes; terminal controls omitted):\n\n{visible}",
            prefix.len(),
            bytes.len()
        )
    } else {
        use std::fmt::Write as _;
        let mut preview = format!("Binary file · {} bytes. Hex preview (first 256 bytes):\n", bytes.len());
        for row in bytes.get(..bytes.len().min(256)).unwrap_or_default().chunks(16) {
            for byte in row {
                let _ = write!(preview, "{byte:02x} ");
            }
            preview.push('\n');
        }
        preview
    }
}

fn uploaded_file(value: Value, name: &str, size: usize, session: Option<&str>) -> Result<DraftFile> {
    #[derive(Deserialize)]
    struct Uploaded {
        upload: DraftFile,
        #[serde(default, rename = "serverSession")]
        server_session: Option<String>,
    }
    let result: Uploaded = serde_json::from_value(value)
        .context("Inbox did not return a usable upload; file was not attached")?;
    ensure!(
        session.is_none_or(|expected| result.server_session.as_deref().is_none_or(|saved| saved == expected)),
        "Inbox serverSession changed; rediscover before continuing"
    );
    let upload = result.upload;
    ensure!(
        upload.upload_id.starts_with("upl_") && path_id(&upload.upload_id).is_ok(),
        "Inbox returned an invalid upload id; file was not attached"
    );
    ensure!(
        upload.name == name && upload.size == size as u64,
        "Inbox upload metadata did not match the selected file; file was not attached"
    );
    ensure!(
        upload.sha256.len() == 64 && upload.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Inbox upload omitted its content hash; file was not attached"
    );
    Ok(upload)
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct ReplyDraft {
    pub(crate) v: u8,
    pub(crate) thread_id: String,
    pub(crate) body: String,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    #[serde(default)]
    pub(crate) files: Vec<DraftFile>,
    #[serde(default)]
    pub(crate) recipients: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DraftFileRef {
    pub(crate) upload_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pasted: Option<DraftPasted>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DraftFilePlace {
    pub(crate) upload_id: String,
    pub(crate) pasted: DraftPasted,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct DraftFileOps {
    #[serde(rename = "files_add")]
    pub(crate) add: Vec<DraftFileRef>,
    #[serde(rename = "files_remove")]
    pub(crate) remove: Vec<String>,
    #[serde(rename = "files_place")]
    pub(crate) place: Vec<DraftFilePlace>,
}

#[derive(Debug, Serialize)]
pub(crate) struct DraftSave<'a> {
    pub(crate) body: &'a str,
    pub(crate) base: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) base_body: Option<&'a str>,
    #[serde(flatten)]
    pub(crate) files: &'a DraftFileOps,
    /// None preserves recipients; Some([]) explicitly clears the choice.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) recipients: Option<&'a [String]>,
}

#[derive(Debug, Deserialize)]
struct DraftEnvelope {
    #[serde(default, rename = "serverSession")]
    server_session: Option<String>,
    draft: Option<ReplyDraft>,
}

#[derive(Debug)]
pub(crate) enum DraftWrite {
    Saved(Option<ReplyDraft>),
    Conflict(Option<ReplyDraft>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum AgentKind {
    Live,
    Mcp,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct TaggedAgent {
    pub(crate) session: String,
    pub(crate) host: Option<String>,
    pub(crate) kind: AgentKind,
    pub(crate) name: Option<String>,
    pub(crate) label: Option<String>,
    pub(crate) busy: Option<bool>,
    pub(crate) taggable: bool,
    pub(crate) owner: bool,
}

impl TaggedAgent {
    pub(crate) fn display_name(&self) -> &str {
        self.label.as_deref().or(self.name.as_deref()).or(self.host.as_deref()).unwrap_or(&self.session)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct TaggedAgents {
    #[serde(rename = "serverSession")]
    pub(crate) server_session: String,
    pub(crate) sessions: Vec<TaggedAgent>,
}

impl Client {
    fn upload_reply_file(
        &self,
        thread_id: &str,
        machine: Option<&str>,
        selected: &SelectedUpload,
    ) -> Result<DraftFile> {
        ensure!(
            self.machine_of(thread_id)?.as_deref() == machine,
            "Thread ownership changed; reopen the file picker and review its destination"
        );
        ensure!(selected.bytes.len() as u64 <= UPLOAD_MAX_BYTES, "Not attached: a file is at most 100 MiB");
        let value =
            self.upload_for_record(Some(thread_id), &selected.name, &selected.key, &selected.bytes)?;
        uploaded_file(
            value,
            &selected.name,
            selected.bytes.len(),
            machine.is_none().then(|| self.server_session()),
        )
    }

    pub(crate) fn reply_draft(&self, thread_id: &str) -> Result<Option<ReplyDraft>> {
        ensure!(
            self.machine_of(thread_id)?.is_none(),
            "Remote reply drafts are memory-only; no draft GET is supported"
        );
        let response: DraftEnvelope =
            self.get_json("GET", &format!("/api/inbox/threads/{}/draft", path_id(thread_id)?))?;
        if let Some(session) = response.server_session {
            ensure!(
                session == self.server_session(),
                "Inbox serverSession changed; rediscover before continuing"
            );
        }
        validate_draft(thread_id, response.draft)
    }

    pub(crate) fn save_reply_draft(&self, thread_id: &str, save: &DraftSave<'_>) -> Result<DraftWrite> {
        ensure!(
            self.machine_of(thread_id)?.is_none(),
            "Remote reply drafts are memory-only; no draft POST is supported"
        );
        let mut body = serde_json::to_value(save)?;
        body.as_object_mut()
            .context("Draft save body must be an object")?
            .insert("serverSession".to_owned(), json!(self.server_session()));
        let path = format!("/api/inbox/threads/{}/draft", path_id(thread_id)?);
        match self.post_json::<DraftEnvelope>(&path, &body) {
            Ok(response) => Ok(DraftWrite::Saved(validate_draft(thread_id, response.draft)?)),
            Err(error) => {
                if let Some(http) = error.downcast_ref::<HttpError>()
                    && http.status == 409
                    && http.code.as_deref() == Some("draft_conflict")
                {
                    let draft = match http.payload.get("draft") {
                        Some(value) => serde_json::from_value(value.clone())
                            .context("reading conflicting reply draft")?,
                        None => self.reply_draft(thread_id)?,
                    };
                    return Ok(DraftWrite::Conflict(validate_draft(thread_id, draft)?));
                }
                Err(error)
            }
        }
    }

    pub(crate) fn discard_reply_draft(&self, thread_id: &str) -> Result<bool> {
        #[derive(Deserialize)]
        struct Deleted {
            ok: bool,
            deleted: bool,
        }
        ensure!(
            self.machine_of(thread_id)?.is_none(),
            "Remote reply drafts are memory-only; no draft delete is supported"
        );
        let result: Deleted = self.post_json(
            &format!("/api/inbox/threads/{}/draft/delete", path_id(thread_id)?),
            &json!({"serverSession": self.server_session()}),
        )?;
        ensure!(result.ok, "Inbox did not confirm draft discard");
        Ok(result.deleted)
    }

    pub(crate) fn tagged_agents(&self, thread_id: &str) -> Result<TaggedAgents> {
        let path = format!("/api/inbox/threads/{}/taggable", path_id(thread_id)?);
        let result: TaggedAgents = self.get_json("GET", &path)?;
        if self.machine_of(thread_id)?.is_none() {
            ensure!(
                result.server_session == self.server_session(),
                "Inbox serverSession changed; rediscover before continuing"
            );
        }
        Ok(result)
    }
}

fn validate_draft(thread_id: &str, draft: Option<ReplyDraft>) -> Result<Option<ReplyDraft>> {
    if let Some(draft) = &draft {
        ensure!(draft.v == 1 && draft.thread_id == thread_id, "Inbox returned an incompatible reply draft");
    }
    Ok(draft)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SaveOutcome {
    Saved,
    Memory,
    Conflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SendDisposition {
    /// The thread accepted the reply. Delivery still comes from its delivery records.
    Accepted,
    /// Queued on this machine for another Inbox; not accepted there or delivered.
    Held,
}

#[derive(Debug)]
pub(crate) struct DraftSend {
    pub(crate) message_id: String,
    pub(crate) words: String,
    pub(crate) uploads: Vec<String>,
    pub(crate) tags: Vec<ReplyTag>,
    pub(crate) to: Option<Addressee>,
    pub(crate) clear_draft: String,
}

#[derive(Debug)]
pub(crate) struct DraftState {
    pub(crate) thread_id: String,
    pub(crate) draft: Option<ReplyDraft>,
    machine: Option<String>,
    body: String,
    files: Vec<DraftFile>,
    /// Bounded previews only; never local paths or a second copy of uploaded bytes.
    file_previews: BTreeMap<String, String>,
    recipients: Option<Vec<String>>,
    held_files: Vec<DraftFile>,
    /// Like the window, tag mode is a compose choice, not stored in the reply draft.
    tag_mode: TagMode,
    conflict: bool,
    claimed: bool,
    failed: bool,
}

impl DraftState {
    pub(crate) fn load(client: &Client, thread_id: &str) -> Result<Self> {
        let machine = client.machine_of(thread_id)?;
        let draft = if machine.is_some() { None } else { client.reply_draft(thread_id)? };
        let mut state = Self {
            thread_id: thread_id.to_owned(),
            draft,
            body: String::new(),
            files: Vec::new(),
            file_previews: BTreeMap::new(),
            machine,
            recipients: None,
            held_files: Vec::new(),
            tag_mode: TagMode::Ask,
            conflict: false,
            claimed: false,
            failed: false,
        };
        state.use_saved();
        Ok(state)
    }

    pub(crate) fn body(&self) -> &str {
        &self.body
    }
    pub(crate) fn files(&self) -> &[DraftFile] {
        &self.files
    }
    pub(crate) fn recipients(&self) -> Option<&[String]> {
        self.recipients.as_deref()
    }
    pub(crate) fn has_conflict(&self) -> bool {
        self.conflict
    }
    pub(crate) fn is_claimed(&self) -> bool {
        self.claimed
    }
    pub(crate) fn is_remote(&self) -> bool {
        self.machine.is_some()
    }

    fn check_owner(&self, client: &Client) -> Result<()> {
        ensure!(
            client.machine_of(&self.thread_id)? == self.machine,
            "Thread ownership changed; draft retained without uploading or saving to another Inbox"
        );
        Ok(())
    }

    pub(crate) fn tag_mode(&self) -> TagMode {
        self.tag_mode
    }

    pub(crate) fn set_tag_mode(
        &mut self,
        mode: TagMode,
        thread: &Thread,
        agents: &TaggedAgents,
    ) -> Result<()> {
        ensure!(!self.claimed, "reply is awaiting confirmation");
        let mut route = route_reply(thread, self.recipients.as_deref().unwrap_or_default(), agents)?;
        apply_tag_mode(&mut route.tags, mode, agents)?;
        self.tag_mode = mode;
        Ok(())
    }

    /// Pin the recipient when Reply opens, so an arriving agent message cannot move it.
    pub(crate) fn pin_recipient(&mut self, thread: &Thread) -> Result<()> {
        ensure!(thread.thread_id == self.thread_id, "draft belongs to another thread");
        if self.recipients.is_none()
            && let Some(message) = send::words_message(thread)
        {
            let session = if message.author.kind == "agent" {
                message.author.session.as_ref()
            } else {
                message.to.as_ref().map(|to| &to.session)
            };
            if let Some(session) = session.filter(|session| !session.starts_with("perm:")) {
                self.set_recipients(vec![session.clone()])?;
            }
        }
        Ok(())
    }

    pub(crate) fn is_dirty(&self) -> bool {
        self.conflict
            || self.failed
            || self.body != self.draft.as_ref().map_or("", |draft| draft.body.as_str())
            || self.files != self.held_files
            || self.recipients.as_deref() != self.draft.as_ref().and_then(|draft| draft.recipients.as_deref())
    }

    pub(crate) fn set_body(&mut self, body: String) -> Result<()> {
        ensure!(!self.claimed, "reply is awaiting confirmation; keep its exact send payload");
        if self.body == body {
            return Ok(());
        }
        for file in &mut self.files {
            if let Some(pasted) = &mut file.pasted {
                pasted.at = map_anchor(&self.body, &body, pasted.at);
            }
        }
        self.body = body;
        Ok(())
    }

    fn add_uploaded(&mut self, upload: DraftFile, preview: String) -> Result<()> {
        ensure!(!self.claimed, "reply is awaiting confirmation");
        ensure!(
            !self.files.iter().any(|file| file.upload_id == upload.upload_id),
            "Inbox returned an upload already in this draft"
        );
        self.file_previews.insert(upload.upload_id.clone(), preview);
        self.files.push(upload);
        Ok(())
    }

    fn remove_file(&mut self, upload_id: &str) -> Result<()> {
        ensure!(!self.claimed, "reply is awaiting confirmation");
        self.files.retain(|file| file.upload_id != upload_id);
        self.file_previews.remove(upload_id);
        Ok(())
    }

    pub(crate) fn set_recipients(&mut self, recipients: Vec<String>) -> Result<()> {
        ensure!(!self.claimed, "reply is awaiting confirmation");
        ensure!(recipients.len() <= RECIPIENTS_MAX, "a reply can reach at most {RECIPIENTS_MAX} agents");
        for (index, id) in recipients.iter().enumerate() {
            ensure!(
                valid_session(id) && !recipients.iter().take(index).any(|previous| previous == id),
                "invalid or duplicate recipient {id:?}"
            );
        }
        self.recipients = Some(recipients);
        self.tag_mode = TagMode::Ask;
        Ok(())
    }

    pub(crate) fn save(&mut self, client: &Client) -> Result<SaveOutcome> {
        ensure!(!self.claimed, "reply is awaiting confirmation");
        self.check_owner(client)?;
        if self.is_remote() {
            ensure!(self.body.encode_utf16().count() <= BODY_MAX, "Draft exceeds {BODY_MAX} characters");
            return Ok(SaveOutcome::Memory);
        }
        if self.conflict {
            return Ok(SaveOutcome::Conflict);
        }
        if !self.is_dirty() {
            return Ok(SaveOutcome::Saved);
        }
        ensure!(
            self.body.encode_utf16().count() <= BODY_MAX,
            "Not saved: draft exceeds {BODY_MAX} characters"
        );
        let files = file_ops(&self.held_files, &self.files);
        let request = DraftSave {
            body: &self.body,
            base: self.draft.as_ref().map(|draft| draft.updated_at.as_str()),
            base_body: self.draft.as_ref().map(|draft| draft.body.as_str()),
            files: &files,
            recipients: self.recipients.as_deref(),
        };
        match client.save_reply_draft(&self.thread_id, &request) {
            Ok(DraftWrite::Saved(draft)) => {
                self.draft = draft;
                self.use_saved();
                Ok(SaveOutcome::Saved)
            }
            Ok(DraftWrite::Conflict(draft)) => {
                self.draft = draft;
                self.conflict = true;
                self.failed = false;
                Ok(SaveOutcome::Conflict)
            }
            Err(error) => {
                // A lost answer can still have saved. Read it back once, never resend or
                // erase local words merely because the transport did not confirm them.
                if error.downcast_ref::<HttpError>().is_none_or(|http| http.status >= 500)
                    && let Ok(draft) = client.reply_draft(&self.thread_id)
                    && save_landed(&self.body, self.recipients.as_deref(), &files, draft.as_ref())
                {
                    self.draft = draft;
                    self.use_saved();
                    return Ok(SaveOutcome::Saved);
                }
                self.failed = true;
                Err(error)
            }
        }
    }

    pub(crate) fn refresh(&mut self, client: &Client) -> Result<()> {
        ensure!(!self.claimed, "reply is awaiting confirmation");
        self.check_owner(client)?;
        if self.is_remote() {
            return Ok(());
        }
        let draft = client.reply_draft(&self.thread_id)?;
        let old_body = self.draft.as_ref().map_or("", |draft| draft.body.as_str());
        let new_body = draft.as_ref().map_or("", |draft| draft.body.as_str());
        if !self.is_dirty() {
            self.draft = draft;
            self.use_saved();
        } else if self.draft.as_ref().map(|draft| &draft.updated_at)
            != draft.as_ref().map(|draft| &draft.updated_at)
            && (self.conflict || (new_body != old_body && new_body != self.body))
        {
            // Keep local words and file operations while displaying the newer conflict.
            // File-only changes leave our old base alone: the next save merges them and
            // returns all files, rather than falsely treating unseen files as synchronized.
            self.conflict = true;
            self.draft = draft;
        }
        Ok(())
    }

    /// Drop only local edits after a conflict. This never deletes the server's draft.
    pub(crate) fn use_saved(&mut self) {
        if self.claimed {
            return;
        }
        if self.is_remote() {
            return;
        }
        self.body = self.draft.as_ref().map_or_else(String::new, |draft| draft.body.clone());
        self.files = self.draft.as_ref().map_or_else(Vec::new, |draft| draft.files.clone());
        self.file_previews.retain(|id, _| self.files.iter().any(|file| &file.upload_id == id));
        self.held_files.clone_from(&self.files);
        self.recipients = self.draft.as_ref().and_then(|draft| draft.recipients.clone());
        self.conflict = false;
        self.failed = false;
    }

    /// Explicit authorization to overwrite the version shown in the conflict. A newer
    /// race can conflict again; it is never retried without another person decision.
    pub(crate) fn keep_mine(&mut self, client: &Client) -> Result<SaveOutcome> {
        ensure!(!self.claimed, "reply is awaiting confirmation");
        self.conflict = false;
        self.failed = true;
        self.save(client)
    }

    pub(crate) fn discard(&mut self, client: &Client) -> Result<()> {
        ensure!(!self.claimed, "reply is awaiting confirmation");
        self.check_owner(client)?;
        if !self.is_remote() {
            client.discard_reply_draft(&self.thread_id)?;
        }
        self.draft = None;
        self.tag_mode = TagMode::Ask;
        if self.is_remote() {
            self.body.clear();
            self.files.clear();
            self.file_previews.clear();
            self.held_files.clear();
            self.recipients = None;
            self.conflict = false;
            self.failed = false;
        }
        self.use_saved();
        Ok(())
    }

    /// Call once on explicit Send. Permission-question replies and other agents' picks
    /// remain separate targets; only the words target receives this envelope.
    pub(crate) fn prepare_send(&mut self, client: &Client, thread: &Thread) -> Result<DraftSend> {
        ensure!(thread.thread_id == self.thread_id, "draft belongs to another thread");
        ensure!(
            self.save(client)? != SaveOutcome::Conflict,
            "draft changed elsewhere: Keep mine or Use saved first"
        );
        ensure!(
            !self.body.trim().is_empty() || !self.files.is_empty(),
            "draft has no words or files to send"
        );
        let agents = client.tagged_agents(&self.thread_id)?;
        let mut route = route_reply(thread, self.recipients.as_deref().unwrap_or_default(), &agents)?;
        apply_tag_mode(&mut route.tags, self.tag_mode, &agents)?;
        let send = DraftSend {
            message_id: route.message_id,
            words: self.body.clone(),
            uploads: self.files.iter().map(|file| file.upload_id.clone()).collect(),
            tags: route.tags,
            to: route.to,
            clear_draft: self.draft.as_ref().map_or_else(String::new, |draft| draft.updated_at.clone()),
        };
        self.claimed = true;
        Ok(send)
    }

    pub(crate) fn release_send(&mut self) {
        self.claimed = false;
    }

    pub(crate) fn after_send(&mut self, client: &Client, result: &ReplyResult) -> Result<SendDisposition> {
        if result.held.is_some() {
            // A held request owns this exact body until its outbox result settles.
            // Do not save it again or announce that the agent received it.
            return Ok(SendDisposition::Held);
        }
        ensure!(result.reply.is_some(), "Inbox did not confirm a reply; draft retained");
        if self.is_remote() {
            self.claimed = false;
            self.discard(client)?;
            return Ok(SendDisposition::Accepted);
        }
        self.claimed = false;
        self.draft = None;
        self.tag_mode = TagMode::Ask;
        self.use_saved();
        // The server can retain another window's words or files we did not carry.
        // If this read fails, local sent words remain cleared, never re-saved.
        self.draft = client.reply_draft(&self.thread_id)?;
        self.use_saved();
        Ok(SendDisposition::Accepted)
    }
}

fn valid_session(value: &str) -> bool {
    let mut bytes = value.bytes();
    value.len() <= 200
        && bytes.next().is_some_and(|byte| byte.is_ascii_alphanumeric())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
}

fn file_ops(before: &[DraftFile], after: &[DraftFile]) -> DraftFileOps {
    DraftFileOps {
        add: after
            .iter()
            .filter(|file| !before.iter().any(|old| old.upload_id == file.upload_id))
            .map(|file| DraftFileRef { upload_id: file.upload_id.clone(), pasted: file.pasted.clone() })
            .collect(),
        remove: before
            .iter()
            .filter(|file| !after.iter().any(|new| new.upload_id == file.upload_id))
            .map(|file| file.upload_id.clone())
            .collect(),
        place: after
            .iter()
            .filter_map(|file| {
                file.pasted.as_ref().map(|pasted| DraftFilePlace {
                    upload_id: file.upload_id.clone(),
                    pasted: pasted.clone(),
                })
            })
            .collect(),
    }
}

/// Move pasted-text anchors through one editor change, using upstream UTF-16 offsets.
fn map_anchor(before: &str, after: &str, at: usize) -> usize {
    if before == after {
        return at.min(after.encode_utf16().count());
    }
    let start = before
        .chars()
        .zip(after.chars())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf16())
        .sum::<usize>();
    let old_len = before.encode_utf16().count();
    let new_len = after.encode_utf16().count();
    let mut suffix = 0;
    for (a, b) in before.chars().rev().zip(after.chars().rev()) {
        if a != b || suffix + a.len_utf16() > old_len.min(new_len) - start {
            break;
        }
        suffix += a.len_utf16();
    }
    if at <= start {
        return at;
    }
    if at >= old_len - suffix {
        return (new_len - suffix + at.saturating_sub(old_len - suffix)).min(new_len);
    }
    let boundary = |text: &str, offset: usize| {
        let mut units = 0;
        for (index, ch) in text.char_indices() {
            if units == offset {
                return index;
            }
            units += ch.len_utf16();
        }
        text.len()
    };
    let old = &before[boundary(before, start)..boundary(before, old_len - suffix)];
    let span = &after[boundary(after, start)..boundary(after, new_len - suffix)];
    if let Some(index) = span.find(old)
        && span.rfind(old) == Some(index)
    {
        return start + span[..index].encode_utf16().count() + at - start;
    }
    start
}

fn save_landed(
    body: &str,
    recipients: Option<&[String]>,
    ops: &DraftFileOps,
    draft: Option<&ReplyDraft>,
) -> bool {
    let Some(draft) = draft else {
        return body.trim().is_empty() && ops.add.is_empty();
    };
    draft.body == body
        && recipients.is_none_or(|ids| draft.recipients.as_deref().unwrap_or_default() == ids)
        && ops.add.iter().all(|added| {
            draft.files.iter().any(|file| file.upload_id == added.upload_id && file.pasted == added.pasted)
        })
        && ops.remove.iter().all(|removed| !draft.files.iter().any(|file| &file.upload_id == removed))
        && ops.place.iter().all(|placed| {
            draft
                .files
                .iter()
                .find(|file| file.upload_id == placed.upload_id)
                .is_none_or(|file| file.pasted.as_ref() == Some(&placed.pasted))
        })
}

#[derive(Debug)]
pub(crate) struct ReplyRoute {
    pub(crate) message_id: String,
    pub(crate) to: Option<Addressee>,
    pub(crate) tags: Vec<ReplyTag>,
}

/// First recipient answers its own latest message; the rest are explicit Ask tags.
/// A session that never wrote here must be live to take the reply alone. Never silently
/// redirect a stale saved choice to the thread's current writer.
pub(crate) fn route_reply(
    thread: &Thread,
    recipients: &[String],
    agents: &TaggedAgents,
) -> Result<ReplyRoute> {
    let default = send::words_message(thread).context("thread has no reply target")?;
    ensure!(recipients.len() <= RECIPIENTS_MAX, "too many reply recipients");
    ensure!(
        !default.author.session.as_deref().is_some_and(|session| session.starts_with("perm:")),
        "permission prompts cannot receive reply words"
    );
    for session in recipients {
        ensure!(
            valid_session(session) && !session.starts_with("perm:"),
            "invalid reply recipient {session:?}"
        );
    }
    let owner = if default.author.kind == "agent" {
        default.author.session.as_deref()
    } else {
        default.to.as_ref().map(|to| to.session.as_str())
    };
    let chosen = recipients.first().map(String::as_str).or(owner);
    let target = chosen
        .and_then(|session| {
            thread.messages.iter().rev().find(|message| {
                message.author.kind == "agent" && message.author.session.as_deref() == Some(session)
            })
        })
        .filter(|_| chosen != owner)
        .unwrap_or(default);
    let to = if chosen != owner && target.id == default.id {
        let session = chosen.context("reply has no recipient")?;
        let agent = agents
            .sessions
            .iter()
            .find(|agent| agent.session == session)
            .context("recipient ended and has no message to answer; choose another recipient")?;
        ensure!(
            agent.kind == AgentKind::Live,
            "this Inbox reader has no message to answer; add it as a tag instead"
        );
        Some(Addressee { host: agent.host.clone(), session: session.to_owned() })
    } else {
        None
    };
    let mut tags = Vec::new();
    for session in recipients.iter().skip(1) {
        if Some(session.as_str()) == chosen || tags.iter().any(|tag: &ReplyTag| tag.session == *session) {
            continue;
        }
        let agent = agents
            .sessions
            .iter()
            .find(|agent| agent.session == *session)
            .with_context(|| format!("tagged recipient {session} ended; remove it before sending"))?;
        ensure!(agent.taggable, "update Plannotator to tag {}", agent.display_name());
        tags.push(ReplyTag {
            host: agent.host.clone(),
            session: session.clone(),
            label: Some(agent.display_name().to_owned()),
            mode: Some(TagMode::Ask),
        });
    }
    ensure!(tags.len() < RECIPIENTS_MAX, "too many tagged recipients");
    Ok(ReplyRoute { message_id: target.id.clone(), to, tags })
}

fn apply_tag_mode(tags: &mut [ReplyTag], mode: TagMode, agents: &TaggedAgents) -> Result<()> {
    if mode != TagMode::Ask {
        ensure!(!tags.is_empty(), "Choose an additional recipient before Add or Hand off");
        ensure!(
            mode != TagMode::Handoff || tags.len() == 1,
            "Hand off takes exactly one additional recipient"
        );
        for tag in tags.iter() {
            let agent = agents
                .sessions
                .iter()
                .find(|agent| agent.session == tag.session)
                .context("Tagged agent is no longer available; choose recipients again")?;
            ensure!(
                agent.kind == AgentKind::Live && agent.taggable && !agent.owner,
                "Add and Hand off require a live, taggable agent other than the thread owner"
            );
        }
    }
    for tag in tags {
        tag.mode = Some(mode);
    }
    Ok(())
}

fn tag_mode_label(mode: TagMode) -> &'static str {
    match mode {
        TagMode::Ask => "Ask",
        TagMode::Add => "Add to thread",
        TagMode::Handoff => "Hand off thread",
    }
}

#[derive(Debug)]
struct RecipientChoice {
    session: String,
    label: String,
    state: &'static str,
    alone: bool,
    add: bool,
}

fn choices(thread: &Thread, agents: &TaggedAgents) -> Vec<RecipientChoice> {
    let mut choices: Vec<RecipientChoice> = agents
        .sessions
        .iter()
        .map(|agent| {
            let wrote = thread.messages.iter().any(|message| {
                message.author.kind == "agent"
                    && message.author.session.as_deref() == Some(agent.session.as_str())
            });
            RecipientChoice {
                session: agent.session.clone(),
                label: format!("{} · {}", agent.display_name(), agent.session),
                state: if agent.kind == AgentKind::Mcp {
                    "reads Inbox; not woken"
                } else if !agent.taggable {
                    "update required for tags"
                } else if agent.busy == Some(true) {
                    "working"
                } else {
                    "live"
                },
                alone: wrote || agent.kind == AgentKind::Live,
                add: agent.taggable,
            }
        })
        .collect();
    for message in &thread.messages {
        if message.author.kind != "agent" {
            continue;
        }
        let Some(session) = &message.author.session else { continue };
        if session.starts_with("perm:") || choices.iter().any(|choice| choice.session == *session) {
            continue;
        }
        choices.push(RecipientChoice {
            session: session.clone(),
            label: format!(
                "{} · {session}",
                message.author.name.as_deref().or(message.author.host.as_deref()).unwrap_or("Agent")
            ),
            state: "ended; reply waits for its return",
            alone: true,
            add: false,
        });
    }
    choices
}

/// Open the editor with the existing local state, including unsaved conflict words.
/// The caller must not replace a dirty `DraftState` with a fresh `load` to open this pane.
pub(crate) fn edit_draft(
    client: &Client,
    terminal: &mut ratatui::DefaultTerminal,
    state: &mut DraftState,
) -> Result<()> {
    ensure!(!state.is_claimed(), "reply is awaiting confirmation; resolve its send first");
    state.check_owner(client)?;
    let thread = client.thread(&state.thread_id)?.thread;
    state.pin_recipient(&thread)?;
    let mut editor = Compose::with_text(state.body());
    let mut status = String::from("Drafts never send themselves");
    let mut changed = None;
    let mut discard_armed = false;
    loop {
        terminal.draw(|frame| draw_draft_editor(frame, state, &editor, &status))?;
        if changed.is_some_and(|at: Instant| at.elapsed() >= SAVE_DELAY) {
            changed = None;
            status = save_status(state.save(client));
            if editor.value() != state.body() {
                editor = Compose::with_text(state.body());
            }
        }
        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                if !(ctrl && key.code == KeyCode::Char('d')) {
                    discard_armed = false;
                }
                if ctrl {
                    match key.code {
                        KeyCode::Char('s') => {
                            changed = None;
                            status = save_status(state.save(client));
                        }
                        KeyCode::Char('u') => match choose_upload(client, terminal, &state.thread_id) {
                            Ok(Some((file, preview))) => {
                                state.add_uploaded(file, preview)?;
                                changed = None;
                                status = save_status(state.save(client));
                            }
                            Ok(None) => {}
                            Err(error) => status = format!("Not attached: {error:#}"),
                        },
                        KeyCode::Char('f') => {
                            if let Err(error) = manage_files(client, terminal, state) {
                                status = format!("Files: {error:#}");
                            }
                        }
                        KeyCode::Char('r') => {
                            match pick_recipients(client, terminal, &thread, state.recipients()) {
                                Ok(Some(ids)) => {
                                    state.set_recipients(ids)?;
                                    changed = Some(Instant::now());
                                }
                                Ok(None) => {}
                                Err(error) => status = error.to_string(),
                            }
                        }
                        KeyCode::Char('m') => match pick_tag_mode(client, terminal, &thread, state) {
                            Ok(()) => {
                                status = format!(
                                    "{} on next Send; mode stays in this window only",
                                    tag_mode_label(state.tag_mode())
                                );
                            }
                            Err(error) => status = error.to_string(),
                        },
                        KeyCode::Char('k' | 't' | 'l' | 'd') => {
                            if draft_state_key(
                                client,
                                state,
                                key.code,
                                &mut changed,
                                &mut status,
                                &mut discard_armed,
                            ) {
                                return Ok(());
                            }
                        }
                        _ => {
                            editor.handle_key(key);
                            state.set_body(editor.value())?;
                            changed = Some(Instant::now());
                        }
                    }
                    if editor.value() != state.body() {
                        editor = Compose::with_text(state.body());
                    }
                    continue;
                }
                match editor.handle_key(key) {
                    ComposeAction::Save | ComposeAction::Cancel => {
                        changed = None;
                        match state.save(client) {
                            Ok(SaveOutcome::Saved | SaveOutcome::Memory) => return Ok(()),
                            result => status = save_status(result),
                        }
                    }
                    ComposeAction::Edited => {
                        let body = editor.value();
                        if body != state.body() {
                            state.set_body(body)?;
                            changed = Some(Instant::now());
                        }
                    }
                }
            }
            Event::Paste(text) => {
                editor.insert_text(&text);
                state.set_body(editor.value())?;
                changed = Some(Instant::now());
            }
            _ => {}
        }
    }
}

fn draw_draft_editor(frame: &mut ratatui::Frame, state: &DraftState, editor: &Compose, status: &str) {
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(4), Constraint::Min(3), Constraint::Length(4)])
            .areas(frame.area());
    let recipients = state
        .recipients()
        .filter(|ids| !ids.is_empty())
        .map_or_else(|| "thread's reply recipient".to_owned(), |ids| ids.join(", "));
    let saved = if state.is_remote() {
        "Kept in memory only · quit loses this draft · pending files expire after 24h".to_owned()
    } else if state.has_conflict() {
        "CONFLICT · local words not saved".to_owned()
    } else if state.is_dirty() {
        "Not saved".to_owned()
    } else {
        state
            .draft
            .as_ref()
            .map_or_else(|| "No draft".to_owned(), |draft| format!("Saved {}", draft.updated_at))
    };
    frame.render_widget(
        Paragraph::new(format!(
            "{saved}\nTo: {recipients} · {} files · {}\n{status}",
            state.files().len(),
            tag_mode_label(state.tag_mode())
        )),
        header,
    );
    let block = Block::default().title("Reply draft (not Send)").borders(Borders::ALL);
    let inner = block.inner(body);
    frame.render_widget(block, body);
    let (lines, row, col) = editor.wrapped(usize::from(inner.width.max(1)));
    let scroll = row.saturating_sub(usize::from(inner.height.saturating_sub(1)));
    frame.render_widget(
        Paragraph::new(lines.into_iter().map(Line::from).collect::<Vec<_>>())
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)),
        inner,
    );
    if inner.height > 0 && inner.width > 0 {
        frame.set_cursor_position((
            inner.x + u16::try_from(col).unwrap_or(0).min(inner.width - 1),
            inner.y + u16::try_from(row.saturating_sub(scroll)).unwrap_or(0).min(inner.height - 1),
        ));
    }
    frame.render_widget(Paragraph::new("Enter/Esc save & close · Alt-Enter newline · Ctrl-S save · Ctrl-R recipients · Ctrl-M tag mode\nCtrl-U upload file · Ctrl-F preview/remove files\nCtrl-K Keep mine · Ctrl-T Use saved · Ctrl-L reload · Ctrl-D twice discard").wrap(Wrap { trim: false }), footer);
}

fn draft_state_key(
    client: &Client,
    state: &mut DraftState,
    code: KeyCode,
    changed: &mut Option<Instant>,
    status: &mut String,
    discard_armed: &mut bool,
) -> bool {
    match code {
        KeyCode::Char('k') => {
            *changed = None;
            *status = save_status(state.keep_mine(client));
        }
        KeyCode::Char('t') => {
            if state.is_remote() {
                "Remote draft stays in memory; Ctrl-D twice explicitly discards it".clone_into(status);
            } else {
                state.use_saved();
                *changed = None;
                "Local edits discarded; server draft unchanged".clone_into(status);
            }
        }
        KeyCode::Char('l') => {
            *status = match state.refresh(client) {
                Ok(()) if state.is_remote() => {
                    "In-memory remote draft retained; no server draft endpoint".to_owned()
                }
                Ok(()) => "Draft read again".to_owned(),
                Err(error) => error.to_string(),
            };
        }
        KeyCode::Char('d') if *discard_armed => {
            match state.discard(client) {
                Ok(()) => return true,
                Err(error) => *status = error.to_string(),
            }
            *discard_armed = false;
        }
        KeyCode::Char('d') => {
            *discard_armed = true;
            if state.is_remote() {
                "Ctrl-D again discards this in-memory remote draft, including its file references"
            } else {
                "Ctrl-D again discards the shared draft, including its files"
            }
            .clone_into(status);
        }
        _ => {}
    }
    false
}

fn choose_upload(
    client: &Client,
    terminal: &mut ratatui::DefaultTerminal,
    thread_id: &str,
) -> Result<Option<(DraftFile, String)>> {
    let machine = client.machine_of(thread_id)?;
    let destination = machine.as_ref().map_or_else(
        || client.url().to_owned(),
        |machine| format!("linked machine {machine} via {}", client.url()),
    );
    let mut editor = Compose::default();
    let mut selected: Option<SelectedUpload> = None;
    let mut status = String::from(
        "Enter one literal local path; no shell expansion. Enter reads a preview, not an upload.",
    );
    let mut scroll = 0_u16;
    loop {
        terminal.draw(|frame| {
            let [header, body, footer] = Layout::vertical([Constraint::Length(6), Constraint::Min(3), Constraint::Length(4)]).areas(frame.area());
            if let Some(file) = &selected {
                frame.render_widget(Paragraph::new(format!("Upload {} ({} bytes) to {destination}?\nOnly the basename and bytes leave this computer.\nThe server stores a pending file now; agents receive it only on Send.\nRemote files must be sent within 24 hours; their draft is held in memory only.", file.name, file.bytes.len())).wrap(Wrap { trim: false }), header);
                frame.render_widget(Paragraph::new(file.preview.as_str()).block(Block::default().title("Selected bytes · read-only preview").borders(Borders::ALL)).wrap(Wrap { trim: false }).scroll((scroll, 0)), body);
            } else {
                frame.render_widget(Paragraph::new("Select a file to attach\nAt most 100 MiB. Regular files only; no symlinks, directories, devices or secret .env files.\nNothing is uploaded until the next screen's explicit y confirmation.").wrap(Wrap { trim: false }), header);
                let block = Block::default().title("Local file path (never saved or transmitted)").borders(Borders::ALL);
                let inner = block.inner(body);
                frame.render_widget(block, body);
                let (lines, row, col) = editor.wrapped(usize::from(inner.width.max(1)));
                frame.render_widget(Paragraph::new(lines.into_iter().map(Line::from).collect::<Vec<_>>()), inner);
                if inner.height > 0 && inner.width > 0 {
                    frame.set_cursor_position((inner.x + u16::try_from(col).unwrap_or(0).min(inner.width - 1), inner.y + u16::try_from(row).unwrap_or(0).min(inner.height - 1)));
                }
            }
            frame.render_widget(Paragraph::new(format!("{status}\n{}", if selected.is_some() { "y upload these bytes · n choose another · ↑/↓/PgUp/PgDn preview · Esc cancel" } else { "Enter preview · Esc cancel · paste a path or type it exactly (no quotes)" })).wrap(Wrap { trim: false }), footer);
        })?;
        match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if key.code == KeyCode::Esc {
                    return Ok(None);
                }
                if let Some(file) = &selected {
                    match key.code {
                        KeyCode::Char('y') if key.modifiers.is_empty() => {
                            "Uploading selected bytes; no message is being sent…".clone_into(&mut status);
                            terminal.draw(|frame| {
                                frame.render_widget(Paragraph::new(status.as_str()), frame.area());
                            })?;
                            match client.upload_reply_file(thread_id, machine.as_deref(), file) {
                                Ok(upload) => {
                                    return Ok(Some((
                                        upload,
                                        selected.take().context("Selected upload disappeared")?.preview,
                                    )));
                                }
                                Err(error) => {
                                    status = format!(
                                        "Not attached: {error:#}. No automatic retry; y retries the exact selected bytes with the same key and destination. Esc abandons this attempt."
                                    );
                                }
                            }
                        }
                        KeyCode::Char('n') => {
                            selected = None;
                            scroll = 0;
                            "Choose another file; nothing is uploaded automatically".clone_into(&mut status);
                        }
                        KeyCode::Up => scroll = scroll.saturating_sub(1),
                        KeyCode::Down => scroll = scroll.saturating_add(1),
                        KeyCode::PageUp => scroll = scroll.saturating_sub(10),
                        KeyCode::PageDown => scroll = scroll.saturating_add(10),
                        _ => {}
                    }
                } else if editor.handle_key(key) == ComposeAction::Save {
                    match SelectedUpload::read(Path::new(&editor.value())) {
                        Ok(file) => {
                            selected = Some(file);
                            "Review the file and destination, then y to upload or Esc to cancel"
                                .clone_into(&mut status);
                        }
                        Err(error) => status = format!("Not attached: {error:#}"),
                    }
                }
            }
            Event::Paste(text) if selected.is_none() => {
                if text.chars().any(char::is_control) {
                    "Paste a single literal file path without line breaks or control characters"
                        .clone_into(&mut status);
                } else {
                    editor.insert_text(&text);
                }
            }
            _ => {}
        }
    }
}

fn manage_files(
    client: &Client,
    terminal: &mut ratatui::DefaultTerminal,
    state: &mut DraftState,
) -> Result<()> {
    let mut cursor = ListState::default().with_selected(Some(0));
    let mut scroll = 0_u16;
    let mut remove: Option<String> = None;
    let mut status = String::from("Files belong to the reply draft. Nothing here sends a message.");
    loop {
        terminal.draw(|frame| {
            let [header, body, footer] = Layout::vertical([Constraint::Length(6), Constraint::Min(3), Constraint::Length(4)]).areas(frame.area());
            let rows = state.files().iter().map(|file| ListItem::new(format!("{} · {} bytes · {}", file.name, file.size, file.kind))).collect::<Vec<_>>();
            frame.render_stateful_widget(List::new(rows).block(Block::default().title("Draft files").borders(Borders::ALL)).highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)), header, &mut cursor);
            let detail = state.files().get(cursor.selected().unwrap_or(0)).map_or_else(|| "No files in this draft. Esc then Ctrl-U to select one.".to_owned(), |file| {
                let preview = state.file_previews.get(&file.upload_id).map_or("Content preview unavailable: this pending file was restored from the server. The Inbox exposes no general pending-upload content route; after Send, use the thread's attachment viewer.", String::as_str);
                format!("{}\nUpload: {}\nSHA-256: {}\nCreated: {}\nType: {} · {} × {} · {} ms\n\n{preview}", file.name, file.upload_id, file.sha256, file.created_at, file.media_type.as_deref().unwrap_or(&file.kind), file.width.map_or_else(|| "—".to_owned(), |value| value.to_string()), file.height.map_or_else(|| "—".to_owned(), |value| value.to_string()), file.duration_ms.map_or_else(|| "—".to_owned(), |value| value.to_string()))
            });
            frame.render_widget(Paragraph::new(detail).block(Block::default().title("Read-only preview").borders(Borders::ALL)).wrap(Wrap { trim: false }).scroll((scroll, 0)), body);
            frame.render_widget(Paragraph::new(format!("{status}\n↑/↓ choose · PgUp/PgDn scroll · Delete remove (then y) · Ctrl-S save removal · Esc back")).wrap(Wrap { trim: false }), footer);
        })?;
        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Release {
                continue;
            }
            let index = cursor.selected().unwrap_or(0);
            if key.code == KeyCode::Char('y') && key.modifiers.is_empty() {
                if let Some(id) = remove.take() {
                    state.remove_file(&id)?;
                    status = save_status(state.save(client));
                    cursor.select(Some(index.min(state.files().len().saturating_sub(1))));
                    scroll = 0;
                }
                continue;
            }
            remove = None;
            match key.code {
                KeyCode::Esc => return Ok(()),
                KeyCode::Up => {
                    cursor.select(Some(index.saturating_sub(1)));
                    scroll = 0;
                }
                KeyCode::Down => {
                    cursor.select(Some((index + 1).min(state.files().len().saturating_sub(1))));
                    scroll = 0;
                }
                KeyCode::PageUp => scroll = scroll.saturating_sub(10),
                KeyCode::PageDown => scroll = scroll.saturating_add(10),
                KeyCode::Delete => {
                    if let Some(file) = state.files().get(index) {
                        remove = Some(file.upload_id.clone());
                        status = format!(
                            "y removes {} from this shared draft and saves. Local file is untouched; server sweeps unreferenced uploads after 24 hours. Any other key cancels.",
                            file.name
                        );
                    }
                }
                KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    status = save_status(state.save(client));
                }
                _ => {}
            }
        }
    }
}

fn save_status(result: Result<SaveOutcome>) -> String {
    match result {
        Ok(SaveOutcome::Saved) => "Draft saved; nothing sent".to_owned(),
        Ok(SaveOutcome::Memory) => {
            "Remote draft kept in memory only; nothing sent. Quit loses it; pending files expire after 24h"
                .to_owned()
        }
        Ok(SaveOutcome::Conflict) => {
            "Changed in another window: Ctrl-K Keep mine or Ctrl-T Use saved".to_owned()
        }
        Err(error) => format!("Not saved: {error}"),
    }
}

fn pick_tag_mode(
    client: &Client,
    terminal: &mut ratatui::DefaultTerminal,
    thread: &Thread,
    state: &mut DraftState,
) -> Result<()> {
    let agents = client.tagged_agents(&state.thread_id)?;
    let mut status = String::from(
        "Ask sends a copy; Add includes agents in the thread; Hand off gives one agent ownership",
    );
    loop {
        terminal.draw(|frame| frame.render_widget(Paragraph::new(format!("Current: {}\n\n1  Ask tagged agents\n2  Add tagged agents to this thread\n3  Hand this thread to one tagged agent\n\n{status}\n\nEsc cancels; nothing is sent until Send", tag_mode_label(state.tag_mode())))
            .block(Block::default().title("Tagged agents' role").borders(Borders::ALL)).wrap(Wrap { trim: false }), frame.area()))?;
        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Release {
                continue;
            }
            let mode = match key.code {
                KeyCode::Esc => return Ok(()),
                KeyCode::Char('1') => TagMode::Ask,
                KeyCode::Char('2') => TagMode::Add,
                KeyCode::Char('3') => TagMode::Handoff,
                _ => continue,
            };
            match state.set_tag_mode(mode, thread, &agents) {
                Ok(()) => return Ok(()),
                Err(error) => status = error.to_string(),
            }
        }
    }
}

fn pick_recipients(
    client: &Client,
    terminal: &mut ratatui::DefaultTerminal,
    thread: &Thread,
    selected: Option<&[String]>,
) -> Result<Option<Vec<String>>> {
    let agents = client.tagged_agents(&thread.thread_id)?;
    let choices = choices(thread, &agents);
    ensure!(!choices.is_empty(), "No recipients available");
    let mut chosen = selected.map_or_else(
        || {
            send::words_message(thread)
                .and_then(|message| {
                    message
                        .author
                        .session
                        .clone()
                        .or_else(|| message.to.as_ref().map(|to| to.session.clone()))
                })
                .into_iter()
                .collect()
        },
        <[String]>::to_vec,
    );
    let mut cursor = ListState::default().with_selected(Some(0));
    let mut status =
        String::from("Enter makes the selected agent the only recipient; Space toggles an additional agent");
    loop {
        terminal.draw(|frame| {
            let [body, footer] =
                Layout::vertical([Constraint::Min(3), Constraint::Length(3)]).areas(frame.area());
            let items = choices
                .iter()
                .map(|choice| {
                    let mark = match chosen.iter().position(|id| id == &choice.session) {
                        Some(0) => "To",
                        Some(_) => "+ ",
                        None => "  ",
                    };
                    ListItem::new(format!("[{mark}] {} · {}", choice.label, choice.state))
                })
                .collect::<Vec<_>>();
            frame.render_stateful_widget(
                List::new(items)
                    .block(
                        Block::default()
                            .title("Reply recipients · first answers, others tagged")
                            .borders(Borders::ALL),
                    )
                    .highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                body,
                &mut cursor,
            );
            frame.render_widget(
                Paragraph::new(format!(
                    "{status}\n↑/↓ choose · Enter only · Space toggle · Ctrl-S apply · Esc cancel"
                ))
                .wrap(Wrap { trim: false }),
                footer,
            );
        })?;
        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Release {
                continue;
            }
            let index = cursor.selected().unwrap_or(0);
            match key.code {
                KeyCode::Esc => return Ok(None),
                KeyCode::Up => cursor.select(Some(index.saturating_sub(1))),
                KeyCode::Down => cursor.select(Some((index + 1).min(choices.len() - 1))),
                KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    match route_reply(thread, &chosen, &agents) {
                        Ok(_) => return Ok(Some(chosen)),
                        Err(error) => status = error.to_string(),
                    }
                }
                KeyCode::Enter => {
                    if let Some(choice) = choices.get(index) {
                        if choice.alone {
                            return Ok(Some(vec![choice.session.clone()]));
                        }
                        "This Inbox reader can be added as a tag, not the sole recipient"
                            .clone_into(&mut status);
                    }
                }
                KeyCode::Char(' ') => {
                    let Some(choice) = choices.get(index) else {
                        continue;
                    };
                    if let Some(position) = chosen.iter().position(|id| id == &choice.session) {
                        if chosen.len() == 1 {
                            "Keep at least one recipient".clone_into(&mut status);
                            continue;
                        }
                        let mut next = chosen.clone();
                        next.remove(position);
                        if let Some(lead) = next
                            .iter()
                            .position(|id| choices.iter().any(|choice| &choice.session == id && choice.alone))
                        {
                            let first = next.remove(lead);
                            next.insert(0, first);
                            chosen = next;
                        } else {
                            "An Inbox reader without a message cannot take the reply alone"
                                .clone_into(&mut status);
                        }
                    } else if !choice.add {
                        "Ended or older sessions cannot be additional recipients".clone_into(&mut status);
                    } else if chosen.len() >= RECIPIENTS_MAX {
                        status = format!("A reply can reach at most {RECIPIENTS_MAX} agents");
                    } else {
                        chosen.push(choice.session.clone());
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(body: &str) -> ReplyDraft {
        ReplyDraft {
            v: 1,
            thread_id: "thr_1".to_owned(),
            body: body.to_owned(),
            created_at: "one".to_owned(),
            updated_at: "two".to_owned(),
            files: Vec::new(),
            recipients: None,
        }
    }

    fn state() -> DraftState {
        let mut state = DraftState {
            thread_id: "thr_1".to_owned(),
            draft: Some(draft("saved")),
            machine: None,
            body: String::new(),
            files: Vec::new(),
            file_previews: BTreeMap::new(),
            recipients: None,
            held_files: Vec::new(),
            tag_mode: TagMode::Ask,
            conflict: false,
            claimed: false,
            failed: false,
        };
        state.use_saved();
        state
    }

    #[test]
    fn conflict_keeps_local_words_until_explicit_choice() -> Result<()> {
        let mut state = state();
        state.set_body("mine".to_owned())?;
        state.draft = Some(draft("theirs"));
        state.conflict = true;
        assert_eq!(state.body(), "mine");
        assert!(state.is_dirty());
        state.use_saved();
        assert_eq!(state.body(), "theirs");
        assert!(!state.has_conflict());
        assert!(!state.is_dirty());
        Ok(())
    }

    #[test]
    fn claimed_draft_refuses_edits() {
        let mut state = state();
        state.claimed = true;
        assert!(state.set_body("different send".to_owned()).is_err());
        assert!(state.set_recipients(vec!["agent:1".to_owned()]).is_err());
        state.use_saved();
        assert!(state.is_claimed());
    }

    #[test]
    fn recipient_choice_accepts_upstream_session_punctuation() -> Result<()> {
        let mut state = state();
        state.set_recipients(vec!["pi:abc.def-1_2".to_owned()])?;
        assert!(state.set_recipients(vec!["a".to_owned(), "a".to_owned()]).is_err());
        assert!(state.set_recipients(vec!["/api/other".to_owned()]).is_err());
        Ok(())
    }

    #[test]
    fn save_wire_always_names_file_ops_and_only_chosen_recipients() -> Result<()> {
        let ops = DraftFileOps::default();
        let body = serde_json::to_value(DraftSave {
            body: "hello",
            base: None,
            base_body: None,
            files: &ops,
            recipients: None,
        })?;
        assert_eq!(
            body,
            json!({"body":"hello","base":null,"files_add":[],"files_remove":[],"files_place":[]})
        );
        Ok(())
    }

    #[test]
    fn pasted_anchors_use_utf16_without_splitting_emoji() {
        assert_eq!(map_anchor("Hello world", "Oh. Hello world", 6), 10);
        assert_eq!(map_anchor("😀Hello world", "x😀Hello world", 8), 9);
        assert_eq!(map_anchor("A😀B", "AB", 3), 1);
        assert_eq!(map_anchor("hello", "XhelloY", 3), 4);
        assert_eq!(map_anchor("abc", "aXXbc", 1), 1);
    }

    #[test]
    fn recipient_without_message_cannot_silently_redirect() {
        let thread = Thread {
            thread_id: "thr_1".to_owned(),
            messages: vec![super::super::client::Message {
                id: "msg_1".to_owned(),
                author: super::super::client::Author {
                    kind: "agent".to_owned(),
                    session: Some("old".to_owned()),
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        };
        let agents = TaggedAgents { server_session: "s".to_owned(), sessions: Vec::new() };
        assert!(route_reply(&thread, &["ended".to_owned()], &agents).is_err());
    }

    fn file(id: &str) -> DraftFile {
        DraftFile {
            upload_id: id.to_owned(),
            name: id.to_owned(),
            size: 1,
            kind: "text".to_owned(),
            sha256: "sha".to_owned(),
            created_at: "now".to_owned(),
            media_type: None,
            width: None,
            height: None,
            duration_ms: None,
            shown_sha256: None,
            pasted: None,
        }
    }

    #[test]
    fn save_changes_only_files_the_box_knew() {
        let old = vec![file("one"), file("two")];
        let current = vec![file("two"), file("three")];
        let ops = file_ops(&old, &current);
        assert_eq!(ops.remove, vec!["one"]);
        assert_eq!(ops.add.iter().map(|file| file.upload_id.as_str()).collect::<Vec<_>>(), vec!["three"]);
        assert!(ops.place.is_empty());
    }

    #[test]
    fn recipient_choice_targets_its_own_latest_message() -> Result<()> {
        use super::super::client::{Author, Message};
        let message = |id: &str, session: &str| Message {
            id: id.to_owned(),
            author: Author {
                kind: "agent".to_owned(),
                session: Some(session.to_owned()),
                ..Default::default()
            },
            ..Default::default()
        };
        let thread = Thread {
            messages: vec![message("a1", "a"), message("a2", "a"), message("b1", "b")],
            ..Default::default()
        };
        let agents = TaggedAgents { server_session: "s".to_owned(), sessions: Vec::new() };
        let route = route_reply(&thread, &["a".to_owned()], &agents)?;
        assert_eq!(route.message_id, "a2");
        assert!(route.to.is_none());
        assert!(route.tags.is_empty());
        Ok(())
    }

    #[test]
    fn permission_prompt_never_receives_words() {
        use super::super::client::{Author, Message};
        let thread = Thread {
            messages: vec![Message {
                id: "perm".to_owned(),
                author: Author {
                    kind: "agent".to_owned(),
                    session: Some("perm:one".to_owned()),
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        };
        let agents = TaggedAgents { server_session: "s".to_owned(), sessions: Vec::new() };
        assert!(route_reply(&thread, &[], &agents).is_err());
    }

    #[test]
    fn local_upload_rejects_oversize_nonfiles_and_secret_names() -> Result<()> {
        let root = std::env::temp_dir().join(format!(
            "plannotator-upload-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos()
        ));
        std::fs::create_dir(&root)?;
        let result = (|| -> Result<()> {
            let path = root.join("notes.txt");
            std::fs::write(&path, b"chosen bytes")?;
            let selected = SelectedUpload::read(&path)?;
            assert_eq!(selected.name, "notes.txt");
            assert_eq!(selected.bytes, b"chosen bytes");
            assert!(!selected.preview.contains(&root.to_string_lossy().to_string()));
            assert!(SelectedUpload::read(&root).is_err());
            File::create(root.join("large.bin"))?.set_len(UPLOAD_MAX_BYTES + 1)?;
            assert!(SelectedUpload::read(&root.join("large.bin")).is_err());
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(&path, root.join("alias.txt"))?;
                assert!(SelectedUpload::read(&root.join("alias.txt")).is_err());
            }
            Ok(())
        })();
        std::fs::remove_dir_all(&root)?;
        result?;
        for name in [".env", ".env.local", ".env::$DATA", "a\\b", "a\nb", "a\u{202e}b"] {
            assert!(validate_upload_name(name).is_err(), "{name:?}");
        }
        Ok(())
    }

    #[test]
    fn upload_response_must_confirm_exact_selected_file() -> Result<()> {
        let mut upload = file("upl_selected");
        upload.sha256 = "a".repeat(64);
        let raw = serde_json::to_value(upload)?;
        let upload = uploaded_file(json!({"upload":raw.clone()}), "upl_selected", 1, Some("run"))?;
        assert_eq!(upload.upload_id, "upl_selected");
        assert!(uploaded_file(json!({"upload":raw.clone()}), "wrong.txt", 1, Some("run")).is_err());
        assert!(uploaded_file(json!({"upload":raw.clone()}), "upl_selected", 2, Some("run")).is_err());
        assert!(
            uploaded_file(
                json!({"upload":raw.clone(),"serverSession":"other"}),
                "upl_selected",
                1,
                Some("run")
            )
            .is_err()
        );
        assert!(
            uploaded_file(json!({"upload":raw,"serverSession":"remote"}), "upl_selected", 1, None).is_ok()
        );
        assert!(uploaded_file(json!({"error":"disk full"}), "upl_selected", 1, Some("run")).is_err());
        Ok(())
    }

    #[test]
    fn uploaded_files_keep_ids_and_remove_only_selected_reference() -> Result<()> {
        let mut state = state();
        state.add_uploaded(file("upl_one"), "preview".to_owned())?;
        state.add_uploaded(file("upl_two"), "other".to_owned())?;
        state.held_files.clone_from(&state.files);
        state.remove_file("upl_one")?;
        let ops = file_ops(&state.held_files, state.files());
        assert_eq!(ops.remove, vec!["upl_one"]);
        assert_eq!(state.files().first().map(|file| file.upload_id.as_str()), Some("upl_two"));
        assert!(!state.file_previews.contains_key("upl_one"));
        state.claimed = true;
        assert!(state.remove_file("upl_two").is_err());
        assert!(state.add_uploaded(file("upl_three"), String::new()).is_err());
        Ok(())
    }

    #[test]
    fn file_previews_are_bounded_and_do_not_render_terminal_controls() {
        let preview = upload_preview(b"safe\x1b[31m red\x1b[0m\x07");
        assert!(preview.contains("safe red"));
        assert!(!preview.contains('\u{1b}'));
        let binary = upload_preview(&[0, 255, 27]);
        assert!(binary.contains("00 ff 1b"));
        assert!(upload_preview(&vec![b'x'; UPLOAD_PREVIEW_BYTES * 2]).len() < UPLOAD_PREVIEW_BYTES + 150);
    }

    #[test]
    fn upload_uses_binary_wire_and_reply_keeps_returned_upload_id() -> Result<()> {
        use std::io::{BufRead as _, BufReader, Read as _, Write as _};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let name = "report & ü.txt";
        let mut uploaded = file("upl_0123456789ABCDEF");
        uploaded.name = name.to_owned();
        uploaded.size = 3;
        uploaded.sha256 = "a".repeat(64);
        let replies = [
            json!({"serverSession":"run?&=1"}),
            json!({"upload":uploaded}),
            json!({"message_id":"msg_reply","questions":[],"reply":{"id":"msg_reply"},"replayed":false}),
        ];
        let server = std::thread::spawn(move || -> Result<Vec<(String, Vec<u8>)>> {
            let mut requests = Vec::new();
            for response in replies {
                let (mut stream, _) = listener.accept()?;
                stream.set_read_timeout(Some(Duration::from_secs(5)))?;
                let mut reader = BufReader::new(stream.try_clone()?);
                let mut headers = String::new();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    ensure!(reader.read_line(&mut line)? > 0, "Request ended before headers");
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((key, value)) = line.split_once(':')
                        && key.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse()?;
                    }
                    headers.push_str(&line);
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body)?;
                requests.push((headers, body));
                let body = response.to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )?;
            }
            Ok(requests)
        });
        let client = Client::from_url(&format!("http://{address}/"))?;
        let selected = SelectedUpload {
            name: name.to_owned(),
            bytes: b"a\0b".to_vec(),
            preview: String::new(),
            key: "stable-upload-key".to_owned(),
        };
        let upload = client.upload_reply_file("thr_1", None, &selected)?;
        let mut draft = state();
        draft.add_uploaded(upload, selected.preview)?;
        let uploads: Vec<String> = draft.files().iter().map(|file| file.upload_id.clone()).collect();
        client.reply_items(
            "msg_target",
            "reply-attempt",
            None,
            &super::super::actions::ReplyItems { uploads: &uploads, ..Default::default() },
        )?;
        let requests = server.join().map_err(|_| anyhow::anyhow!("HTTP fixture thread panicked"))??;
        let (headers, bytes) = requests.get(1).context("Missing upload request")?;
        let request_path = headers.split_whitespace().nth(1).context("Missing upload URL")?;
        let parsed = url::Url::parse(&format!("http://{address}{request_path}"))?;
        let params: BTreeMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(parsed.path(), "/api/inbox/uploads");
        assert_eq!(params.get("name").map(String::as_str), Some(name));
        assert_eq!(params.get("serverSession").map(String::as_str), Some("run?&=1"));
        assert_eq!(params.get("idempotency_key").map(String::as_str), Some("stable-upload-key"));
        let lower = headers.to_ascii_lowercase();
        assert!(lower.contains("content-type: application/octet-stream\r\n"));
        assert!(lower.contains("accept: application/json\r\n"));
        assert!(lower.contains(&format!("origin: http://{address}\r\n")));
        assert!(lower.contains("sec-fetch-site: same-origin\r\n"));
        assert_eq!(bytes, b"a\0b");
        let reply: Value = serde_json::from_slice(&requests.get(2).context("Missing reply request")?.1)?;
        assert_eq!(reply.get("uploads"), Some(&json!(["upl_0123456789ABCDEF"])));
        assert_eq!(reply.get("serverSession"), Some(&json!("run?&=1")));
        assert!(reply.get("path").is_none());
        assert!(validate_upload_name(".env.example").is_ok());
        assert!(validate_upload_name("prod.env").is_err());
        assert!(validate_upload_name(".envrc").is_err());
        Ok(())
    }

    fn serve_remote_upload_requests(
        listener: &std::net::TcpListener,
        replies: [(String, u16, Value); 5],
    ) -> Result<Vec<(String, Vec<u8>)>> {
        use std::io::{BufRead as _, BufReader, Read as _, Write as _};

        listener.set_nonblocking(true)?;
        let mut requests = Vec::new();
        for (expected, status, response) in replies {
            let start = Instant::now();
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && start.elapsed() < Duration::from_secs(5) =>
                    {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => return Err(error.into()),
                }
            };
            stream.set_nonblocking(false)?;
            stream.set_read_timeout(Some(Duration::from_secs(5)))?;
            let mut reader = BufReader::new(stream.try_clone()?);
            let mut headers = String::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                ensure!(reader.read_line(&mut line)? > 0, "request ended before headers");
                if line == "\r\n" {
                    break;
                }
                if let Some((key, value)) = line.split_once(':')
                    && key.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse()?;
                }
                headers.push_str(&line);
            }
            let path = headers.split_whitespace().nth(1).context("missing request path")?;
            ensure!(
                path.split('?').next() == Some(expected.as_str()),
                "unexpected request {path}; expected {expected}"
            );
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes)?;
            requests.push((headers, bytes));
            let response = response.to_string();
            write!(
                stream,
                "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                response.len()
            )?;
        }
        Ok(requests)
    }

    #[test]
    fn remote_upload_retries_exact_request_and_draft_never_touches_server_drafts() -> Result<()> {
        use std::net::TcpListener;
        const MACHINE: &str = "ibx_00000000000000000000000002";
        const THREAD: &str = "msg_00000000000000000000000001";
        const UPLOAD: &str = "upl_00000000000000000000000001";
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let mut upload = file(UPLOAD);
        upload.name = "remote.txt".to_owned();
        upload.size = 3;
        upload.sha256 = "a".repeat(64);
        let replies = [
            ("/api/inbox/health".to_owned(), 200, json!({"serverSession":"local-run"})),
            (
                "/api/inbox/threads".to_owned(),
                200,
                json!({"serverSession":"local-run","cursor":0,"sections":[{"threads":[{"thread_id":THREAD,"machine":MACHINE}]}]}),
            ),
            (
                format!("/api/inbox/m/{MACHINE}/uploads"),
                503,
                json!({"code":"machine_offline","error":"offline"}),
            ),
            (
                format!("/api/inbox/m/{MACHINE}/uploads"),
                200,
                json!({"serverSession":"far-run","upload":upload}),
            ),
            (
                format!("/api/inbox/m/{MACHINE}/threads/{THREAD}/taggable"),
                200,
                json!({"serverSession":"far-run","owner":null,"sessions":[]}),
            ),
        ];
        let server = std::thread::spawn(move || serve_remote_upload_requests(&listener, replies));
        let client = Client::from_url(&format!("http://{address}/"))?;
        client.list()?;
        let mut state = DraftState::load(&client, THREAD)?;
        assert!(state.is_remote());
        state.set_body("words stay here".to_owned())?;
        assert_eq!(state.save(&client)?, SaveOutcome::Memory);
        state.refresh(&client)?;
        state.use_saved();
        assert_eq!(state.body(), "words stay here");
        assert!(client.reply_draft(THREAD).is_err());
        assert!(client.discard_reply_draft(THREAD).is_err());
        let selected = SelectedUpload {
            name: "remote.txt".to_owned(),
            bytes: b"a\0b".to_vec(),
            preview: "preview".to_owned(),
            key: "stable-remote-key".to_owned(),
        };
        assert!(client.upload_reply_file(THREAD, Some(MACHINE), &selected).is_err());
        assert_eq!(state.body(), "words stay here");
        let uploaded = client.upload_reply_file(THREAD, Some(MACHINE), &selected)?;
        state.add_uploaded(uploaded, selected.preview)?;
        assert_eq!(state.save(&client)?, SaveOutcome::Memory);
        state.refresh(&client)?;
        let thread = Thread {
            thread_id: THREAD.to_owned(),
            messages: vec![super::super::client::Message {
                id: THREAD.to_owned(),
                author: super::super::client::Author {
                    kind: "agent".to_owned(),
                    session: Some("remote-agent".to_owned()),
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        };
        let prepared = state.prepare_send(&client, &thread)?;
        assert_eq!(prepared.uploads, vec![UPLOAD]);
        assert!(prepared.clear_draft.is_empty());
        assert!(state.is_claimed());
        state.release_send();
        state.discard(&client)?;
        assert!(state.body().is_empty() && state.files().is_empty());
        let requests = server.join().map_err(|_| anyhow::anyhow!("remote fixture thread panicked"))??;
        let first = requests.get(2).context("missing first upload")?;
        let retry = requests.get(3).context("missing retry")?;
        assert_eq!(first, retry);
        assert_eq!(retry.1, b"a\0b");
        let path = retry.0.split_whitespace().nth(1).context("missing retry path")?;
        let parsed = url::Url::parse(&format!("http://{address}{path}"))?;
        let params: BTreeMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(params.get("serverSession").map(String::as_str), Some("local-run"));
        assert_eq!(params.get("idempotency_key").map(String::as_str), Some("stable-remote-key"));
        assert_eq!(params.get("name").map(String::as_str), Some("remote.txt"));
        assert!(requests.iter().all(|(headers, _)| !headers.contains("/draft")));
        Ok(())
    }
}
