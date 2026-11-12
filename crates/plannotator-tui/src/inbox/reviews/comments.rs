use super::model::{Record, segment};
use super::{Mode, ReviewPane, Side, feedback};
use crate::app::compose::Compose;
use crate::inbox::{client::Client, send::new_key};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

impl ReviewPane {
    pub(super) fn new_comment(&mut self, global: bool, suggestion: bool) -> Result<()> {
        self.writable()?;
        let mut annotation = if global {
            json!({"scope":"general", "filePath":"", "lineStart":0, "lineEnd":0, "side":"new"})
        } else {
            self.selection_annotation()?
        };
        let fields = annotation.as_object_mut().context("Review annotation must be an object")?;
        fields.insert("id".into(), Value::String(new_key()));
        fields.insert("type".into(), Value::String(if suggestion { "suggestion" } else { "comment" }.into()));
        fields.insert(
            "createdAt".into(),
            json!(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis())
            ),
        );
        self.compose = Compose::default();
        self.mode = if suggestion { Mode::Suggestion(annotation) } else { Mode::Comment(annotation) };
        Ok(())
    }

    pub(super) fn selection_annotation(&self) -> Result<Value> {
        let first = self.selection.unwrap_or(self.cursor).min(self.cursor);
        let last = self.selection.unwrap_or(self.cursor).max(self.cursor);
        let start = self
            .rows
            .get(first)
            .and_then(|row| row.location.as_ref())
            .context("Select a code line first (j/k to move, v to select a range)")?;
        let side = if self.side == Side::Old || start.new.is_none() { "old" } else { "new" };
        let number = |line: &super::Location| if side == "old" { line.old } else { line.new };
        let start_number =
            number(start).context("This line is absent on the selected side; o switches sides")?;
        let mut end_number = start_number;
        let mut text = Vec::new();
        for row in self.rows.get(first..=last).unwrap_or_default() {
            let Some(line) = &row.location else {
                ensure!(first == last, "A comment must stay inside one diff hunk or expanded file side");
                continue;
            };
            ensure!(
                line.file == start.file && line.hunk == start.hunk && line.outside == start.outside,
                "A comment must stay inside one file and hunk"
            );
            if let Some(at) = number(line) {
                ensure!(
                    text.is_empty() || at == end_number + 1,
                    "Selected lines are not contiguous on the {side} side"
                );
                end_number = at;
                text.push(line.text.as_str());
            }
        }
        let path = &self.files.get(start.file).context("Review file no longer exists")?.path;
        let mut annotation = json!({"scope":"line", "filePath":path, "lineStart":start_number, "lineEnd":end_number, "side":side});
        let text = text.join("\n");
        let fields = annotation.as_object_mut().context("Review annotation must be an object")?;
        fields.insert("originalCode".into(), Value::String(text.clone()));
        if start.outside {
            fields.insert("outsideDiff".into(), Value::Bool(true));
            fields.insert("anchorText".into(), Value::String(text));
        } else {
            fields.insert("selectedText".into(), Value::String(text));
        }
        Ok(annotation)
    }

    pub(super) fn save_comment(
        &mut self,
        client: &Client,
        mut annotation: Value,
        suggestion: bool,
    ) -> Result<()> {
        self.writable()?;
        let text = self.compose.value();
        ensure!(!text.trim().is_empty(), "A comment or suggestion cannot be empty");
        annotation
            .as_object_mut()
            .context("Review annotation must be an object")?
            .insert(if suggestion { "suggestedCode" } else { "text" }.into(), Value::String(text));
        let response: Value =
            Self::post(client, &self.guide_path("/annotations"), json!({"annotation":annotation}))?;
        let record: Record = serde_json::from_value(
            response.get("annotation").cloned().context("Inbox omitted the saved comment")?,
        )?;
        self.records.retain(|old| old.id != record.id);
        self.records.push(record);
        self.mode = Mode::Browse;
        self.selection = None;
        self.status = "Comment saved with the thread; s sends it to the agent".into();
        Ok(())
    }

    pub(super) fn edit_comment(&mut self, suggestion: bool) -> Result<()> {
        self.writable()?;
        let record = self.records.get(self.selected_comment).context("No comment selected")?;
        ensure!(
            !suggestion || feedback::field(&record.annotation, "scope") != "general",
            "Global comments cannot suggest code"
        );
        self.compose = Compose::with_text(feedback::field(
            &record.annotation,
            if suggestion { "suggestedCode" } else { "text" },
        ));
        self.mode = if suggestion {
            Mode::Suggestion(record.annotation.clone())
        } else {
            Mode::Comment(record.annotation.clone())
        };
        Ok(())
    }

    pub(super) fn remove_comment(&mut self, client: &Client, id: &str) -> Result<()> {
        self.writable()?;
        let _: Value =
            Self::post(client, &format!("/api/inbox/annotations/{}/remove", segment(id)), json!({}))?;
        self.records.retain(|record| record.id != id);
        self.selected_comment = self.selected_comment.min(self.records.len().saturating_sub(1));
        self.mode = Mode::Comments;
        self.status = "Comment removed".into();
        Ok(())
    }

    pub(super) fn show_comment(&mut self) -> Result<()> {
        let record = self.records.get(self.selected_comment).context("No comment selected")?;
        let annotation = &record.annotation;
        let path = feedback::field(annotation, "filePath").to_owned();
        let side = feedback::field(annotation, "side").to_owned();
        let line = annotation.get("lineStart").and_then(Value::as_u64).unwrap_or_default();
        self.mode = Mode::Browse;
        if path.is_empty() {
            self.cursor = 0;
            return Ok(());
        }
        self.jump_to(&path, usize::try_from(line)?, side == "old")
    }
}
