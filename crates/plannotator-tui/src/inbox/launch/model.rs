//! Wire models for the Inbox's launch engine. Missing capabilities are not invented.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use super::super::client::Message;

#[derive(Debug, Deserialize)]
pub(super) struct Sessions {
    #[serde(rename = "serverSession")]
    pub(super) server_session: String,
    pub(super) home: String,
    pub(super) projects: Vec<SessionProject>,
    pub(super) launch: Option<LaunchCatalog>,
}

#[derive(Debug, Deserialize)]
pub(super) struct SessionFolder {
    pub(super) name: String,
    pub(super) root: String,
}
#[derive(Debug, Deserialize)]
pub(super) struct SessionProject {
    pub(super) project: SessionFolder,
    pub(super) sessions: Vec<Session>,
}
#[derive(Debug, Deserialize)]
pub(super) struct Session {
    #[serde(rename = "session")]
    pub(super) id: String,
    pub(super) host: String,
    pub(super) label: Option<String>,
    pub(super) started_at: String,
    pub(super) busy: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(super) struct LaunchCatalog {
    pub(super) agents: Vec<Agent>,
    pub(super) projects: Vec<LaunchProject>,
    pub(super) scratch: Option<Scratch>,
    #[serde(default)]
    pub(super) not_ready: Vec<NotReady>,
    #[serde(default)]
    pub(super) drives: Vec<Drive>,
    #[serde(default)]
    pub(super) finder: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct Agent {
    #[serde(rename = "agent")]
    pub(super) id: String,
    pub(super) name: String,
    pub(super) scratch_trusted: Option<bool>,
    #[serde(default)]
    pub(super) untrusted: BTreeMap<String, Untrusted>,
}

#[derive(Debug, Deserialize)]
pub(super) struct Untrusted {
    pub(super) folder: String,
    pub(super) facts: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub(super) struct LaunchProject {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) root: String,
    pub(super) exists: bool,
    #[serde(default)]
    pub(super) slow: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct Scratch {
    pub(super) root: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct Drive {
    pub(super) path: String,
    pub(super) name: String,
    #[serde(default)]
    pub(super) network: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct NotReady {
    pub(super) name: String,
    pub(super) code: String,
    #[serde(default)]
    pub(super) over_ssh: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct Settings {
    #[serde(rename = "serverSession")]
    pub(super) server_session: String,
    pub(super) via: Option<String>,
    pub(super) guest: Option<bool>,
}

impl Settings {
    pub(super) fn guest(&self) -> bool {
        self.via.as_deref() == Some("tailnet") && self.guest != Some(false)
    }

    pub(super) fn local(&self) -> bool {
        self.via.as_deref() != Some("tailnet")
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct Machines {
    #[serde(default)]
    pub(super) machines: Vec<Machine>,
}

#[derive(Debug, Deserialize)]
pub(super) struct Machine {
    pub(super) id: String,
    pub(super) name: String,
    #[serde(rename = "self")]
    pub(super) is_self: bool,
    pub(super) state: String,
    pub(super) launch: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ThreadRead {
    #[serde(rename = "serverSession")]
    pub(super) server_session: String,
    pub(super) thread: LaunchThread,
    pub(super) run: Option<Run>,
    pub(super) attach_command: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct LaunchThread {
    pub(super) thread_id: String,
    pub(super) messages: Vec<LaunchMessage>,
}

#[derive(Debug, Deserialize)]
pub(super) struct LaunchMessage {
    #[serde(flatten)]
    pub(super) message: Message,
    pub(super) launch: Option<LaunchRecord>,
}

#[derive(Debug, Deserialize)]
pub(super) struct LaunchRecord {
    pub(super) id: String,
    pub(super) agent: String,
    pub(super) session_ref: Option<String>,
    pub(super) folder: String,
    pub(super) scratch: bool,
    pub(super) deleted_at: Option<String>,
    #[serde(default)]
    pub(super) full_permissions: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct Run {
    pub(super) state: String,
    pub(super) cause: Option<String>,
    pub(super) since: Option<String>,
    #[serde(default)]
    pub(super) stopped_by_person: bool,
    #[serde(flatten)]
    pub(super) trust_scope: TrustScope,
    pub(super) trust: Option<TrustWarning>,
    pub(super) attach_command: Option<String>,
    pub(super) made_folder: Option<String>,
    pub(super) clone: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub(super) struct TrustScope {
    #[serde(default)]
    pub(super) untrusted: bool,
    #[serde(default)]
    pub(super) home_folder: bool,
    #[serde(default)]
    pub(super) trust_covers_home: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct TrustWarning {
    pub(super) warning: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct Folders {
    pub(super) path: String,
    pub(super) exists: bool,
    pub(super) state: Option<String>,
    #[serde(rename = "folders")]
    pub(super) children: Vec<String>,
    #[serde(default)]
    pub(super) locked: Vec<String>,
    pub(super) writable: Option<bool>,
    pub(super) nearest: Option<Nearest>,
    pub(super) blocked: Option<Value>,
    pub(super) start: Option<bool>,
    #[serde(default)]
    pub(super) nostart: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct Nearest {
    pub(super) path: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct CloneTarget {
    pub(super) into: String,
    pub(super) path: String,
    pub(super) state: String,
    pub(super) taken: Option<String>,
    pub(super) writable: bool,
}

/// A confirmed new thread. A held remote write is never represented as a started thread.
#[derive(Debug, Deserialize)]
pub(crate) struct Started {
    pub(crate) thread_id: String,
}
