#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic, reason = "tests assert invariants")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;
use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Value, json};

use super::api::Delivery;
use super::forms::{Form, FormKind, Modal, Submission};
use super::model::{Card, Column, Initiative, Steps, Target, initiative_of, split_refs};
use super::*;

fn card(id: &str, title: &str, column: Column) -> Card {
    Card {
        id: id.to_owned(),
        title: title.to_owned(),
        column,
        project_id: "prj_one".to_owned(),
        ..Card::default()
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn filters_intersect_and_selection_survives_reorder() {
    let mut board = Board::default();
    let mut matching = card("crd_match", "Ship widgets", Column::InProgress);
    matching.initiative = Some("ini_one".to_owned());
    matching.notes = Some("keyboard accessibility".to_owned());
    board.model.cards = vec![card("crd_other", "Other", Column::Done), matching];
    board.model.initiatives =
        vec![Initiative { id: "ini_one".into(), name: "Widgets".into(), ..Initiative::default() }];
    board.column = Some(Column::InProgress);
    board.project = Some("prj_one".into());
    board.initiative = Some("ini_one".into());
    board.query = "ACCESSIBILITY".into();
    board.rebuild(None);
    assert_eq!(board.visible, [1]);
    board.model.cards.swap(0, 1);
    board.rebuild(Some("crd_match"));
    assert_eq!(board.selected().expect("selected").id, "crd_match");
    board.project = Some("prj_missing".into());
    board.rebuild(None);
    assert!(board.selected().is_none());
}

#[test]
fn deleted_initiative_becomes_unassigned_but_history_keeps_its_name() {
    let mut card = card("crd_one", "Title", Column::Backlog);
    card.initiative = Some("ini_one".into());
    let initiatives = vec![Initiative {
        id: "ini_one".into(),
        name: "Old effort".into(),
        deleted_at: Some("yesterday".into()),
    }];
    assert_eq!(initiative_of(&card, &initiatives), None);
    let text = detail::history_line(
        &json!({"kind":"initiative", "initiative":"ini_one", "by":{"kind":"person"}}),
        &initiatives,
    );
    assert!(text.contains("Old effort"));
}

#[test]
fn references_preserve_url_commas_and_split_spreadsheet_cells() {
    assert_eq!(
        split_refs("a/b#1,a/b#2 https://map.test/@37,-122,https://x.test/?f=a,b, HTTPS://z.test, "),
        ["a/b#1", "a/b#2", "https://map.test/@37,-122", "https://x.test/?f=a,b", "HTTPS://z.test"]
    );
}

#[test]
fn checklist_excludes_cancelled_steps_and_preserves_ended_session_time() {
    let steps: Steps = serde_json::from_value(json!({
        "agent":{"kind":"agent","name":"Ada"}, "presence":"ended", "updated_at":"2026-11-11T12:00:00Z",
        "steps":[{"step":"Plan","status":"done","at":"12:00"}, {"step":"Build","status":"waiting"}, {"step":"Old work","status":"dropped"}]
    })).expect("steps wire");
    assert_eq!(steps.progress(), (1, 2));
    let mut board = Board::default();
    let mut card = card("crd_one", "Title", Column::InProgress);
    card.threads.push("msg_one".into());
    board.opened = Some(CardDetail { card, history: vec![] });
    board.model.steps.insert("msg_one".into(), vec![steps]);
    let text = board.detail_lines().iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    assert!(text.contains("1/2"));
    assert!(text.contains("as of 2026-11-11T12:00:00Z"));
    assert!(text.contains("Was waiting (session ended)"));
    assert!(!text.contains("Waiting on you"));
}

#[test]
fn unread_is_only_agent_writes_after_seen_cursor() {
    let mut card = card("crd_one", "Title", Column::Todo);
    card.heard_seq = 10;
    assert!(!card.unread(None));
    card.heard_by.kind = "agent".into();
    assert!(card.unread(None));
    assert!(card.unread(Some(9)));
    assert!(!card.unread(Some(10)));
}

#[test]
fn create_form_retains_idempotency_and_notes_can_be_cleared() {
    let projects = vec![Project { id: "prj_one".into(), name: "Widgets".into(), root: "/widgets".into() }];
    let mut form = Form::create(&projects, &[], None, None);
    form.paste("New work");
    let Submission::Post { body: first, .. } = form.submit().expect("new card") else {
        panic!("POST expected")
    };
    let Submission::Post { body: second, .. } = form.submit().expect("retry") else {
        panic!("POST expected")
    };
    assert_eq!(first["idempotency_key"], second["idempotency_key"]);
    assert_eq!(first["project"], "prj_one");
    assert_eq!(first["column"], "backlog");
    let form = Form::single("Notes", FormKind::Notes("crd_one".into()), "Notes", "");
    let Submission::Post { body, .. } = form.submit().expect("empty clears notes") else {
        panic!("POST expected")
    };
    assert_eq!(body, json!({"op":"edit", "notes":""}));
}

#[test]
fn snapshot_refresh_and_render_only_issue_gets() {
    let (client, calls, server) = server(vec![
        (200, health()),
        (
            200,
            json!({
                "serverSession":"test", "cursor":7, "cards":[{"id":"crd_one","project_id":"prj_one","title":"Keyboard work","column":"in_review","you":"Review changes"}],
                "initiatives":[],"steps":{},"dropped_unseen":0
            }),
        ),
        (200, projects()),
    ]);
    let mut board = Board::default();
    board.refresh(&client).expect("read board");
    let mut terminal = Terminal::new(TestBackend::new(80, 20)).expect("terminal");
    terminal.draw(|frame| board.draw(frame, frame.area())).expect("draw");
    let text: String =
        terminal.backend().buffer().content.iter().map(ratatui::buffer::Cell::symbol).collect();
    assert!(text.contains("Keyboard work"));
    server.join().expect("server");
    assert!(calls.try_iter().all(|call| call.method == "GET"));
}

#[test]
fn failed_notes_save_keeps_the_editor_text() {
    let (client, calls, server) = server(vec![(200, health()), (409, json!({"error":"conflict"}))]);
    let mut board = Board {
        modal: Some(Modal::Form(Form::single(
            "Notes",
            FormKind::Notes("crd_one".into()),
            "Notes",
            "Never lose these words",
        ))),
        ..Board::default()
    };
    assert!(board.handle_key(key(KeyCode::Enter), &client).is_err());
    let Some(Modal::Form(form)) = &board.modal else { panic!("editor retained") };
    assert_eq!(form.fields[0].value(), "Never lose these words");
    server.join().expect("server");
    let calls: Vec<_> = calls.try_iter().collect();
    assert_eq!(calls[1].path, "/api/inbox/cards/crd_one");
    assert_eq!(calls[1].body["serverSession"], "test");
}

#[test]
fn cancellation_needs_confirmation_and_sends_reason() {
    let (client, calls, server) = server(vec![
        (200, health()),
        (200, json!({"card":{"id":"crd_one","column":"dropped"}})),
        (200, empty_cards()),
        (200, projects()),
    ]);
    let mut board = Board::default();
    board.model.cards.push(card("crd_one", "Work", Column::Todo));
    board.rebuild(None);
    board.handle_key(key(KeyCode::Char('d')), &client).expect("reason field");
    board.paste("No longer needed");
    board.handle_key(key(KeyCode::Enter), &client).expect("confirmation");
    assert!(matches!(board.modal, Some(Modal::Confirm { .. })));
    assert_eq!(calls.try_iter().count(), 1, "only health read before confirming");
    board.handle_key(key(KeyCode::Char('y')), &client).expect("confirmed");
    server.join().expect("server");
    let calls: Vec<_> = calls.try_iter().collect();
    assert_eq!(calls[0].method, "POST");
    assert_eq!(calls[0].body["op"], "drop");
    assert_eq!(calls[0].body["reason"], "No longer needed");
}

#[test]
fn delivery_retries_card_link_without_resending_confirmed_agent_message() {
    let (client, calls, server) = server(vec![
        (200, health()),
        (200, json!({"message":{"id":"msg_new"},"thread_id":"msg_new"})),
        (409, json!({"error":"temporary refusal"})),
        (200, json!({"card":{"id":"crd_one"}})),
    ]);
    let card = card("crd_one", "Work", Column::Todo);
    let target = Target {
        host: "codex".into(),
        session: "session_one".into(),
        label: "Codex".into(),
        project: "prj_one".into(),
    };
    let mut delivery = Delivery::new(&card, target, true);
    assert!(delivery.perform(&client).is_err());
    delivery.perform(&client).expect("link retry");
    server.join().expect("server");
    let calls: Vec<_> = calls.try_iter().collect();
    assert_eq!(calls.iter().filter(|call| call.path == "/api/inbox/messages").count(), 1);
    assert_eq!(calls[2].body, calls[3].body);
    assert_eq!(calls[3].body["thread_id"], "msg_new");
    assert_eq!(calls[3].body["op"], "send");
}

#[test]
fn board_request_rejects_another_server_incarnation_and_unsafe_ids() {
    let (client, _calls, server) =
        server(vec![(200, health()), (200, json!({"serverSession":"other","cards":[]}))]);
    assert!(client.board_cards().is_err());
    assert!(client.board_card("../settings").is_err());
    server.join().expect("server");
}

#[test]
fn explicit_seen_uses_the_displayed_cursor_without_refreshing_or_marking_forges() {
    let (client, calls, server) = server(vec![(200, health()), (200, json!({"seen_seq":7}))]);
    let mut board = Board::default();
    board.model.cursor = 7;
    board.handle_key(key(KeyCode::Char('v')), &client).expect("mark read");
    assert_eq!(board.model.seen_seq, Some(7));
    server.join().expect("server");
    let calls: Vec<_> = calls.try_iter().collect();
    assert_eq!(calls[1].path, "/api/inbox/cards/seen");
    assert_eq!(calls[1].body, json!({"through_seq":7,"serverSession":"test"}));
}

#[test]
fn tidy_is_board_scoped_and_retains_its_key_until_confirmation() {
    let (client, calls, server) = server(vec![
        (200, health()),
        (
            200,
            json!({"agents":[
        {"name":"Ada","session":"ses_one","reach":"wakes","items":[{"subject":"Work"}]}
    ],"run":null}),
        ),
    ]);
    let mut board = Board { project: Some("prj_one".into()), ..Board::default() };
    board.handle_key(key(KeyCode::Char('T')), &client).expect("tidy preview");
    let Some(Modal::Form(form)) = &board.modal else { panic!("tidy form") };
    let Submission::Confirm { body, .. } = form.submit().expect("preview send") else {
        panic!("confirmation")
    };
    assert_eq!(body["surface"], "board");
    assert_eq!(body["project"], "prj_one");
    assert_eq!(body["sessions"], json!(["ses_one"]));
    assert!(body["idempotency_key"].as_str().is_some_and(|key| !key.is_empty()));
    server.join().expect("server");
    let calls: Vec<_> = calls.try_iter().collect();
    assert_eq!(calls[1].path, "/api/inbox/tidy?surface=board&project=prj_one");
    assert!(calls.iter().all(|call| call.method == "GET"));
}

fn health() -> Value {
    json!({"serverSession":"test"})
}
fn empty_cards() -> Value {
    json!({"serverSession":"test","cursor":1,"cards":[]})
}
fn projects() -> Value {
    json!({"projects":[{"id":"prj_one","name":"Widgets","root":"/widgets"}]})
}

struct Call {
    method: String,
    path: String,
    body: Value,
}

/// A real loopback HTTP transport, never the running Inbox or any user's threads.
fn server(replies: Vec<(u16, Value)>) -> (Client, Receiver<Call>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture server");
    let address = listener.local_addr().expect("address");
    let (tx, rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        for (status, body) in replies {
            let (mut stream, _) = listener.accept().expect("accept");
            stream.set_read_timeout(Some(Duration::from_secs(3))).expect("deadline");
            let mut reader = BufReader::new(stream.try_clone().expect("reader"));
            let mut line = String::new();
            reader.read_line(&mut line).expect("request line");
            let mut words = line.split_whitespace();
            let method = words.next().expect("method").to_owned();
            let path = words.next().expect("path").to_owned();
            let mut length = 0;
            loop {
                line.clear();
                reader.read_line(&mut line).expect("header");
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse::<usize>().expect("length");
                }
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).expect("request body");
            let request_body = if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&bytes).expect("JSON request")
            };
            tx.send(Call { method, path, body: request_body }).expect("capture request");
            let body = body.to_string();
            write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).expect("response");
        }
    });
    let client = Client::from_url(&format!("http://{address}")).expect("connect fixture");
    (client, rx, server)
}
