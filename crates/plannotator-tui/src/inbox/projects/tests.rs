#![allow(clippy::expect_used, clippy::panic, reason = "tests assert protocol invariants")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::thread;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use serde_json::{Value, json};

use super::api::{Effect, Mutation};
use super::model::{Answer, Project, checked_name, matches_name};
use super::{Modal, Projects};
use crate::inbox::client::Client;

fn project(id: &str, name: &str, listed: usize) -> Project {
    serde_json::from_value(json!({"id": id, "name": name, "root": format!("/work/{name}"),
        "threads": 2, "unread": 1, "listed": listed, "archived": 2 - listed}))
    .expect("project")
}

#[test]
fn archived_project_stays_only_while_held_or_explicitly_shown() {
    let mut pane = Projects {
        entries: vec![project("prj_a", "api", 1), project("prj_b", "docs", 0)],
        ..Projects::default()
    };
    pane.rebuild(None);
    assert_eq!(pane.visible.len(), 1);
    pane.set_filter(Some("prj_b".to_owned()));
    assert_eq!(pane.selected().map(|p| p.id.as_str()), Some("prj_b"));
    assert_eq!(pane.visible.len(), 2);
    pane.set_filter(None);
    assert_eq!(pane.visible.len(), 1);
    pane.show_archived = true;
    pane.query = "DOCS".to_owned();
    pane.rebuild(None);
    assert_eq!(pane.visible.len(), 1);
    assert_eq!(pane.selected().map(|p| p.id.as_str()), Some("prj_b"));
}

#[test]
fn rename_matches_nfc_names_and_checkout_folders_without_matching_custom_old_names() {
    let mut checkout = project("prj_b", "docs · work", 1);
    checkout.root = "/work/docs/.claude/worktrees/change".to_owned();
    let mut renamed = project("prj_c", "Manual", 1);
    renamed.root = "/other/docs".to_owned();
    renamed.display_name = Some("Manual".to_owned());
    let projects = vec![project("prj_a", "Café", 1), checkout, renamed];
    assert_eq!(
        matches_name("  CAFE\u{301}  ", "prj_x", &projects).first().map(|p| p.id.as_str()),
        Some("prj_a")
    );
    assert_eq!(matches_name("docs", "prj_x", &projects).len(), 1);
    assert_eq!(matches_name("docs", "prj_x", &projects).first().map(|p| p.id.as_str()), Some("prj_b"));
    assert!(matches_name("Café", "prj_a", &projects).is_empty());
    assert_eq!(checked_name("  Cafe\u{301}\t notes \n").expect("name"), "Café notes");
    assert!(checked_name(&"\u{1d400}".repeat(64)).is_ok());
    assert!(checked_name(&"\u{1d400}".repeat(65)).is_err());
    assert!(checked_name("bad\u{202e}name").is_err());
    assert!(checked_name("bad\u{0085}name").is_err());
}

#[test]
fn batch_undo_uses_exact_server_ids_and_preserves_session_even_on_partial_success() {
    let answer: Answer = serde_json::from_value(json!({"batch": {"read": ["msg_a"], "looked": ["msg_b"]},
        "error": {"code": "batch_incomplete", "message": "Stopped partway"}}))
    .expect("answer");
    let request = Mutation {
        project: "prj_a".to_owned(),
        label: "Mark read".to_owned(),
        session: "s1".to_owned(),
        route: "read",
        body: json!({"batch_id": "same-on-retry"}),
        effect: Effect::Batch { archive: false },
    };
    let undo = request.inverse(&answer).expect("undo partial batch");
    assert_eq!(undo.route, "unread");
    assert_eq!(undo.body, json!({"threads": ["msg_a"], "looked": ["msg_b"]}));
    assert_eq!(undo.session, "s1");
    assert_eq!(undo.project, "prj_a");
    let archive = Mutation { effect: Effect::Batch { archive: true }, ..request };
    let answer: Answer = serde_json::from_value(json!({"batch": {"read": ["msg_c"], "resolved": ["msg_d"]}}))
        .expect("archive answer");
    assert_eq!(
        archive.inverse(&answer).expect("archive undo").body,
        json!({"resolved": false, "threads": {"read": ["msg_c"], "resolved": ["msg_d"]}})
    );
}

#[test]
fn mark_read_rule_excludes_quiet_and_standing_reads_but_includes_returned_threads() {
    for (section, read, unseen, resolved, back, expected) in [
        ("quiet", false, 0, false, false, false),
        ("waiting", false, 0, false, false, true),
        ("new", false, 0, false, false, false),
        ("new", false, 1, false, false, true),
        ("stopped", true, 0, false, false, false),
        ("stopped", true, 1, false, false, true),
        ("waiting", false, 1, true, false, false),
        ("waiting", false, 1, true, true, true),
    ] {
        let row = serde_json::from_value(json!({"section": section, "unseen": unseen,
            "read_at": read.then_some("then"), "resolved_at": resolved.then_some("then"), "back_from_archive": back})).expect("row");
        assert_eq!(super::eligibility::readable(&row), expected, "{section} read={read} unseen={unseen}");
    }
}

struct Step {
    method: &'static str,
    path: &'static str,
    status: u16,
    response: Value,
}

