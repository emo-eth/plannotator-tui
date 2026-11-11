//! Small text helpers shared by the list and the thread: who wrote a message, and when.

use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::client::Author;

/// Who a message is from, as a short label: `you`, the agent's name, else its host.
pub(crate) fn author(author: &Author) -> String {
    if author.kind == "person" {
        return "you".to_owned();
    }
    author
        .name
        .as_deref()
        .or(author.host.as_deref())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("agent")
        .to_owned()
}

/// How long ago an RFC 3339 instant was, in the shortest useful unit; the date once it is
/// over a week old, and the raw text when it does not parse.
pub(crate) fn ago(iso: &str, now: OffsetDateTime) -> String {
    let Ok(at) = OffsetDateTime::parse(iso, &Rfc3339) else {
        return iso.to_owned();
    };
    let seconds = (now - at).whole_seconds();
    if seconds < 60 {
        "now".to_owned()
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3600)
    } else if seconds < 7 * 86_400 {
        format!("{}d", seconds / 86_400)
    } else {
        let date = at.date();
        format!("{:04}-{:02}-{:02}", date.year(), u8::from(date.month()), date.day())
    }
}

/// A row's one-line title: its subject, else its thread name, else the id's tail.
pub(crate) fn title(subject: Option<&str>, thread_name: Option<&str>, thread_id: &str) -> String {
    subject.or(thread_name).map(str::trim).filter(|s| !s.is_empty()).map_or_else(
        || {
            format!(
                "(untitled {})",
                thread_id.chars().rev().take(6).collect::<String>().chars().rev().collect::<String>()
            )
        },
        str::to_owned,
    )
}
