#![allow(clippy::expect_used, clippy::indexing_slicing, reason = "tests assert by panicking")]

use super::super::tests::{fixture, pull};
use super::super::{Platform, PullState};
use super::*;
use ratatui::crossterm::event::KeyModifiers;
use serde_json::json;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn review() -> PullReview {
    let pull = pull(Platform::Github, false);
    let diff = PullDiff {
        key: pull.key.clone(),
        scope: DiffScope::Layer,
        base: "main".into(),
        head: "feature".into(),
        patch: String::new(),
        head_sha: Some("b".repeat(40)),
    };
    PullReview {
        pull,
        diff,
        lines: Vec::new(),
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
    }
}

#[test]
fn merge_needs_separate_confirmation_and_pins_read_diff_not_list_head() {
    let (client, server) = fixture(vec![(
        200,
        json!({"serverSession":"srv_test","ok":true,"key":"github.com/o/r#1","method":"squash","url":"https://github.com/o/r/pull/1"}),
    )]);
    let mut pane = review();
    pane.handle_key(key(KeyCode::Char('M')), &client).expect("arm");
    assert_eq!(pane.pull.state, PullState::Open);
    assert_eq!(pane.merge.as_ref().expect("confirmation").head, "b".repeat(40));
    pane.handle_key(key(KeyCode::Esc), &client).expect("cancel");
    assert!(pane.merge.is_none());
    pane.handle_key(key(KeyCode::Char('M')), &client).expect("arm again");
    let repeat = KeyEvent::new_with_kind(KeyCode::Enter, KeyModifiers::NONE, KeyEventKind::Repeat);
    pane.handle_key(repeat, &client).expect("ignore repeat");
    assert!(pane.merge.is_some());
    assert_eq!(pane.pull.state, PullState::Open);
    pane.handle_key(key(KeyCode::Enter), &client).expect("confirmed merge");
    assert_eq!(pane.pull.state, PullState::Merged);
    assert!(pane.merge.is_none());
    assert!(pane.handle_key(key(KeyCode::Char('M')), &client).is_err());
    let requests = server.join().expect("server");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].0, "POST /api/inbox/pulls/merge HTTP/1.1\r\n");
    assert_eq!(
        requests[1].1,
        json!({"serverSession":"srv_test","key":"github.com/o/r#1","head_sha":"b".repeat(40),"confirmed":true})
    );
}

#[test]
fn uncertain_merge_cannot_be_replayed_without_refresh() {
    let (client, server) = fixture(vec![(502, json!({"code":"pull_merge_failed","error":"response lost"}))]);
    let mut pane = review();
    pane.handle_key(key(KeyCode::Char('M')), &client).expect("arm");
    assert!(pane.handle_key(key(KeyCode::Enter), &client).is_err());
    assert!(pane.merge.is_none());
    assert!(pane.merge_uncertain);
    pane.handle_key(key(KeyCode::Enter), &client).expect("no second write");
    assert!(pane.handle_key(key(KeyCode::Char('M')), &client).is_err());
    assert_eq!(server.join().expect("server").len(), 2);
}

#[test]
fn merge_rejects_drafts_unknown_heads_stacks_and_unsent_review() {
    let mut pane = review();
    assert!(MergeConfirmation::prepare(&pane.pull, &pane.diff, true).is_err());
    for state in [PullState::Draft, PullState::Merged, PullState::Closed, PullState::Unknown] {
        pane.pull.state = state;
        assert!(MergeConfirmation::prepare(&pane.pull, &pane.diff, false).is_err());
    }
    pane.pull.state = PullState::Open;
    pane.diff.scope = DiffScope::Stack;
    assert!(MergeConfirmation::prepare(&pane.pull, &pane.diff, false).is_err());
    pane.diff.scope = DiffScope::Layer;
    pane.diff.head_sha = Some("not-a-sha".into());
    assert!(MergeConfirmation::prepare(&pane.pull, &pane.diff, false).is_err());
    pane.diff.head_sha = None;
    assert!(MergeConfirmation::prepare(&pane.pull, &pane.diff, false).is_err());
}