fn server(steps: Vec<Step>) -> (Client, thread::JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}/", listener.local_addr().expect("address"));
    let worker = thread::spawn(move || {
        let mut bodies = Vec::new();
        let health = Step {
            method: "GET",
            path: "/api/inbox/health",
            status: 200,
            response: json!({"serverSession": "s1"}),
        };
        for step in std::iter::once(health).chain(steps) {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(&mut stream);
            let mut line = String::new();
            reader.read_line(&mut line).expect("request");
            assert!(line.starts_with(&format!("{} {} ", step.method, step.path)), "{line}");
            let mut len = 0;
            let mut origin = false;
            loop {
                line.clear();
                reader.read_line(&mut line).expect("header");
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = value.trim().parse().expect("length");
                }
                origin |= line.to_ascii_lowercase().starts_with("origin:");
            }
            assert!(origin, "same-origin guard");
            let mut body = vec![0; len];
            reader.read_exact(&mut body).expect("body");
            if len > 0 {
                bodies.push(serde_json::from_slice(&body).expect("JSON body"));
            }
            drop(reader);
            let text = step.response.to_string();
            write!(stream, "HTTP/1.1 {} Result\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", step.status, text.len(), text).expect("respond");
        }
        bodies
    });
    (Client::from_url(&url).expect("connect"), worker)
}

fn projects(listed: usize) -> Step {
    Step {
        method: "GET",
        path: "/api/inbox/projects",
        status: 200,
        response: json!({"serverSession": "s1", "projects": [{"id": "prj_a", "name": "api", "root": "/work/api", "listed": listed}]}),
    }
}

fn threads() -> Step {
    Step {
        method: "GET",
        path: "/api/inbox/threads",
        status: 200,
        response: json!({"serverSession": "s1", "sections": []}),
    }
}

#[test]
fn opening_searching_and_rendering_never_write() {
    let (client, worker) = server(vec![projects(1), threads()]);
    let mut pane = Projects::default();
    pane.refresh(&client).expect("read projects");
    pane.handle_key(KeyEvent::from(KeyCode::Char('/')), &client).expect("search");
    pane.paste("api");
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
    terminal.draw(|frame| pane.draw(frame, frame.area())).expect("draw");
    assert_eq!(pane.visible.len(), 1);
    assert!(worker.join().expect("server").is_empty());
}

#[test]
fn archive_needs_confirmation_and_failed_request_reuses_batch_id_with_partial_undo() {
    let (client, worker) = server(vec![
        projects(1),
        threads(),
        Step {
            method: "POST",
            path: "/api/inbox/projects/prj_a/resolve",
            status: 503,
            response: json!({"error": "disk busy", "code": "busy"}),
        },
        Step {
            method: "POST",
            path: "/api/inbox/projects/prj_a/resolve",
            status: 200,
            response: json!({"batch": {"resolved": ["msg_a"], "read": ["msg_b"]}, "error": {"code": "batch_incomplete", "message": "Stopped partway"}}),
        },
        projects(0),
        threads(),
    ]);
    let mut pane = Projects::default();
    pane.refresh(&client).expect("read");
    pane.handle_key(KeyEvent::from(KeyCode::Char('A')), &client).expect("ask");
    assert!(pane.pending.is_none());
    pane.handle_key(KeyEvent::from(KeyCode::Enter), &client).expect("enter is not confirmation");
    assert!(pane.pending.is_none());
    let error = pane.handle_key(KeyEvent::from(KeyCode::Char('y')), &client).expect_err("failure");
    assert!(format!("{error:#}").contains("disk busy"));
    assert!(pane.pending.is_some());
    assert!(!matches!(pane.modal, Some(Modal::Discard)));
    pane.handle_key(KeyEvent::from(KeyCode::Char('!')), &client).expect("retry same request");
    assert!(pane.pending.is_none());
    assert!(pane.undo.is_some());
    assert!(pane.status.as_deref().is_some_and(|status| status.contains("batch_incomplete")));
    let requests = worker.join().expect("server");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests.first(), requests.last());
    assert_eq!(requests.first().and_then(|body| body.get("serverSession")), Some(&json!("s1")));
}

#[test]
fn permanent_delete_requires_exact_id_and_never_offers_false_undo() {
    let (client, worker) = server(vec![
        projects(1),
        threads(),
        Step {
            method: "POST",
            path: "/api/inbox/projects/prj_a/delete",
            status: 200,
            response: json!({"ok": true}),
        },
        projects(0),
        threads(),
    ]);
    let mut pane = Projects::default();
    pane.refresh(&client).expect("read");
    pane.handle_key(KeyEvent::from(KeyCode::Char('D')), &client).expect("ask delete");
    assert!(pane.handle_key(KeyEvent::from(KeyCode::Enter), &client).is_err());
    assert!(pane.pending.is_none());
    pane.paste("prj_a");
    pane.handle_key(KeyEvent::from(KeyCode::Enter), &client).expect("typed confirmation");
    assert!(pane.undo.is_none());
    assert!(pane.status.as_deref().is_some_and(|status| status.contains("cannot be undone")));
    assert_eq!(worker.join().expect("server").len(), 1);
}
