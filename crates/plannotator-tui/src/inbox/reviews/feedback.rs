//! The review editor's feedback format, preserving old/new coordinates and suggestion text.
use super::model::Record;
use serde_json::Value;
use std::fmt::Write as _;

pub(crate) fn guide_feedback(records: &[Value]) -> String {
    let records: Vec<Record> = records
        .iter()
        .filter_map(|value| serde_json::from_value::<Record>(value.clone()).ok())
        .filter(|record| record.guide.is_some())
        .collect();
    let mut groups: Vec<(&str, Vec<&Record>)> = Vec::new();
    for record in &records {
        if let Some((_, group)) = groups.iter_mut().find(|(id, _)| *id == record.message_id) {
            group.push(record);
        } else {
            groups.push((&record.message_id, vec![record]));
        }
    }
    groups
        .into_iter()
        .map(|(_, records)| {
            let title = records
                .first()
                .and_then(|record| record.guide.as_ref())
                .and_then(|guide| guide.get("title"))
                .and_then(Value::as_str)
                .unwrap_or("Guided review");
            export(title, &records)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub(super) fn title_line(title: &str) -> String {
    let line = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= 120 {
        line
    } else {
        format!("{}…", line.chars().take(119).collect::<String>())
    }
}

pub(super) fn export(title: &str, records: &[&Record]) -> String {
    if records.is_empty() {
        return String::new();
    }
    let title = title_line(title);
    let mut globals: Vec<_> =
        records.iter().filter(|record| field(&record.annotation, "scope") == "general").collect();
    globals
        .sort_by_key(|record| record.annotation.get("createdAt").and_then(Value::as_u64).unwrap_or_default());
    let has_lines = records.iter().any(|record| field(&record.annotation, "scope") != "general");
    let intro = if globals.is_empty() {
        format!(
            "The following comments are on the diffs of the guided review \"{title}\" you sent, one section per file."
        )
    } else if !has_lines {
        format!(
            "The following comments are on the guided review \"{title}\" you sent, under \"Global comments\": they are on the whole review, not on a line or a file."
        )
    } else {
        format!(
            "The following comments are on the guided review \"{title}\" you sent: first the global comments, on the whole review, then the comments on its diffs, one section per file."
        )
    };
    let mut out = format!("# Feedback on the guided review\n\n{intro}");
    if has_lines {
        out.push_str(" Line numbers are the file's own: (new) the changed file, (old) the original.");
    }
    out.push_str("\n\n");
    if !globals.is_empty() {
        out.push_str("## Global comments\n\n");
        for record in globals {
            let a = &record.annotation;
            let _ = writeln!(out, "{}{}\n", prefix(a), field(a, "text"));
        }
    }
    let mut files: Vec<(&str, Vec<&Record>)> = Vec::new();
    for record in records.iter().filter(|r| field(&r.annotation, "scope") != "general") {
        let path = field(&record.annotation, "filePath");
        if let Some((_, list)) = files.iter_mut().find(|(name, _)| *name == path) {
            list.push(record);
        } else {
            files.push((path, vec![record]));
        }
    }
    for (path, mut records) in files {
        let _ = writeln!(out, "## {path}\n");
        records.sort_by_key(|r| number(&r.annotation, "lineStart"));
        for record in records {
            let a = &record.annotation;
            let (start, end) = (number(a, "lineStart"), number(a, "lineEnd"));
            let range = if start == end { format!("Line {start}") } else { format!("Lines {start}-{end}") };
            let _ = write!(out, "### {range} ({})", field(a, "side"));
            let token = field(a, "tokenText");
            if !token.is_empty() {
                let _ = write!(out, " — ``{}``", token.replace('`', "\\`"));
                if let (Some(start), Some(end)) = (a.get("charStart"), a.get("charEnd")) {
                    let _ = write!(out, " (chars {start}-{end})");
                }
            }
            let outside = a.get("outsideDiff").and_then(Value::as_bool) == Some(true);
            if outside {
                out.push_str(" (not in the diff)");
            }
            out.push('\n');
            if a.get("outdated").and_then(Value::as_bool) == Some(true) {
                out.push_str("[Outdated — the code changed after this comment]\n");
            }
            let _ = writeln!(out, "{}{}", prefix(a), field(a, "text"));
            quote(&mut out, "Highlighted text", field(a, "selectedText"));
            if outside {
                quote(
                    &mut out,
                    "Code",
                    a.get("anchorText").and_then(Value::as_str).unwrap_or_else(|| field(a, "originalCode")),
                );
            }
            let suggestion = field(a, "suggestedCode");
            if !outside || !suggestion.is_empty() {
                if !suggestion.is_empty() || !field(a, "text").is_empty() {
                    quote(&mut out, "Replaces", field(a, "originalCode"));
                }
                quote(&mut out, "Suggested code", suggestion);
            }
            out.push('\n');
        }
    }
    out.trim().into()
}

pub(super) fn verdict_words(approved: bool, title: &str, comments: usize) -> String {
    let title = title_line(title);
    match (approved, comments) {
        (true, 0) => format!("I approved your review \"{title}\"."),
        (true, n) => format!(
            "I approved your review \"{title}\", with {} below. Nothing in it blocks the change.",
            if n == 1 { "a comment".into() } else { format!("{n} comments") }
        ),
        (false, 0) => format!("I reviewed \"{title}\" and I am asking for changes."),
        (false, n) => format!(
            "I reviewed \"{title}\" and I am asking for changes: {} below.",
            if n == 1 { "my comment is".into() } else { format!("my {n} comments are") }
        ),
    }
}

fn prefix(a: &Value) -> String {
    let label = field(a, "conventionalLabel");
    if label.is_empty() {
        return String::new();
    }
    let decorations: Vec<_> = a
        .get("decorations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if decorations.is_empty() {
        format!("**{label}:** ")
    } else {
        format!("**{label} ({}):** ", decorations.join(", "))
    }
}
fn quote(out: &mut String, label: &str, code: &str) {
    if code.is_empty() {
        return;
    }
    let fence = "`".repeat(code.split(|c| c != '`').map(str::len).max().unwrap_or_default().max(2) + 1);
    let _ = write!(out, "\n**{label}:**\n{fence}\n{code}\n{fence}\n");
}
pub(super) fn field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}
fn number(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn global_comments_precede_lines_and_export_cannot_close_its_code_fence() {
        let records = vec![
            json!({"id":"line","message_id":"m","guide":{"title":"One\n\n#Injected"},"annotation":{"scope":"line","filePath":"x.rs","lineStart":2,"lineEnd":3,"side":"old","text":"Keep it", "selectedText":"```\ncode"}}),
            json!({"id":"g","message_id":"m","guide":{"title":"One"},"annotation":{"scope":"general","text":"Split it", "createdAt":1}}),
        ];
        let text = guide_feedback(&records);
        assert!(text.contains("\"One #Injected\""));
        assert!(text.contains("### Lines 2-3 (old)"));
        assert!(text.contains("````\n```\ncode\n````"));
        assert!(text.find("## Global comments") < text.find("## x.rs"));
        assert!(!text.contains("Line 0"));
    }
    #[test]
    fn approval_notes_are_explicitly_nonblocking() {
        assert!(verdict_words(true, "Change", 1).ends_with("Nothing in it blocks the change."));
        assert!(verdict_words(false, "Change", 0).contains("asking for changes"));
    }
}
