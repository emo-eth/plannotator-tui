#![allow(clippy::expect_used, clippy::indexing_slicing, reason = "tests assert by panicking")]

use super::*;

fn thread_pull(number: u64, state: &str) -> Value {
    json!({
        "id": format!("msg_test:github.com/o/r#{number}"),
        "project_id": "prj_test", "thread_id": "msg_test", "pull": format!("github.com/o/r#{number}"),
        "url": format!("https://github.com/o/r/pull/{number}"), "added_by": {"kind":"person"},
        "added_at":"2026-11-08T10:00:00Z", "state":state, "title":null, "base":"main", "head":"feature",
        "read_at":null, "platform":"github", "host":"github.com", "repo":"o/r", "number":number,
        "label":format!("#{number}"), "ref":format!("o/r#{number}"), "checks":"failing", "review":"approved"
    })
}

#[test]
fn thread_list_keeps_removed_timestamp_and_follows_server_stack_order() {
    let mut removed = thread_pull(4, "closed");
    removed["removed_at"] = json!("2026-11-09T10:00:00Z");
    removed["removed_by"] = json!({"kind":"agent","session":"ses_test","host":"pi","name":null});
    let answer: ThreadPulls = serde_json::from_value(json!({
        "pulls":[thread_pull(3,"unknown"),thread_pull(1,"merged"),thread_pull(2,"open")],
        "groups":[{"kind":"stack","pulls":["github.com/o/r#1","github.com/o/r#2"]},
                  {"kind":"related","pulls":["github.com/o/r#3"]}],
        "removed":[removed]
    }))
    .expect("thread pull response");
    assert_eq!(
        answer.ordered().map(|p| p.reference.as_str()).collect::<Vec<_>>(),
        ["o/r#1", "o/r#2", "o/r#3"]
    );
    assert_eq!(answer.removed[0].removed_at.as_deref(), Some("2026-11-09T10:00:00Z"));
}

#[test]
fn unknown_is_not_open_and_finished_or_draft_pulls_do_not_show_review_verdicts() {
    let mut pull: ThreadPull = serde_json::from_value(thread_pull(1, "open")).expect("pull");
    assert_eq!(pull.extras().collect::<Vec<_>>(), ["checks failing", "approved"]);
    pull.state = PullState::Draft;
    assert_eq!(pull.extras().collect::<Vec<_>>(), ["checks failing"]);
    pull.state = PullState::Merged;
    assert!(pull.extras().next().is_none());
    assert!(!PullState::Unknown.is_open());
}

#[test]
fn add_remove_payloads_use_refs_and_current_session_not_store_keys() {
    let add = PullChange::add_text(" o/r#1,\nhttps://gitlab.com/group/project/-/merge_requests/4  ");
    assert_eq!(
        session_body("srv_current", &add).expect("body"),
        json!({
            "serverSession":"srv_current", "add":["o/r#1","https://gitlab.com/group/project/-/merge_requests/4"]
        })
    );
    assert_eq!(
        session_body("srv_current", PullChange::remove("o/r#1")).expect("body"),
        json!({"serverSession":"srv_current","remove":["o/r#1"]})
    );
    assert_eq!(
        serde_json::to_value(PullChange::remove_all()).expect("remove all"),
        json!({"remove":["all"]})
    );
    assert!(PullChange::add_text(" \n, ").add.is_empty());
}

#[test]
fn query_encoding_preserves_keys_and_never_treats_hash_as_a_fragment() {
    let key = "gitlab.example/group/sub/repo#61:pull";
    let path = query_path("/api/inbox/pulls/diff", &[("key", key), ("scope", "layer")]);
    assert_eq!(path, "/api/inbox/pulls/diff?key=gitlab.example%2Fgroup%2Fsub%2Frepo%2361%3Apull&scope=layer");
    assert_eq!(thread_path("msg_123").expect("valid id"), "/api/inbox/threads/msg_123/pulls");
    for id in ["", "../msg", "msg/a", "machine:msg", "msg?x=1"] {
        assert!(thread_path(id).is_err());
    }
}

#[test]
fn response_must_have_current_server_session_and_cannot_be_empty_success() {
    let response: Response<ThreadPulls> = serde_json::from_value(json!({
        "serverSession":"srv_old", "pulls":[], "groups":[], "removed":[]
    }))
    .expect("response");
    assert!(response.checked("srv_new").is_err());
    assert!(
        serde_json::from_value::<Response<ThreadPulls>>(json!({"pulls":[],"groups":[],"removed":[]}))
            .is_err()
    );
    assert!(serde_json::from_value::<ThreadPulls>(json!({})).is_err());
}

