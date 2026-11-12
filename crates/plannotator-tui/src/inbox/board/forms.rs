use anyhow::{Result, ensure};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use serde_json::{Value, json};

use crate::app::compose::{Compose, ComposeAction};

use super::model::{Card, Column, Initiative, Project, Target, split_refs};

pub(super) enum Modal {
    Form(Form),
    Pick(Picker),
    Confirm { prompt: String, path: String, body: Value },
}

pub(super) struct Picker {
    pub(super) title: String,
    pub(super) items: Vec<(String, Choice)>,
    pub(super) cursor: usize,
}

pub(super) enum Choice {
    Project(Option<String>),
    Filter(Option<String>),
    Move(String, Column),
    Assign(String, Option<String>),
    Thread(String),
    Url(String),
    Initiative(String),
    NewInitiative,
    RenameInitiative(String, String),
    DeleteInitiative(String, String),
    RestoreInitiative(String),
    Target(Target, bool),
}

pub(super) enum FormKind {
    Create { key: String },
    Title(String),
    Notes(String),
    Link(String),
    ThreadLink(String),
    Drop(String),
    InitiativeNew,
    InitiativeRename(String),
    Search,
    Tidy { key: String, project: Option<String>, sessions: Vec<String> },
}

pub(super) struct Form {
    pub(super) title: String,
    pub(super) kind: FormKind,
    pub(super) fields: Vec<Field>,
    pub(super) cursor: usize,
}

pub(super) struct Field {
    pub(super) label: &'static str,
    pub(super) value: FieldValue,
}

pub(super) enum FieldValue {
    Text(Compose),
    Choice { items: Vec<(String, String)>, cursor: usize },
}

pub(super) enum Submission {
    Post { path: String, body: Value },
    Confirm { prompt: String, path: String, body: Value },
    Search(String),
}

impl Field {
    pub(super) fn text(label: &'static str, text: &str) -> Self {
        Self { label, value: FieldValue::Text(Compose::with_text(text)) }
    }

    pub(super) fn choice(label: &'static str, items: Vec<(String, String)>, selected: Option<&str>) -> Self {
        let cursor = selected.and_then(|value| items.iter().position(|(_, id)| id == value)).unwrap_or(0);
        Self { label, value: FieldValue::Choice { items, cursor } }
    }

    pub(super) fn value(&self) -> String {
        match &self.value {
            FieldValue::Text(editor) => editor.value(),
            FieldValue::Choice { items, cursor } => {
                items.get(*cursor).map_or(String::new(), |(_, value)| value.clone())
            }
        }
    }
}

impl Form {
    pub(super) fn single(title: &str, kind: FormKind, label: &'static str, value: &str) -> Self {
        Self { title: title.to_owned(), kind, fields: vec![Field::text(label, value)], cursor: 0 }
    }

    pub(super) fn create(
        projects: &[Project],
        initiatives: &[Initiative],
        project: Option<&str>,
        initiative: Option<&str>,
    ) -> Self {
        let projects = projects
            .iter()
            .map(|project| (format!("{} · {}", project.name, project.root), project.id.clone()))
            .collect();
        let initiatives = std::iter::once(("None".to_owned(), String::new()))
            .chain(
                initiatives
                    .iter()
                    .filter(|initiative| initiative.deleted_at.is_none())
                    .map(|initiative| (initiative.name.clone(), initiative.id.clone())),
            )
            .collect();
        Self {
            title: "New card · Tab changes field · Enter saves".to_owned(),
            kind: FormKind::Create { key: super::super::send::new_key() },
            fields: vec![
                Field::text("Title", ""),
                Field::choice("Project", projects, project),
                Field::text("Notes", ""),
                Field::text("Links (spaces separate URLs)", ""),
                Field::choice(
                    "Column",
                    vec![
                        ("Backlog".to_owned(), "backlog".to_owned()),
                        ("Todo".to_owned(), "todo".to_owned()),
                    ],
                    None,
                ),
                Field::choice("Initiative", initiatives, initiative),
            ],
            cursor: 0,
        }
    }

    pub(super) fn paste(&mut self, text: &str) {
        if let Some(Field { value: FieldValue::Text(editor), .. }) = self.fields.get_mut(self.cursor) {
            editor.insert_text(text);
        }
    }

    pub(super) fn key(&mut self, key: KeyEvent) -> ComposeAction {
        match key.code {
            KeyCode::Tab => {
                self.cursor = (self.cursor + 1) % self.fields.len().max(1);
                ComposeAction::Edited
            }
            KeyCode::BackTab => {
                self.cursor = self.cursor.checked_sub(1).unwrap_or(self.fields.len().saturating_sub(1));
                ComposeAction::Edited
            }
            KeyCode::Esc => ComposeAction::Cancel,
            _ => match self.fields.get_mut(self.cursor).map(|field| &mut field.value) {
                Some(FieldValue::Text(editor)) => editor.handle_key(key),
                Some(FieldValue::Choice { items, cursor }) => {
                    match key.code {
                        KeyCode::Left | KeyCode::Up => {
                            *cursor = cursor.checked_sub(1).unwrap_or(items.len().saturating_sub(1));
                        }
                        KeyCode::Right | KeyCode::Down | KeyCode::Char(' ') => {
                            *cursor = (*cursor + 1) % items.len().max(1);
                        }
                        KeyCode::Enter => return ComposeAction::Save,
                        _ => {}
                    }
                    ComposeAction::Edited
                }
                None => ComposeAction::Edited,
            },
        }
    }

