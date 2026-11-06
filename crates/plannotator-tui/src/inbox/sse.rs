#![allow(clippy::all, clippy::pedantic)]

//! Hand-rolled Server-Sent Events (SSE) reader for Plannotator Inbox.
//!
//! Consumes `GET /api/inbox/events?cursor=n`, parsing `hello`, `record`, `status`,
//! and `: ping` heartbeat lines. Reconnects on stream drops.

use std::io::BufRead;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

use anyhow::{Context, Result};
use plannotator_tui_schema::inbox::{InboxEventRecord, InboxServerEvent, SseHello, SseStatus};

/// Parse a completed SSE event block.
pub(crate) fn parse_sse_block(
    event_type: Option<&str>,
    data: &str,
    last_id: Option<u64>,
) -> Result<Option<(InboxServerEvent, Option<u64>)>> {
    let event_type = event_type.unwrap_or("message");
    match event_type {
        "hello" => {
            let hello: SseHello =
                serde_json::from_str(data).with_context(|| format!("parsing hello SSE data: {data}"))?;
            let cursor = Some(hello.cursor);
            Ok(Some((InboxServerEvent::Hello(hello), cursor)))
        }
        "record" => {
            let record: InboxEventRecord =
                serde_json::from_str(data).with_context(|| format!("parsing record SSE data: {data}"))?;
            let seq = Some(record.seq).or(last_id);
            Ok(Some((InboxServerEvent::Record(record), seq)))
        }
        "status" => {
            let status: SseStatus =
                serde_json::from_str(data).with_context(|| format!("parsing status SSE data: {data}"))?;
            Ok(Some((InboxServerEvent::Status(status), last_id)))
        }
        _ => Ok(None),
    }
}

/// Read lines from a `BufRead` stream and dispatch parsed SSE events.
pub(crate) fn read_sse_stream<R: BufRead, F>(mut reader: R, mut on_event: F) -> Result<()>
where
    F: FnMut(InboxServerEvent, Option<u64>),
{
    let mut current_event: Option<String> = None;
    let mut current_data = String::new();
    let mut current_id: Option<u64> = None;

    let mut line = String::new();
    while reader.read_line(&mut line)? > 0 {
        let trimmed = line.trim_end_matches(&['\r', '\n'][..]);

        if trimmed.is_empty() {
            // End of event block: dispatch if data was present
            if !current_data.is_empty() {
                if let Ok(Some((event, seq))) =
                    parse_sse_block(current_event.as_deref(), &current_data, current_id)
                {
                    on_event(event, seq);
                }
            }
            current_event = None;
            current_data.clear();
        } else if trimmed.starts_with(':') {
            // Comment or heartbeat line, e.g. ": ping"
            let comment = trimmed[1..].trim();
            if comment == "ping" {
                on_event(InboxServerEvent::Ping, None);
            }
        } else if let Some(rest) = trimmed.strip_prefix("event:") {
            current_event = Some(rest.trim().to_owned());
        } else if let Some(rest) = trimmed.strip_prefix("data:") {
            if !current_data.is_empty() {
                current_data.push('\n');
            }
            current_data.push_str(rest.trim_start());
        } else if let Some(rest) = trimmed.strip_prefix("id:") {
            if let Ok(num) = rest.trim().parse::<u64>() {
                current_id = Some(num);
            }
        } else if trimmed.starts_with("retry:") {
            // Ignore retry intervals or handle if needed
        }

        line.clear();
    }

    // Flush any pending trailing block
    if !current_data.is_empty() {
        if let Ok(Some((event, seq))) = parse_sse_block(current_event.as_deref(), &current_data, current_id) {
            on_event(event, seq);
        }
    }

    Ok(())
}

/// Spawns a background worker thread that connects to `GET /api/inbox/events?cursor=n`
/// and continuously delivers `InboxServerEvent`s over a channel, reconnecting on drop.
pub(crate) fn spawn_sse_listener(
    base_url: String,
    initial_cursor: u64,
    tx: Sender<InboxServerEvent>,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut cursor = initial_cursor;
        let client = ureq::builder()
            .timeout_connect(Duration::from_secs(5))
            .timeout_read(Duration::from_secs(45)) // Ping arrives every 15s; 45s detects drop
            .build();

        while !stop.load(Ordering::Relaxed) {
            let url = format!("{base_url}api/inbox/events?cursor={cursor}");
            let resp = client.get(&url).set("Accept", "text/event-stream").call();

            match resp {
                Ok(response) => {
                    let reader = std::io::BufReader::new(response.into_reader());
                    let _ = read_sse_stream(reader, |event, new_seq| {
                        if let Some(s) = new_seq {
                            cursor = s;
                        }
                        let _ = tx.send(event);
                    });
                }
                Err(_) => {
                    // Back off before reconnecting
                    std::thread::sleep(Duration::from_millis(1500));
                }
            }

            if stop.load(Ordering::Relaxed) {
                break;
            }
            // Small pause before reconnecting
            std::thread::sleep(Duration::from_millis(500));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_parse_hello_event() {
        let stream = "event: hello\ndata: {\"serverSession\":\"sess123\",\"cursor\":42}\n\n";
        let mut events = Vec::new();
        read_sse_stream(Cursor::new(stream), |ev, seq| events.push((ev, seq))).expect("read");

        assert_eq!(events.len(), 1);
        let (ev, seq) = &events[0];
        assert_eq!(*seq, Some(42));
        match ev {
            InboxServerEvent::Hello(h) => {
                assert_eq!(h.server_session, "sess123");
                assert_eq!(h.cursor, 42);
            }
            _ => panic!("expected hello"),
        }
    }

    #[test]
    fn test_parse_heartbeat_ping() {
        let stream = ": ping\n\n";
        let mut events = Vec::new();
        read_sse_stream(Cursor::new(stream), |ev, seq| events.push((ev, seq))).expect("read");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, InboxServerEvent::Ping);
    }

    #[test]
    fn test_parse_record_event() {
        let stream = "id: 99\nevent: record\ndata: {\"seq\":99,\"kind\":\"project\",\"id\":\"prj_1\"}\n\n";
        let mut events = Vec::new();
        read_sse_stream(Cursor::new(stream), |ev, seq| events.push((ev, seq))).expect("read");

        assert_eq!(events.len(), 1);
        let (ev, seq) = &events[0];
        assert_eq!(*seq, Some(99));
        match ev {
            InboxServerEvent::Record(r) => {
                assert_eq!(r.seq, 99);
                assert_eq!(r.kind, "project");
                assert_eq!(r.id, "prj_1");
            }
            _ => panic!("expected record"),
        }
    }

    #[test]
    fn test_parse_multiple_events() {
        let stream = ": ping\n\nevent: hello\ndata: {\"serverSession\":\"s\",\"cursor\":1}\n\n: ping\n\n";
        let mut events = Vec::new();
        read_sse_stream(Cursor::new(stream), |ev, _| events.push(ev)).expect("read");

        assert_eq!(events.len(), 3);
        assert_eq!(events[0], InboxServerEvent::Ping);
        match &events[1] {
            InboxServerEvent::Hello(h) => assert_eq!(h.cursor, 1),
            _ => panic!("expected hello"),
        }
        assert_eq!(events[2], InboxServerEvent::Ping);
    }
}
