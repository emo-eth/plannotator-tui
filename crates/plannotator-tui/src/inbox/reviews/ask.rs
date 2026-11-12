use super::{ReviewAction, ReviewPane, feedback, model};
use crate::inbox::client::Client;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

impl ReviewPane {
    pub(super) fn ask_selection(&self) -> Result<ReviewAction> {
        let annotation = self.selection_annotation()?;
        let start = annotation.get("lineStart").context("Selection omitted its first line")?;
        let end = annotation.get("lineEnd").context("Selection omitted its last line")?;
        let words = format!(
            "About {}:{start}–{end} ({}) in review \"{}\":\n\n{}\n\n",
            feedback::field(&annotation, "filePath"),
            feedback::field(&annotation, "side"),
            feedback::title_line(&self.message.guide.title),
            feedback::field(&annotation, "originalCode")
        );
        Ok(ReviewAction::Ask { words, show_me: None })
    }

    pub(super) fn show_me(&self, client: &Client, overview: bool) -> Result<ReviewAction> {
        self.writable()?;
        ensure!(self.code_review && self.message.guide.review, "Show me requires a code review");
        let settings: Value = client.get_json("GET", "/api/inbox/settings")?;
        ensure!(
            settings.pointer("/shape/on").and_then(Value::as_bool) == Some(true),
            "Shape is off in Inbox Settings"
        );
        let thread: Value =
            client.get_json("GET", &format!("/api/inbox/threads/{}", model::segment(&self.thread_id)))?;
        let messages = model::links(thread.get("thread").context("Inbox omitted thread")?)?;
        let round = 1 + messages
            .iter()
            .take_while(|message| message.id != self.message.id)
            .filter(|message| message.guide.review)
            .count();
        let (target, where_text) = if overview {
            (
                json!({"review":self.message.id,"round":round,"file":null,"lines":null}),
                "on the Overview of".into(),
            )
        } else {
            let selection = self.selection_annotation()?;
            ensure!(
                feedback::field(&selection, "side") == "new",
                "Show me uses new-file line numbers; use Ask for removed lines"
            );
            let file = selection.get("filePath").context("Selection omitted its file")?;
            let start = selection.get("lineStart").context("Selection omitted its first line")?;
            let end = selection.get("lineEnd").context("Selection omitted its last line")?;
            let range = if start == end { start.to_string() } else { format!("{start}–{end}") };
            let target = json!({"review":self.message.id,"round":round,"file":file,"lines":[start,end]});
            let description = format!(
                "on {} {range} of {} in",
                if start == end { "line" } else { "lines" },
                feedback::field(&selection, "filePath")
            );
            (target, description)
        };
        let title =
            feedback::title_line(&self.message.guide.title).replace('\\', "\\\\").replace('"', "\\\"");
        let words = format!(
            "The reviewer pressed Show me {where_text} your review \"{title}\" ({}).\nMake a Shape of it, so they can see it the way you see it. `plannotator shape --help` has how.\nStore it with `--thread {} --through {}`: it opens over the review by itself, so don't open a page.\nOn each part that is code, put `data-code=\"path#L42\"` (or `#L42-L48`) with the review's paths and the new file's line numbers: a click lands on those lines of the diff.\nThen answer here in one or two sentences.",
            self.message.id, self.thread_id, self.message.id
        );
        Ok(ReviewAction::Ask { words, show_me: Some(target) })
    }
}
