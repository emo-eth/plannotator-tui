use std::collections::{BTreeMap, HashSet};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

use super::client::{Client, Row};

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct AgentGroup {
    pub(crate) machine: Option<String>,
    pub(crate) agents: Vec<Agent>,
    pub(crate) total: u64,
    pub(crate) ended_today: u64,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct AgentIcon {
    pub(crate) id: String,
    #[serde(rename = "type")]
    pub(crate) icon_type: String,
    pub(crate) at: u64,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct AgentFacesWire {
    pub(crate) faces: BTreeMap<String, AgentFaceEntry>,
    pub(crate) rev: u64,
    pub(crate) icons: Option<Vec<AgentIcon>>,
}

impl AgentFacesWire {
    pub(crate) fn face_for_agent(&self, agent: &Agent) -> Option<&str> {
        let direct = std::iter::once(agent.id.as_str())
            .chain(std::iter::once(agent.session.as_str()))
            .chain(agent.continues.iter().map(String::as_str))
            .find_map(|id| self.faces.get(id).map(|entry| entry.face.as_str()));
        direct.or_else(|| {
            self.faces
                .values()
                .find(|entry| {
                    std::iter::once(agent.id.as_str())
                        .chain(std::iter::once(agent.session.as_str()))
                        .chain(agent.continues.iter().map(String::as_str))
                        .any(|id| entry.sessions.iter().any(|session| session == id))
                })
                .map(|entry| entry.face.as_str())
        })
    }
}
pub(crate) fn parse_agent_groups(raw: &[Value]) -> Result<Vec<AgentGroup>> {
    serde_json::from_value(Value::Array(raw.to_vec())).context("parsing Inbox agent groups")
}

impl Client {
    pub(crate) fn agent_faces(&self) -> Result<AgentFacesWire> {
        self.get_json("GET", "/api/inbox/agent-faces")
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum AgentState {
    Waiting,
    Working,
    Idle,
    Ended,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct AgentProject {
    pub(crate) id: Option<String>,
    pub(crate) name: String,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct AgentLaunch {
    pub(crate) id: String,
    pub(crate) thread_id: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct Agent {
    pub(crate) id: String,
    pub(crate) session: String,
    #[serde(default)]
    pub(crate) continues: Vec<String>,
    pub(crate) launch: Option<AgentLaunch>,
    pub(crate) host: Option<String>,
    pub(crate) label: Option<String>,
    pub(crate) project: Option<AgentProject>,
    pub(crate) state: AgentState,
    #[serde(default)]
    pub(crate) waiting: u64,
    #[serde(default)]
    pub(crate) live: bool,
    pub(crate) started_at: Option<String>,
    pub(crate) last_seen_at: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct AgentFaceEntry {
    pub(crate) face: String,
    pub(crate) sessions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentThread {
    pub(crate) id: String,
    pub(crate) subject: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentEntry {
    pub(crate) agent: Agent,
    pub(crate) machine: Option<String>,
    pub(crate) name: String,
    pub(crate) thread: Option<AgentThread>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentDisplayGroup {
    pub(crate) machine: Option<String>,
    pub(crate) agents: Vec<AgentEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MachineFilter {
    Any,
    Host(Option<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentListFilter {
    pub(crate) id: String,
    pub(crate) name: String,
    machine: MachineFilter,
    pub(crate) sessions: Vec<String>,
    pub(crate) launches: Vec<String>,
}

pub(crate) fn agent_host_name(host: Option<&str>) -> &str {
    match host {
        Some("claude-code" | "claude") => "Claude Code",
        Some("pi") => "Pi",
        Some("opencode") => "OpenCode",
        Some("codex") => "Codex",
        Some("cursor") => "Cursor",
        Some("grok") => "Grok Build",
        Some("qwen") => "Qwen Code",
        Some("dsh") => "DeepSeek Harness",
        Some("t3") => "T3 Code",
        Some("cline") => "Cline",
        Some(host) => host,
        None => "An agent",
    }
}

fn agent_sessions(agent: &Agent, others: Option<&HashSet<String>>) -> Vec<String> {
    let mut sessions = vec![agent.session.clone()];
    sessions
        .extend(agent.continues.iter().filter(|old| !others.is_some_and(|set| set.contains(*old))).cloned());
    sessions
}

fn row_of(row: &Row, sessions: &[String]) -> bool {
    (row.author.kind == "agent"
        && row
            .author
            .session
            .as_deref()
            .is_some_and(|session| sessions.iter().any(|candidate| candidate == session)))
        || row
            .addressee
            .as_ref()
            .is_some_and(|addressee| sessions.iter().any(|session| session == &addressee.session))
        || row
            .participants
            .iter()
            .any(|participant| sessions.iter().any(|session| session == &participant.session))
}

fn row_name(row: &Row) -> String {
    row.thread_name.as_deref().or(row.subject.as_deref()).unwrap_or_default().trim().to_owned()
}

fn agent_thread(
    agent: &Agent,
    machine: Option<&str>,
    self_machine: Option<&str>,
    rows: &[Row],
    others: &HashSet<String>,
) -> Option<AgentThread> {
    let row_is_mine = |row: &Row| row.machine.as_deref().or(self_machine) == machine.or(self_machine);
    if let Some(launch) = &agent.launch {
        let subject = rows
            .iter()
            .find(|row| row.thread_id == launch.thread_id && row_is_mine(row))
            .map(row_name)
            .unwrap_or_default();
        return Some(AgentThread { id: launch.thread_id.clone(), subject });
    }
    let sessions = agent_sessions(agent, Some(others));
    let mut best: Option<&Row> = None;
    for row in rows.iter().filter(|row| row_is_mine(row) && row_of(row, &sessions)) {
        let Some(current) = best else {
            best = Some(row);
            continue;
        };
        if row.waiting_on_person != current.waiting_on_person {
            if row.waiting_on_person {
                best = Some(row);
            }
        } else if row.last_agent_at.as_deref().unwrap_or(&row.last_at)
            > current.last_agent_at.as_deref().unwrap_or(&current.last_at)
        {
            best = Some(row);
        }
    }
    best.map(|row| AgentThread { id: row.thread_id.clone(), subject: row_name(row) })
}

fn agent_row_name(agent: &Agent, thread: Option<&AgentThread>) -> String {
    agent
        .label
        .as_deref()
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .or_else(|| thread.map(|thread| thread.subject.as_str()).filter(|subject| !subject.is_empty()))
        .or_else(|| {
            agent.project.as_ref().map(|project| project.name.as_str()).filter(|name| !name.is_empty())
        })
        .unwrap_or_else(|| agent_host_name(agent.host.as_deref()))
        .to_owned()
}

fn drawn_as(older: &AgentEntry, later: &AgentEntry) -> bool {
    later.agent.continues.iter().any(|session| session == &older.agent.session)
        && !older.agent.continues.iter().any(|session| session == &later.agent.session)
        && older.thread.as_ref().is_some_and(|thread| {
            later.thread.as_ref().is_some_and(|later_thread| thread.id == later_thread.id)
        })
}

pub(crate) fn agent_groups(
    groups: &[AgentGroup],
    rows: &[Row],
    self_machine: Option<&str>,
) -> Vec<AgentDisplayGroup> {
    groups
        .iter()
        .map(|group| {
            let listed: Vec<Agent> =
                group.agents.iter().filter(|agent| agent.state != AgentState::Ended).cloned().collect();
            let others: HashSet<String> = listed.iter().map(|agent| agent.session.clone()).collect();
            let all: Vec<AgentEntry> = listed
                .into_iter()
                .map(|agent| {
                    let thread = agent_thread(
                        &agent,
                        group.machine.as_deref().or(self_machine),
                        self_machine,
                        rows,
                        &others,
                    );
                    let name = agent_row_name(&agent, thread.as_ref());
                    AgentEntry { agent, machine: group.machine.clone(), name, thread }
                })
                .collect();
            let agents =
                all.iter().filter(|entry| !all.iter().any(|later| drawn_as(entry, later))).cloned().collect();
            AgentDisplayGroup { machine: group.machine.clone(), agents }
        })
        .collect()
}
pub(crate) fn agent_list_filter(
    id: &str,
    groups: &[AgentGroup],
    rows: &[Row],
    self_machine: Option<&str>,
) -> AgentListFilter {
    for group in groups {
        let Some(agent) = group.agents.iter().find(|agent| agent.id == id) else { continue };
        let machine = group.machine.as_deref().or(self_machine);
        let listed: Vec<Agent> =
            group.agents.iter().filter(|listed| listed.state != AgentState::Ended).cloned().collect();
        let others: HashSet<String> = listed.iter().map(|listed| listed.session.clone()).collect();
        let all: Vec<AgentEntry> = listed
            .iter()
            .map(|listed| {
                let thread = agent_thread(listed, machine, self_machine, rows, &others);
                AgentEntry {
                    agent: listed.clone(),
                    machine: machine.map(str::to_owned),
                    name: agent_row_name(listed, thread.as_ref()),
                    thread,
                }
            })
            .collect();
        let shown =
            all.iter().find(|entry| entry.agent.id == id && !all.iter().any(|later| drawn_as(entry, later)));
        let selected = if let Some(shown) = shown {
            shown.clone()
        } else {
            let thread = agent_thread(agent, machine, self_machine, rows, &others);
            AgentEntry {
                agent: agent.clone(),
                machine: machine.map(str::to_owned),
                name: agent_row_name(agent, thread.as_ref()),
                thread,
            }
        };
        let lineage: Vec<&AgentEntry> = if shown.is_some() {
            all.iter().filter(|entry| entry.agent.id == id || drawn_as(entry, &selected)).collect()
        } else {
            vec![&selected]
        };
        let mut sessions = Vec::new();
        let mut launches = Vec::new();
        for entry in lineage {
            for session in agent_sessions(&entry.agent, Some(&others)) {
                if !sessions.contains(&session) {
                    sessions.push(session);
                }
            }
            if let Some(launch) = &entry.agent.launch {
                launches.push(launch.thread_id.clone());
            }
        }
        return AgentListFilter {
            id: id.to_owned(),
            name: selected.name,
            machine: MachineFilter::Host(group.machine.clone()),
            sessions,
            launches,
        };
    }
    AgentListFilter {
        id: id.to_owned(),
        name: agent_host_name(None).to_owned(),
        machine: MachineFilter::Any,
        sessions: vec![id.to_owned()],
        launches: Vec::new(),
    }
}

pub(crate) fn agent_row_matches(row: &Row, filter: &AgentListFilter, self_machine: Option<&str>) -> bool {
    if let MachineFilter::Host(machine) = &filter.machine
        && row.machine.as_deref().or(self_machine) != machine.as_deref().or(self_machine)
    {
        return false;
    }
    filter.launches.iter().any(|launch| launch == &row.thread_id) || row_of(row, &filter.sessions)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing, reason = "tests assert by panicking")]
mod tests {
    use super::{
        AgentFaceEntry, AgentFacesWire, AgentGroup, AgentState, agent_groups, agent_host_name,
        agent_list_filter, agent_row_matches,
    };
    use crate::inbox::client::Row;

    fn agent(id: &str, state: &str, session: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "session": session,
            "host": "claude-code",
            "project": { "id": "prj_bill", "name": "billing-svc" },
            "state": state,
            "waiting": 0,
            "live": true,
            "started_at": "2026-11-09T09:14:00.000Z",
            "last_seen_at": "2026-11-09T09:46:00.000Z"
        })
    }

    fn row(id: &str, session: &str, waiting: bool, last_agent_at: &str) -> Row {
        serde_json::from_value(serde_json::json!({
            "thread_id": id,
            "subject": format!("Subject {id}"),
            "thread_name": null,
            "project": { "id": "prj_bill", "name": "billing-svc" },
            "section": "waiting",
            "author": { "kind": "agent", "host": "claude-code", "session": session },
            "last_at": last_agent_at,
            "last_agent_at": last_agent_at,
            "waiting_on_person": waiting,
            "participants": [{ "session": session }]
        }))
        .expect("row wire")
    }

    #[test]
    fn parses_agent_groups_from_the_recorded_upstream_list_fixture() {
        let list: serde_json::Value =
            serde_json::from_str(include_str!("fixtures/list.json")).expect("list fixture");
        let raw = list["agents"].as_array().expect("agents in list fixture");
        let groups = super::parse_agent_groups(raw).expect("agent groups in list fixture");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].agents[0].session, "session-fixture");
        assert_eq!(groups[0].agents[0].state, AgentState::Waiting);
        assert_eq!(groups[0].agents[0].waiting, 1);
    }

    #[test]
    fn reads_agent_faces_and_uploaded_icon_wire_fields() {
        let faces: AgentFacesWire = serde_json::from_value(serde_json::json!({
            "faces": { "ses_a": { "face": "set:animals/cat", "sessions": ["ses_old"] } },
            "rev": 3,
            "icons": [{ "id": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef", "type": "image/png", "at": 1_730_000_000_000_i64 }]
        })).expect("agent faces wire");
        assert_eq!(faces.rev, 3);
        assert_eq!(
            faces.faces["ses_a"],
            AgentFaceEntry { face: "set:animals/cat".to_owned(), sessions: vec!["ses_old".to_owned()] }
        );
        assert_eq!(
            faces.icons.as_ref().and_then(|icons| icons.first()).map(|icon| icon.icon_type.as_str()),
            Some("image/png")
        );
        let current: super::Agent =
            serde_json::from_value(agent("ses_a", "idle", "ses_a")).expect("agent wire");
        assert_eq!(faces.face_for_agent(&current), Some("set:animals/cat"));
    }

    #[test]
    fn agent_rows_choose_waiting_thread_then_name_and_filter_on_its_sessions() {
        let groups: Vec<AgentGroup> = serde_json::from_value(serde_json::json!([{
            "agents": [agent("ses_a", "idle", "ses_a")], "total": 1, "ended_today": 0
        }]))
        .expect("agents wire");
        let rows = [
            row("msg_recent", "ses_a", false, "2026-11-09T12:00:00.000Z"),
            row("msg_waiting", "ses_a", true, "2026-11-09T10:00:00.000Z"),
        ];
        let display = agent_groups(&groups, &rows, None);
        assert_eq!(
            display[0].agents[0].thread.as_ref().map(|thread| thread.id.as_str()),
            Some("msg_waiting")
        );
        assert_eq!(display[0].agents[0].name, "Subject msg_waiting");
        let filter = agent_list_filter("ses_a", &groups, &rows, None);
        assert!(agent_row_matches(&rows[0], &filter, None));
        assert!(agent_row_matches(&rows[1], &filter, None));
    }

    #[test]
    fn agent_filter_matches_addressed_threads_only_on_their_machine() {
        let groups: Vec<AgentGroup> = serde_json::from_value(serde_json::json!([{
            "machine": "ibx_home",
            "agents": [agent("ses_a", "idle", "ses_a")], "total": 1, "ended_today": 0
        }]))
        .expect("agents wire");
        let addressed: Row = serde_json::from_value(serde_json::json!({
            "thread_id": "msg_addressed", "project": { "id": "prj_bill", "name": "billing-svc" },
            "section": "waiting", "author": { "kind": "person" }, "last_at": "2026-11-09T12:00:00.000Z",
            "addressee": { "session": "ses_a" }, "machine": "ibx_home"
        }))
        .expect("addressed row wire");
        let other_machine: Row = serde_json::from_value(serde_json::json!({
            "thread_id": "msg_elsewhere", "project": { "id": "prj_bill", "name": "billing-svc" },
            "section": "waiting", "author": { "kind": "agent", "session": "ses_a" }, "last_at": "2026-11-09T12:00:00.000Z",
            "machine": "ibx_other"
        })).expect("merged row wire");
        let filter = agent_list_filter(
            "ses_a",
            &groups,
            &[addressed.clone(), other_machine.clone()],
            Some("ibx_home"),
        );
        assert!(agent_row_matches(&addressed, &filter, Some("ibx_home")));
        assert!(!agent_row_matches(&other_machine, &filter, Some("ibx_home")));
    }

    #[test]
    fn upstream_host_names_and_continued_sessions_are_preserved() {
        assert_eq!(agent_host_name(Some("qwen")), "Qwen Code");
        assert_eq!(agent_host_name(Some("other")), "other");
        let mut raw = agent("ses_new", "idle", "ses_new");
        raw["continues"] = serde_json::json!(["ses_old"]);
        let groups: Vec<AgentGroup> = serde_json::from_value(serde_json::json!([{
            "agents": [raw], "total": 1, "ended_today": 0
        }]))
        .expect("agents wire");
        let rows = [row("msg_old", "ses_old", false, "2026-11-09T12:00:00.000Z")];
        let filter = agent_list_filter("ses_new", &groups, &rows, None);
        assert!(filter.sessions.iter().any(|session| session == "ses_old"));
        assert!(agent_row_matches(&rows[0], &filter, None));
    }
}