    fn value(&self, index: usize) -> String {
        self.fields.get(index).map_or(String::new(), Field::value)
    }

    pub(super) fn submit(&self) -> Result<Submission> {
        let text = self.value(0);
        let text = text.trim();
        let post = |path: String, body: Value| Ok(Submission::Post { path, body });
        let card_path = |id: &str| format!("/api/inbox/cards/{id}");
        match &self.kind {
            FormKind::Create { key } => {
                validate_line(text, 120, "Title")?;
                let project = self.value(1);
                ensure!(
                    !project.is_empty(),
                    "Choose a project first; the Inbox must know at least one project"
                );
                let notes = self.value(2);
                ensure!(notes.chars().count() <= 4000, "Notes: at most 4000 characters");
                let initiative = self.value(5);
                post(
                    "/api/inbox/cards".to_owned(),
                    json!({"title":text, "project":project, "notes":notes,
                    "refs":split_refs(&self.value(3)), "column":self.value(4), "idempotency_key":key,
                    "initiative": if initiative.is_empty() {None} else {Some(initiative)}}),
                )
            }
            FormKind::Title(id) => {
                validate_line(text, 120, "Title")?;
                post(card_path(id), json!({"op":"edit","title":text}))
            }
            FormKind::Notes(id) => {
                ensure!(text.chars().count() <= 4000, "Notes: at most 4000 characters");
                post(card_path(id), json!({"op":"edit","notes":text}))
            }
            FormKind::Link(id) => {
                ensure!(!text.is_empty(), "Enter an issue, PR, or web link");
                post(card_path(id), json!({"op":"link","ref":text}))
            }
            FormKind::ThreadLink(id) => {
                super::api::id(text)?;
                post(card_path(id), json!({"op":"link","thread_id":text}))
            }
            FormKind::Drop(id) => {
                ensure!(text.chars().count() <= 500, "Cancellation reason: at most 500 characters");
                Ok(Submission::Confirm {
                    prompt: "Cancel this card? y confirms; n / Esc keeps it".to_owned(),
                    path: card_path(id),
                    body: json!({"op":"drop","reason":text}),
                })
            }
            FormKind::InitiativeNew => {
                validate_line(text, 40, "Initiative")?;
                post("/api/inbox/initiatives".to_owned(), json!({"name":text}))
            }
            FormKind::InitiativeRename(id) => {
                validate_line(text, 40, "Initiative")?;
                post(format!("/api/inbox/initiatives/{id}"), json!({"op":"rename","name":text}))
            }
            FormKind::Search => Ok(Submission::Search(text.to_owned())),
            FormKind::Tidy { key, project, sessions } => {
                let selected: Vec<_> = sessions
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| self.value(*index) == "yes")
                    .map(|(_, session)| session)
                    .collect();
                ensure!(!selected.is_empty(), "Select at least one agent to ask");
                Ok(Submission::Confirm {
                    prompt: format!("Ask {} agents to tidy this Board? y sends; n cancels", selected.len()),
                    path: "/api/inbox/tidy".to_owned(),
                    body: json!({"sessions":selected,"idempotency_key":key,"project":project,"surface":"board"}),
                })
            }
        }
    }
}

fn validate_line(text: &str, max: usize, name: &str) -> Result<()> {
    ensure!(!text.is_empty(), "{name} cannot be empty");
    ensure!(text.chars().count() <= max, "{name}: at most {max} characters");
    ensure!(!text.contains(['\n', '\r']), "{name} must be one line");
    Ok(())
}

pub(super) fn card_form(card: &Card, key: char) -> Option<Form> {
    let id = card.id.clone();
    Some(match key {
        'e' => Form::single("Rename card", FormKind::Title(id), "Title", &card.title),
        'N' => Form::single(
            "Edit notes · Alt+Enter newline · empty clears",
            FormKind::Notes(id),
            "Notes",
            card.notes.as_deref().unwrap_or(""),
        ),
        'l' => Form::single("Link issue, PR, or web URL", FormKind::Link(id), "Reference", ""),
        'L' => Form::single("Link an existing Inbox thread", FormKind::ThreadLink(id), "Thread id", ""),
        'd' => Form::single("Cancel card", FormKind::Drop(id), "Reason (optional)", ""),
        _ => return None,
    })
}
