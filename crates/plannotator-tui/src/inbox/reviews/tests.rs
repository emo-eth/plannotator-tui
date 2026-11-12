#![allow(clippy::indexing_slicing, reason = "tests assert exact annotation fields")]

use super::*;
use anyhow::{Context, Result};
use serde_json::{Value, json};

fn pane() -> Result<ReviewPane> {
    let snapshot: Snapshot = serde_json::from_value(json!({
        "kind":"plannotator-guided-review", "version":5,
        "guide":{"title":"Change","intent":"Why", "sections":[{"title":"First","overview":"Core change","diffs":[{"file":"a.rs","notes":[{"lineStart":3,"lineEnd":3,"text":"Agent note"}]}]}]},
        "review":{"gitRef":"main..HEAD","rawPatch":"diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -3,2 +3,2 @@\n-old\n+new\n same\n@@ -20 +20 @@\n-later\n+after\n"}
    }))?;
    let message: GuideLink =
        serde_json::from_value(json!({"id":"msg_1","seq":2,"guide":{"title":"Change","review":true}}))?;
    let files = diff::parse(&snapshot.review.raw_patch)?;
    let mut pane = ReviewPane {
        thread_id: "thread_1".into(),
        message,
        snapshot,
        files,
        records: vec![],
        read_only: false,
        code_review: true,
        verdict: true,
        view: View::Files,
        mode: Mode::Browse,
        compose: Compose::default(),
        note: String::new(),
        rows: vec![],
        width: 0,
        cursor: 0,
        scroll: 0,
        height: 10,
        horizontal: 0,
        selected_comment: 0,
        selection: None,
        side: Side::New,
        expanded: None,
        pending: None,
        status: String::new(),
    };
    pane.rebuild(80);
    Ok(pane)
}

#[test]
fn selected_removed_lines_keep_old_coordinates_and_exact_quote() -> Result<()> {
    let mut pane = pane()?;
    pane.cursor = pane
        .rows
        .iter()
        .position(|row| row.location.as_ref().is_some_and(|line| line.old == Some(3) && line.new.is_none()))
        .context("removed line")?;
    let annotation = pane.selection_annotation()?;
    assert_eq!(annotation["side"], "old");
    assert_eq!(annotation["lineStart"], 3);
    assert_eq!(annotation["selectedText"], "old");
    Ok(())
}

#[test]
fn selection_rejects_cross_hunk_ranges_before_sending() -> Result<()> {
    let mut pane = pane()?;
    pane.selection =
        pane.rows.iter().position(|row| row.location.as_ref().is_some_and(|line| line.new == Some(3)));
    pane.cursor = pane
        .rows
        .iter()
        .position(|row| row.location.as_ref().is_some_and(|line| line.new == Some(20)))
        .context("second hunk")?;
    assert!(pane.selection_annotation().is_err());
    Ok(())
}

#[test]
fn archived_guide_is_writable_only_after_archive_boundary() -> Result<()> {
    let mut pane = pane()?;
    assert!(model::read_only(&json!({"resolved_at":"today","archive_seq":3}), &pane.message));
    assert!(!model::read_only(&json!({"resolved_at":"today","archive_seq":1}), &pane.message));
    pane.verdict = false;
    assert_eq!(pane.send_options(), vec![SendKind::Feedback]);
    pane.read_only = true;
    assert!(pane.new_comment(true, false).is_err());
    Ok(())
}

#[test]
fn rendering_includes_agent_notes_and_real_diff_coordinates() -> Result<()> {
    let mut pane = pane()?;
    pane.view = View::Walkthrough;
    let backend = ratatui::backend::TestBackend::new(100, 40);
    let mut terminal = ratatui::Terminal::new(backend)?;
    pane.invalidate();
    terminal.draw(|frame| pane.draw(frame, frame.area()))?;
    let text: String =
        terminal.backend().buffer().content.iter().map(ratatui::buffer::Cell::symbol).collect();
    assert!(text.contains("Agent note"));
    assert!(text.contains("Core change"));
    assert!(text.contains("main..HEAD"));
    Ok(())
}

#[test]
fn failed_reply_retries_identical_payload_and_preserves_note() -> Result<()> {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let worker = std::thread::spawn(move || -> Result<Vec<Value>> {
        let mut bodies = Vec::new();
        for turn in 0..3 {
            let (mut stream, _) = listener.accept()?;
            let mut reader = BufReader::new(stream.try_clone()?);
            let mut line = String::new();
            let mut length = 0;
            loop {
                line.clear();
                reader.read_line(&mut line)?;
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("Content-Length")
                {
                    length = value.trim().parse::<usize>()?;
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body)?;
            if !body.is_empty() {
                bodies.push(serde_json::from_slice(&body)?);
            }
            let (status, response) = match turn {
                0 => ("200 OK", r#"{"serverSession":"fixture"}"#),
                1 => ("503 Service Unavailable", r#"{"error":"lost answer"}"#),
                _ => ("200 OK", r#"{"reply":{"id":"reply_1"},"questions":[]}"#),
            };
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                response.len()
            )?;
        }
        Ok(bodies)
    });
    let client = Client::from_url(&format!("http://{address}"))?;
    let mut pane = pane()?;
    pane.note = "Please keep the tests".into();
    assert!(pane.send(&client, SendKind::Approve).is_err());
    assert!(pane.pending.is_some());
    assert_eq!(pane.note, "Please keep the tests");
    pane.retry_send(&client)?;
    assert!(pane.pending.is_none());
    let bodies = worker.join().map_err(|_| anyhow::anyhow!("fixture server panicked"))??;
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies.first(), bodies.last());
    assert_eq!(
        bodies.first().and_then(|body| body.get("serverSession")).and_then(Value::as_str),
        Some("fixture")
    );
    Ok(())
}
