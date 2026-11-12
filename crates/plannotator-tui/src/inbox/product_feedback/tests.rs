#![allow(clippy::expect_used, reason = "tests assert by panicking")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{Value, json};

use super::super::client::Client;
use super::help::{Help, Hit};
use super::model::{self, Category, Draft};

#[test]
fn input_limits_count_unicode_scalars_after_trimming() {
    let mut draft = Draft {
        message: format!("  {}  ", "🦀".repeat(4_000)),
        contact: "é".repeat(200),
        category: Category::Idea,
    };
    let body = draft.body().expect("exact boundary accepted");
    assert_eq!(body.get("message").and_then(Value::as_str).map(|s| s.chars().count()), Some(4_000));
    draft.message.push('a');
    assert!(draft.body().is_err());
    draft.message = "Hi".into();
    draft.contact.push('a');
    assert!(draft.body().is_err());
    draft.contact.clear();
    draft.message = " \t\n".into();
    assert!(draft.body().is_err());
}

#[test]
fn optional_fields_clear_and_categories_use_the_wire_names() {
    let mut draft = Draft { message: "  My words\n  ".into(), contact: " \t ".into(), ..Draft::default() };
    assert_eq!(draft.body().expect("valid"), json!({"message":"My words", "category":null, "contact":null}));
    for (index, expected) in [(1, "bug"), (2, "idea"), (3, "other")] {
        draft.category = Category::from_index(index);
        assert_eq!(draft.body().expect("valid").get("category").and_then(Value::as_str), Some(expected));
    }
    assert_eq!(Category::from_index(0), Category::None);
}

#[test]
fn submit_uses_the_inbox_session_and_only_allowed_input_fields() {
    let (url, requests, worker) = server(200, json!({"ok":true}));
    let client = Client::from_url(&url).expect("connect fixture");
    let draft = Draft {
        message: "  Keep my exact words.  ".into(),
        category: Category::Bug,
        contact: " @me ".into(),
    };
    model::submit(&client, &draft).expect("send");
    let (request, body) = requests.recv_timeout(Duration::from_secs(3)).expect("one post");
    assert_eq!(request, "POST /api/inbox/feedback HTTP/1.1");
    assert_eq!(
        body,
        json!({"message":"Keep my exact words.", "category":"bug", "contact":"@me", "serverSession":"test-session"})
    );
    worker.join().expect("fixture exits");
    assert!(requests.try_recv().is_err());
}

#[test]
fn rejection_preserves_the_draft_and_never_retries() {
    let (url, requests, worker) = server(
        502,
        json!({"error":"Couldn't send feedback right now; nothing was saved.", "code":"feedback_not_sent"}),
    );
    let client = Client::from_url(&url).expect("connect fixture");
    let draft = Draft { message: "The form fails".into(), ..Draft::default() };
    let before = draft.clone();
    let error = model::submit(&client, &draft).expect_err("refused");
    assert!(model::failure(&error).contains("nothing was saved"));
    assert_eq!(draft, before);
    assert!(requests.recv_timeout(Duration::from_secs(3)).is_ok());
    worker.join().expect("fixture exits");
    assert!(requests.try_recv().is_err());
}

#[test]
fn non_acknowledgement_is_not_success() {
    let (url, requests, worker) = server(200, json!({}));
    let client = Client::from_url(&url).expect("connect fixture");
    let error =
        model::submit(&client, &Draft { message: "Hi".into(), ..Draft::default() }).expect_err("missing ack");
    assert!(model::failure(&error).contains("may have been delivered"));
    assert!(requests.recv_timeout(Duration::from_secs(3)).is_ok());
    worker.join().expect("fixture exits");
}