fn review() -> ReviewPost {
    ReviewPost {
        key: "gitlab.com/group/repo#7:pull".to_owned(),
        head_sha: "a".repeat(40),
        action: ReviewAction::Approve,
        body: "Review body that was posted".to_owned(),
        file_comments: vec![FileComment {
            path: "src/main.rs".to_owned(),
            line: 9,
            side: DiffSide::Right,
            body: "Comment that went".to_owned(),
            start_line: Some(7),
            start_side: Some(DiffSide::Right),
        }],
        file_level_comments: vec![FileLevelComment {
            path: "README.md".to_owned(),
            body: "File note".to_owned(),
        }],
    }
}

#[test]
fn review_wire_preserves_inline_ranges_and_uses_platform_field_names() {
    let request = session_body("srv_current", review()).expect("review body");
    assert_eq!(request["serverSession"], "srv_current");
    assert_eq!(request["action"], "approve");
    assert_eq!(request["fileComments"][0]["side"], "RIGHT");
    assert_eq!(request["fileComments"][0]["start_line"], 7);
    assert_eq!(request["fileComments"][0]["start_side"], "RIGHT");
    assert_eq!(request["fileLevelComments"][0]["path"], "README.md");
    assert!(request.get("file_comments").is_none());
}

#[test]
fn partial_review_retry_never_replays_posted_body_file_comments_or_approval() {
    let response: PostedReview = serde_json::from_value(json!({
        "ok":true,"prUrl":"https://gitlab.com/group/repo/-/merge_requests/7",
        "submission":{"status":"partial","postedFileCommentCount":1,
            "failedFileComments":[{"comment":{"path":"src/b.rs","line":4,"side":"LEFT","body":"retry this"},"error":"rate limited"}],
            "reviewBodyPosted":true,"approval":"succeeded",
            "retry":{"action":"comment","fileComments":[{"path":"src/b.rs","line":4,"side":"LEFT","body":"retry this"}]}}
    })).expect("partial response");
    let ReviewSubmission::Partial(partial) = response.submission else { unreachable!("partial response") };
    let original = review();
    let retry = original.retry(&partial);
    assert_eq!(retry.action, ReviewAction::Comment);
    assert!(retry.body.is_empty());
    assert!(retry.file_level_comments.is_empty());
    assert_eq!(retry.file_comments.len(), 1);
    assert_eq!(retry.file_comments[0].path, "src/b.rs");
    assert_eq!(retry.head_sha, original.head_sha);
    assert_eq!(retry.key, original.key);
    assert_eq!(partial.approval, ApprovalOutcome::Succeeded);
}

#[test]
fn partial_retry_carries_only_explicitly_unposted_body_and_decision() {
    let partial: ReviewPartial = serde_json::from_value(json!({
        "postedFileCommentCount":0,"failedFileComments":[],"reviewBodyPosted":false,
        "approval":"failed","approvalError":"permission denied",
        "retry":{"action":"approve","fileComments":[],"body":"Only this body remains"}
    }))
    .expect("partial");
    let retry = review().retry(&partial);
    assert_eq!(retry.action, ReviewAction::Approve);
    assert_eq!(retry.body, "Only this body remains");
    assert!(retry.file_comments.is_empty());
    assert!(retry.file_level_comments.is_empty());
}

#[test]
fn complete_and_partial_submissions_are_not_conflated() {
    assert!(matches!(
        serde_json::from_value::<ReviewSubmission>(json!({"status":"complete"})).expect("complete"),
        ReviewSubmission::Complete { posted_as: None }
    ));
    assert!(serde_json::from_value::<ReviewSubmission>(json!({"status":"partial"})).is_err());
    assert!(serde_json::from_value::<ReviewSubmission>(json!({"status":"unknown"})).is_err());
}

pub(super) fn pull(platform: Platform, mine: bool) -> Pull {
    Pull {
        key: "github.com/o/r#1".to_owned(),
        platform,
        repo: "o/r".to_owned(),
        number: 1,
        url: "https://github.com/o/r/pull/1".to_owned(),
        title: "Example".to_owned(),
        mine,
        state: PullState::Open,
        head_sha: Some("a".repeat(40)),
    }
}

#[test]
fn platform_review_actions_follow_upstream_capabilities() {
    assert!(pull(Platform::Github, false).action_refusal(ReviewAction::Approve).is_none());
    assert!(pull(Platform::Github, true).action_refusal(ReviewAction::Approve).is_some());
    assert!(pull(Platform::Github, true).action_refusal(ReviewAction::RequestChanges).is_some());
    assert!(pull(Platform::Github, true).action_refusal(ReviewAction::Comment).is_none());
    assert!(pull(Platform::Gitlab, false).action_refusal(ReviewAction::RequestChanges).is_none());
    assert!(pull(Platform::Gitlab, true).action_refusal(ReviewAction::RequestChanges).is_none());
    assert!(pull(Platform::Gitlab, true).action_refusal(ReviewAction::Approve).is_none());
    assert!(pull(Platform::Gitlab, false).action_refusal(ReviewAction::Approve).is_none());
    let mut closed = pull(Platform::Github, false);
    assert!(closed.postable());
    closed.state = PullState::Merged;
    assert!(!closed.postable());
    assert!(closed.action_refusal(ReviewAction::Comment).is_some());
    closed.state = PullState::Open;
    closed.head_sha = None;
    assert!(!closed.postable());
}