#[test]
fn gitlab_request_changes_posts_real_action_but_labels_legacy_comment_truthfully() {
    let (client, server) = fixture(vec![(
        200,
        json!({"serverSession":"srv_test","ok":true,"prUrl":"https://gitlab.example/o/r/-/merge_requests/1","submission":{"status":"complete","postedAs":"comment"}}),
    )]);
    let mut pane = review();
    pane.pull.platform = Platform::Gitlab;
    pane.pull.mine = true;
    pane.body = "Please fix the regression".into();
    pane.prepare(ReviewAction::RequestChanges).expect("GitLab permits self request changes");
    pane.post(&client).expect("post");
    assert!(pane.status.as_deref().expect("status").starts_with("Review posted on GitLab"));
    assert!(!pane.has_draft());
    let requests = server.join().expect("server");
    assert_eq!(requests[1].1["action"], "request_changes");
}

#[test]
fn partial_post_retries_only_server_reported_missing_changes() {
    let missing = json!({"path":"src/b.rs","line":4,"side":"LEFT","body":"remaining"});
    let (client, server) = fixture(vec![
        (
            200,
            json!({"serverSession":"srv_test","ok":true,"prUrl":"https://gitlab.example/o/r/-/merge_requests/1","submission":{"status":"partial","postedFileCommentCount":1,"failedFileComments":[{"comment":missing,"error":"rate limit"}],"reviewBodyPosted":true,"approval":"succeeded","retry":{"action":"comment","fileComments":[missing]}}}),
        ),
        (
            200,
            json!({"serverSession":"srv_test","ok":true,"prUrl":"https://gitlab.example/o/r/-/merge_requests/1","submission":{"status":"complete"}}),
        ),
    ]);
    let mut pane = review();
    pane.pull.platform = Platform::Gitlab;
    pane.body = "Already posted".into();
    pane.comments = vec![
        FileComment {
            path: "src/a.rs".into(),
            line: 1,
            side: DiffSide::Right,
            body: "accepted".into(),
            start_line: None,
            start_side: None,
        },
        serde_json::from_value(missing).expect("comment"),
    ];
    pane.prepare(ReviewAction::Approve).expect("prepare");
    pane.post(&client).expect("partial");
    assert_eq!(pane.pending.as_ref().expect("retry").action, ReviewAction::Comment);
    assert!(pane.body.is_empty());
    assert_eq!(pane.comments.len(), 1);
    pane.post(&client).expect("retry only missing");
    let requests = server.join().expect("server");
    assert_eq!(requests[2].1["action"], "comment");
    assert_eq!(requests[2].1["body"], "");
    assert_eq!(requests[2].1["fileComments"].as_array().expect("comments").len(), 1);
    assert_eq!(requests[2].1["fileComments"][0]["path"], "src/b.rs");
    assert!(!pane.has_draft());
}

#[test]
fn owner_validates_repository_before_confirmation_and_never_sends_message() {
    let (client, server) = fixture(vec![
        (
            200,
            json!({"serverSession":"srv_test","cursor":0,"sections":[{"id":"s","label":"active","threads":[{"thread_id":"msg_target","subject":"Agent task","project":{"id":"prj_target","name":"App"}}]}]}),
        ),
        (200, json!({"serverSession":"srv_test","ok":true})),
        (200, json!({"serverSession":"srv_test","ok":true})),
    ]);
    let mut owner = OwnerPane::load(&client, "github.com/o/r#1").expect("choices");
    assert!(!owner.handle(key(KeyCode::Enter), &client).expect("preflight"));
    assert!(owner.handle(key(KeyCode::Enter), &client).expect("confirm"));
    let requests = server.join().expect("server");
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[2].0, "POST /api/inbox/pulls/owner HTTP/1.1\r\n");
    assert_eq!(
        requests[2].1,
        json!({"serverSession":"srv_test","keys":["github.com/o/r#1"],"project_id":"prj_target","validate_only":true})
    );
    assert_eq!(
        requests[3].1,
        json!({"serverSession":"srv_test","keys":["github.com/o/r#1"],"thread_id":"msg_target","ask":"custom"})
    );
    assert!(owner.status.contains("No message was sent"));
}
