use anyhow::{Result, ensure};
use serde::Deserialize;
use unicode_normalization::UnicodeNormalization;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Project {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) root: String,
    pub(crate) display_name: Option<String>,
    pub(crate) threads: usize,
    pub(crate) unread: usize,
    pub(crate) listed: Option<usize>,
    pub(crate) archived: usize,
    pub(crate) combined: Vec<Member>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct Member {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) root: String,
}

#[derive(Deserialize)]
pub(super) struct ProjectList {
    #[serde(rename = "serverSession")]
    pub(super) server_session: String,
    pub(super) projects: Vec<Project>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub(super) struct Batch {
    pub(super) read: Vec<String>,
    pub(super) looked: Vec<String>,
    pub(super) resolved: Vec<String>,
}

impl Batch {
    pub(super) fn len(&self) -> usize {
        self.read.len() + self.looked.len() + self.resolved.len()
    }
}

#[derive(Deserialize)]
pub(super) struct BatchError {
    pub(super) code: String,
    pub(super) message: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub(super) struct Answer {
    #[serde(rename = "serverSession")]
    pub(super) server_session: Option<String>,
    pub(super) ok: Option<bool>,
    pub(super) project: Option<Project>,
    pub(super) batch: Batch,
    pub(super) skipped: Vec<String>,
    pub(super) error: Option<BatchError>,
}

pub(super) fn name_key(name: &str) -> String {
    name.nfc().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// The server remains the authority for invisible-only names and all wire validation.
pub(super) fn checked_name(value: &str) -> Result<String> {
    ensure!(
        !value
            .chars()
            .any(|c| (c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
                || matches!(c, '\u{2028}' | '\u{2029}')),
        "A project name must not contain control characters."
    );
    ensure!(
        !value.chars().any(|c| matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}'
            | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')),
        "A project name must not contain bidirectional controls."
    );
    let name = value.nfc().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ");
    ensure!(!name.is_empty(), "A project name cannot be empty.");
    ensure!(name.chars().count() <= 64, "A project name can be at most 64 characters.");
    Ok(name)
}

/// Match upstream labelPath for checkout folders on Unix and Windows, without local I/O.
fn folder_name(root: &str) -> Option<&str> {
    let parts: Vec<_> = root.split(['/', '\\']).filter(|part| !part.is_empty()).collect();
    let last = parts.last().copied();
    let parent = parts.len().checked_sub(2).and_then(|i| parts.get(i));
    if parts.len() < 3 || !parent.is_some_and(|p| matches!(name_key(p).as_str(), "worktrees" | ".worktrees"))
    {
        return last;
    }
    let repo = parts.iter().take(parts.len() - 2).rposition(|p| !p.starts_with('.'));
    let Some(repo) = repo else {
        return last;
    };
    let home = repo.checked_sub(1).and_then(|i| parts.get(i)).is_some_and(|p| {
        matches!(name_key(p).as_str(), "users" | "home")
            && (repo == 1 || (repo == 2 && parts.first().is_some_and(|p| p.ends_with(':'))))
    });
    if home { last } else { parts.get(repo).copied() }
}

pub(super) fn matches_name<'a>(typed: &str, source: &str, projects: &'a [Project]) -> Vec<&'a Project> {
    let key = name_key(typed);
    let named: Vec<_> = projects.iter().filter(|p| p.id != source && name_key(&p.name) == key).collect();
    if !named.is_empty() {
        return named;
    }
    projects
        .iter()
        .filter(|p| {
            p.id != source
                && p.display_name.is_none()
                && folder_name(&p.root).is_some_and(|folder| name_key(folder) == key)
        })
        .collect()
}