#[test]
fn client_uses_real_routes_and_never_turns_a_missing_thread_into_an_empty_list() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::thread;

    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback fixture");
    let address = listener.local_addr().expect("fixture address");
    let server = thread::spawn(move || {
        let responses = [
            (200, json!({"serverSession":"srv_test"})),
            (200, json!({"serverSession":"srv_test","pulls":[],"groups":[],"removed":[]})),
            (200, json!({"serverSession":"srv_test","pulls":[],"groups":[],"removed":[]})),
            (404, json!({"code":"thread_not_found","error":"No thread msg_missing."})),
            (
                200,
                json!({"serverSession":"srv_test","ok":true,"prUrl":"https://github.com/o/r/pull/1","submission":{"status":"complete"}}),
            ),
        ];
        let mut requests = Vec::new();
        for (status, response) in responses {
            let (mut stream, _) = listener.accept().expect("fixture connection");
            stream.set_read_timeout(Some(std::time::Duration::from_secs(3))).expect("fixture timeout");
            let mut reader = BufReader::new(stream.try_clone().expect("fixture stream"));
            let mut request = String::new();
            reader.read_line(&mut request).expect("request line");
            let mut length = 0;
            let mut origin = None;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).expect("header");
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse::<usize>().expect("length");
                    }
                    if name.eq_ignore_ascii_case("origin") {
                        origin = Some(value.trim().to_owned());
                    }
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).expect("body");
            let body =
                if body.is_empty() { Value::Null } else { serde_json::from_slice(&body).expect("JSON body") };
            requests.push((request, origin, body));
            let response = response.to_string();
            write!(stream, "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                if status == 200 { "OK" } else { "Not Found" }, response.len()).expect("fixture response");
        }
        requests
    });
    let base = format!("http://{address}");
    let client = Client::from_url(&base).expect("client health");
    assert!(client.thread_pulls("msg_test").expect("thread PRs").pulls.is_empty());
    client.change_thread_pulls("msg_test", &PullChange::add_text("o/r#1")).expect("add PR");
    let error = client.thread_pulls("msg_missing").expect_err("404 must remain an error");
    assert_eq!(error.downcast_ref::<crate::inbox::client::HttpError>().expect("HTTP error").status, 404);
    let posted = client.post_pull_review(&review()).expect("review post");
    assert!(matches!(posted.submission, ReviewSubmission::Complete { posted_as: None }));
    let requests = server.join().expect("fixture server");
    assert_eq!(requests[1].0, "GET /api/inbox/threads/msg_test/pulls HTTP/1.1\r\n");
    assert_eq!(requests[2].0, "POST /api/inbox/threads/msg_test/pulls HTTP/1.1\r\n");
    assert_eq!(requests[2].1.as_deref(), Some(base.as_str()));
    assert_eq!(requests[2].2, json!({"serverSession":"srv_test","add":["o/r#1"]}));
    assert_eq!(requests[4].0, "POST /api/inbox/pulls/review HTTP/1.1\r\n");
    assert_eq!(requests[4].2["serverSession"], "srv_test");
    assert_eq!(requests[4].2["fileComments"][0]["start_line"], 7);
}

pub(super) fn fixture(
    responses: Vec<(u16, Value)>,
) -> (Client, std::thread::JoinHandle<Vec<(String, Value)>>) {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("isolated loopback fixture");
    let address = listener.local_addr().expect("fixture address");
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for (status, response) in std::iter::once((200, json!({"serverSession":"srv_test"}))).chain(responses)
        {
            let (mut stream, _) = listener.accept().expect("fixture connection");
            stream.set_read_timeout(Some(std::time::Duration::from_secs(3))).expect("read timeout");
            let mut reader = BufReader::new(stream.try_clone().expect("stream"));
            let mut request = String::new();
            reader.read_line(&mut request).expect("request");
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).expect("header");
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse::<usize>().expect("length");
                }
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).expect("body");
            let body = if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&bytes).expect("body JSON")
            };
            requests.push((request, body));
            let response = response.to_string();
            write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).expect("response");
        }
        requests
    });
    (Client::from_url(&format!("http://{address}")).expect("client"), server)
}

#[test]
fn gitlab_old_server_reports_comment_not_requested_changes() {
    let submission: ReviewSubmission =
        serde_json::from_value(json!({"status":"complete","postedAs":"comment"})).expect("submission");
    assert!(matches!(submission, ReviewSubmission::Complete { posted_as: Some(ReviewAction::Comment) }));
}
