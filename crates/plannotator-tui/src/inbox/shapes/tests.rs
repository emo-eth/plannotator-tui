#![allow(clippy::expect_used, clippy::indexing_slicing, reason = "tests assert wire and rendering behavior")]

use ratatui::{Terminal, backend::TestBackend};
use serde_json::{Value, json};

use super::{Opened, Shapes, Target, api, model};

fn loaded() -> model::Loaded {
    serde_json::from_value(json!({
        "shape": {
            "id": "shp_01ARZ3NDEKTSV4RRFFQ69G5FAV", "title": "Thread routing",
            "project": {"name": "Inbox", "root": "/work/inbox"},
            "thread": "msg_01ARZ3NDEKTSV4RRFFQ69G5FAV", "state": "active", "latest": 3
        },
        "version": {
            "n": 2, "at": "2026-11-11T12:00:00Z", "title": "Thread routing", "claim": "Routes to its owner",
            "views": {"picture": {"html": "<div data-mark=owner>Thread owner</div>"}, "know": "Retired content"},
            "parts": [
                {"view": "picture", "id": "owner", "text": "Thread owner"},
                {"view": "know", "id": "old", "text": "Retired content"},
                {"view": "changed", "id": "old-change", "text": "Retired diff"},
                {"view": "code", "id": "guide", "text": "Separate guided review"}
            ],
            "refs": [{"n": 1, "kind": "file", "title": "Routing implementation", "target": "src/routes.ts", "line": 42}]
        },
        "seen": 1, "notes": [], "session": {"state": "gone", "agent": "Claude Code"}
    })).expect("upstream Shape wire")
}

#[test]
fn part_navigation_uses_server_anchors_and_hides_retired_views() {
    let mut pane = Shapes {
        opened: Some(Opened::Shape { loaded: Box::new(loaded()), history: Vec::new() }),
        page: 1,
        ..Shapes::default()
    };
    pane.rebuild();
    assert_eq!(pane.rows.len(), 1);
    assert!(matches!(&pane.rows[0].target, Target::Part { view, id, quote }
        if view == "picture" && id == "owner" && quote == "Thread owner"));
}

#[test]
fn setup_editor_cannot_change_project_identity_or_send_unknown_fields() {
    let body = api::setup_body(
        "prj_one",
        r#"{"repos":["owner/other"],"docs":["docs.example.com"],"agent":"pi","model":null}"#,
    )
    .expect("valid setup");
    assert_eq!(body["project_id"], "prj_one");
    assert_eq!(body["model"], Value::Null);
    assert!(
        api::setup_body(
            "prj_one",
            r#"{"project_id":"prj_two","repos":[],"docs":[],"agent":"pi","model":null}"#
        )
        .is_err()
    );
    assert!(api::setup_body("prj_one", r#"{"repos":[],"docs":[],"agent":"shell","model":null}"#).is_err());
}

#[test]
fn query_values_cannot_inject_a_second_project_or_path() {
    assert_eq!(
        api::query("/api/inbox/system/drawing", "project", "a&project=b"),
        "/api/inbox/system/drawing?project=a%26project%3Db"
    );
    assert_eq!(api::encode("repo/a#b?x&y"), "repo%2Fa%23b%3Fx%26y");
}

#[test]
fn system_preserves_sheet_details_and_unread_pull_warning() {
    let drawing: model::Drawing = serde_json::from_value(json!({
        "project": {"id":"prj_one","name":"Inbox","repo":"github.com/org/inbox","state":"drawn","sheets":1,"wanted":0},
        "model": {"sheets":{"C-001":{"title":"Inbox flow","question":"Who owns a reply?","hops":[{"do":"Send it"}]}},"parts":{},"flows":{},"revisions":{}}
    })).expect("System wire");
    let mut pane = Shapes {
        opened: Some(Opened::System { drawing, pulls: model::Pulls { pulls: Vec::new(), unread: 2 } }),
        ..Shapes::default()
    };
    pane.rebuild();
    assert!(pane.rows[0].detail.contains("Who owns a reply?"));
    assert!(pane.rows[0].detail.contains("Send it"));
    assert!(matches!(&pane.rows[0].target, Target::Sheet(id) if id == "C-001"));
    pane.page = 5;
    pane.rebuild();
    assert!(pane.rows[0].detail.contains("2 PRs could not be checked"));
}

#[test]
fn terminal_labels_graphical_handoff_instead_of_claiming_canvas_support() {
    let mut pane = Shapes {
        opened: Some(Opened::Shape { loaded: Box::new(loaded()), history: Vec::new() }),
        ..Shapes::default()
    };
    pane.rebuild();
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).expect("terminal");
    terminal
        .draw(|frame| {
            let area = frame.area();
            pane.draw(frame, area);
        })
        .expect("frame");
    let buffer = terminal.backend().buffer();
    let text = buffer.content.iter().map(ratatui::buffer::Cell::symbol).collect::<String>();
    assert!(text.contains("draws in browser"));
    assert!(text.contains("no direct graphic editing"));
    assert!(pane.form.is_none());
    assert!(pane.worker.is_none());
}