#[test]
fn known_help_keeps_whole_word_proximity_negation_and_broken_rules() {
    let help = Help::load().expect("bundled help");
    let found = |text: &str| {
        help.related(text, None).iter().find_map(|hit| match hit {
            Hit::Known(index) => help.known.get(*index).map(|known| known.id.as_str()),
            Hit::Feature(_) => None,
        })
    };
    for (text, expected) in [
        ("the list keeps jumping", "list-moves"),
        ("the groups are confusing", "simple-list"),
        ("rows are too cluttered", "rows"),
        ("how do I mark a thread as done", "done"),
        ("is there a mobile app", "phone"),
        ("I can't start an agent, where is the setting", "start-agents"),
        ("notifications don't show up", "notifications"),
        ("dark mode please", "theme"),
        ("I keep triggering shortcuts by accident while typing", "keys"),
        ("agent messages are full of jargon", "bro-mode"),
        ("I want to reply by voice", "voice"),
        ("I run agents on two machines", "machines"),
        ("remind me later", "snooze"),
        ("I want to pin threads", "star"),
        ("mark as unread please", "unread"),
        ("show the agent checklist", "checklist"),
        ("a kanban view", "board"),
        ("too many questions at once", "decision-mode"),
        ("I lose my reply when I switch threads", "float-drafts"),
        ("plan review over ssh doesn't work", "plan-review-off"),
        ("does it auto update?", "updates"),
        ("custom instructions for the inbox reminder", "prompts"),
        ("how does this work", "tour"),
    ] {
        assert_eq!(found(text), Some(expected), "{text}");
    }
    for text in [
        "list",
        "the agent moves files around in my repo",
        "I want to remove a project",
        "the list no longer jumps, thanks for the fix",
        "voice input does not work",
        "dark mode is broken in the diff",
        "Claude Code crashes when I start it",
    ] {
        assert_eq!(found(text), None, "{text}");
    }
}

#[test]
fn unsupported_settings_hide_answers_and_faq_is_upstream_order() {
    let help = Help::load().expect("bundled help");
    let settings = json!({"launch":{"unsupported":true}});
    for text in [
        "start an agent",
        "show the agent checklist",
        "bro mode",
        "kanban",
        "too many questions",
        "floating drafts",
    ] {
        assert!(help.related(text, Some(&settings)).iter().all(|hit| matches!(hit, Hit::Feature(_))));
    }
    assert_eq!(
        help.known.iter().filter(|k| k.faq.is_some()).map(|k| k.id.as_str()).collect::<Vec<_>>(),
        ["list-moves", "simple-list", "rows", "done", "phone"]
    );
    assert!(help.related(&format!("{}dark mode", "x ".repeat(2_000)), None).is_empty());
}

#[test]
fn prompt_quotes_the_draft_and_requires_explicit_approval() {
    let draft = Draft { message: "$(do-not-run)\n$SECRET `unsafe`".into(), ..Draft::default() };
    let prompt = super::prompt::text(&draft, None);
    assert!(prompt.contains("> $(do-not-run)\n> $SECRET `unsafe`"));
    assert!(prompt.contains("Only after I say yes"));
    assert!(prompt.contains("<<'PLANNOTATOR_FEEDBACK_END'"));
    assert!(!prompt.contains("--contact 'ada@example.com'"));
}

fn server(status: u16, response: Value) -> (String, Receiver<(String, Value)>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
    let url = format!("http://{}/", listener.local_addr().expect("address"));
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        for index in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept fixture request");
            stream.set_read_timeout(Some(Duration::from_secs(3))).expect("bounded read");
            let (request, body) = request(&stream);
            let (status, body) = if index == 0 {
                assert_eq!(request, "GET /api/inbox/health HTTP/1.1");
                (200, json!({"serverSession":"test-session", "version":"0.28.52"}))
            } else {
                tx.send((request, serde_json::from_slice(&body).expect("json body"))).expect("capture");
                (status, response.clone())
            };
            let body = body.to_string();
            write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).expect("respond");
        }
    });
    (url, rx, worker)
}

fn request(stream: &TcpStream) -> (String, Vec<u8>) {
    let mut reader = BufReader::new(stream);
    let mut request = String::new();
    reader.read_line(&mut request).expect("request line");
    let mut length = 0;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).expect("header");
        if header == "\r\n" || header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().expect("length");
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).expect("body");
    (request.trim_end().to_owned(), body)
}
