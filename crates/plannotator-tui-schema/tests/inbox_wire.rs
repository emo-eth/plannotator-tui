#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use plannotator_tui_schema::inbox::{InboxHealth, ListModel, ThreadModel};

#[test]
fn test_health_round_trip() {
    let raw = include_str!("fixtures/inbox/health.json");
    let health: InboxHealth = serde_json::from_str(raw).expect("deserialize health");
    assert!(health.ok);
    assert_eq!(health.app, "plannotator-inbox");
    assert_eq!(health.version, "0.28.11");
    assert!(!health.server_session.is_empty());

    let reserialized = serde_json::to_string(&health).expect("serialize health");
    let health2: InboxHealth = serde_json::from_str(&reserialized).expect("re-deserialize health");
    assert_eq!(health, health2);
}

#[test]
fn test_threads_list_round_trip() {
    let raw = include_str!("fixtures/inbox/threads.json");
    let list: ListModel = serde_json::from_str(raw).expect("deserialize threads list");
    assert!(!list.server_session.is_empty());
    assert_eq!(list.version, "0.28.11");
    assert_eq!(list.projects.len(), 2);
    assert_eq!(list.sections.len(), 6);

    let quiet_section = list.sections.iter().find(|s| s.label == "Quiet").expect("quiet section");
    assert!(!quiet_section.threads.is_empty());

    let reserialized = serde_json::to_string(&list).expect("serialize threads list");
    let list2: ListModel = serde_json::from_str(&reserialized).expect("re-deserialize threads list");
    assert_eq!(list, list2);
}

#[test]
fn test_thread_detail_round_trip() {
    let raw = include_str!("fixtures/inbox/thread_detail.json");
    let detail: ThreadModel = serde_json::from_str(raw).expect("deserialize thread detail");
    assert!(!detail.server_session.is_empty());
    assert_eq!(detail.thread.thread_id, "msg_01M4MG8TXZN10HCNERMYAN43GB");
    assert_eq!(detail.thread.messages.len(), 2);

    let first_msg = &detail.thread.messages[0];
    assert!(first_msg.questions.is_some());
    let questions = first_msg.questions.as_ref().unwrap();
    assert_eq!(questions.len(), 1);
    assert_eq!(questions[0].key, "q-1b72fe04");
    assert_eq!(questions[0].choices.len(), 3);
    assert!(questions[0].answer.is_some());

    let reserialized = serde_json::to_string(&detail).expect("serialize thread detail");
    let detail2: ThreadModel = serde_json::from_str(&reserialized).expect("re-deserialize thread detail");
    assert_eq!(detail, detail2);
}
