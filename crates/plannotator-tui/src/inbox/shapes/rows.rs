use serde_json::Value;

use super::model::{Drawing, Loaded, Pulls, Stamp, text, visible_part};
use super::{Opened, Row, Shapes, Tab, Target};

impl Shapes {
    pub(super) fn rebuild(&mut self) {
        let keep = self.rows.get(self.selected).map(|row| row.label.clone());
        self.rows = match &self.opened {
            Some(Opened::Shape { loaded, history }) => shape_rows(loaded, history, self.page),
            Some(Opened::System { drawing, pulls }) => system_rows(drawing, pulls, self.page),
            None if self.tab == Tab::Shapes => self.boards.iter().filter(|shape| {
                !self.filter_thread || shape.thread.as_deref() == self.thread.as_deref()
            }).map(|shape| Row {
                label: format!("{}{}{} · {} · v{}", if shape.changed { "* " } else { "" }, if shape.drawing { "[drawing] " } else { "" }, shape.title, shape.project.name, shape.latest),
                detail: format!("{}\n{}\nProject: {}\n{}\nState: {}\nUpdated: {}\nLatest: {} · seen: {} · {} changes waiting\nThread: {}\n\nEnter reads the board. o opens the upstream graphical viewer.\nReads never mark a board seen; v does so after confirmation.", shape.title, shape.id, shape.project.name, shape.project.root, shape.state, shape.updated, shape.latest, shape.seen.map_or_else(|| "never".to_owned(), |n| n.to_string()), shape.waiting, shape.thread.as_deref().unwrap_or("standalone")),
                target: Target::Shape(shape.id.clone()),
            }).collect(),
            None => self.systems.iter().map(|project| Row {
                label: format!("{} · {} · {} sheets", project.name, project.state, project.sheets),
                detail: format!("{}\n{}\nRepo: {}\nState: {}\n{} drawn sheets; {} wanted\n\nEnter reads the drawing. e edits project setup.\nSystem has no graphic-edit HTTP endpoint; o opens its upstream viewer.\nSaving setup does not start drawing: the upstream first-draw launcher is not implemented.", project.name, project.id, project.repo.as_deref().unwrap_or("none"), project.state, project.sheets, project.wanted),
                target: Target::System(project.id.clone()),
            }).collect(),
        };
        self.selected = keep
            .and_then(|label| self.rows.iter().position(|row| row.label == label))
            .unwrap_or(self.selected)
            .min(self.rows.len().saturating_sub(1));
    }

    pub(super) fn page_names(&self) -> &'static [&'static str] {
        match &self.opened {
            Some(Opened::Shape { .. }) => {
                &["Overview", "Parts", "Notes", "References", "History", "Examples", "Stack"]
            }
            Some(Opened::System { .. }) => {
                &["Sheets", "Parts", "Flows", "Revisions", "Sources", "Open PRs", "Wanted"]
            }
            None => &[],
        }
    }
}

fn row(label: impl Into<String>, detail: impl Into<String>) -> Row {
    Row { label: label.into(), detail: detail.into(), target: Target::None }
}

fn pretty(value: &impl serde::Serialize) -> String {
    serde_json::to_string_pretty(value)
        .unwrap_or_else(|error| format!("Cannot display structured data: {error}"))
}

fn shape_rows(loaded: &Loaded, history: &[Stamp], page: usize) -> Vec<Row> {
    let version = &loaded.version;
    match page {
        0 => vec![row(&version.title, format!("{}\n\n{}\n\nVersion {} · {}\n{}\nProject: {} ({})\nState: {} · latest {} · seen {}\nAgent: {} ({})\n\nDrawing parts and examples are available as text on pages 2 and 6.\nPress o for the actual HTML/SVG, pan/zoom, scene player, graphical selection and code-linked parts.\nPress e to request an edit from its drawing agent; this is not local canvas editing.\nPress a to ask a question.\n\nAgent pickup command:\nplannotator shape read {} --notes", version.title, version.claim, version.n, version.at, version.note.as_deref().unwrap_or(""), loaded.shape.project.name, loaded.shape.project.root, loaded.shape.state, loaded.shape.latest, loaded.seen.map_or_else(|| "never".to_owned(), |n| n.to_string()), loaded.session.agent, loaded.session.state, loaded.shape.id))],
        1 => version.parts.iter().filter(|part| visible_part(part)).map(|part| Row {
            label: format!("{} / {} · {}", part.view, part.id, part.text),
            detail: format!("{} / {}\n\n{}\n\n{}", part.view, part.id, part.text, if part.view == "header" { "Header text: e/a comment on the board, because the API does not accept a header-view anchor." } else { "c/e requests a change to this part; a asks about it. The exact view, id, version and quoted words are sent." }),
            target: Target::Part { view: part.view.clone(), id: part.id.clone(), quote: part.text.clone() },
        }).collect(),
        2 => loaded.notes.iter().map(|note| {
            let author = if note.from.as_str() == Some("you") { "you".to_owned() } else { format!("{}:{}", text(&note.from, "host"), text(&note.from, "id")) };
            row(format!("{} · v{} · {} · {}", note.kind, note.v, author, note.text.lines().next().unwrap_or("")), format!("{} · {}\n{} · v{} · {}\nView: {} · part: {}\nQuote: {}\n\n{}\n\nReply to: {}\nDelivery: {}\nPicture: {}\nCode: {}", note.id, note.at, note.kind, note.v, author, note.view.as_deref().unwrap_or("whole board"), note.part.as_deref().unwrap_or("whole view"), note.quote.as_deref().unwrap_or(""), note.text, note.re.as_deref().unwrap_or("none"), note.delivered.as_deref().unwrap_or("not picked up"), note.picture.as_deref().unwrap_or("none"), note.code.as_ref().map_or_else(String::new, |places| places.join(", "))))
        }).collect(),
        3 => version.refs.iter().map(|reference| Row {
            label: format!("[{}] {} · {}", reference.n, reference.kind, reference.title),
            detail: format!("{}\n{}\n{}{}\n\n{}\n\nEnter opens a file in the terminal; images/links/PRs in the browser. Message/diff references open the real board's reference viewer.", reference.title, reference.kind, reference.target, reference.line.map_or(String::new(), |line| format!(":{line}")), pretty(&reference.extra)),
            target: Target::Reference(reference.clone()),
        }).collect(),
        4 => history.iter().rev().map(|stamp| Row {
            label: format!("v{} · {} · {}", stamp.n, stamp.at, stamp.note.as_deref().unwrap_or("")),
            detail: format!("Version {}\n{}\nBase: {}\n\n{}\n\nEnter reads this exact saved version. Feedback remains anchored to its version.\nR keeps a historical version open; 5 then select the latest to return.", stamp.n, stamp.at, stamp.base.map_or_else(|| "first version".to_owned(), |n| n.to_string()), stamp.note.as_deref().unwrap_or("")),
            target: Target::Version(stamp.n),
        }).collect(),
        5 => version.views.pointer("/how/examples").and_then(Value::as_array).into_iter().flatten().map(|example| {
            row(format!("{} · {}", text(example, "name"), text(example, "title")), pretty(example))
        }).collect(),
        6 => version.stack.as_ref().map(|stack| vec![row("Pull request stack · parts and changes", pretty(stack))]).unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn system_rows(drawing: &Drawing, pulls: &Pulls, page: usize) -> Vec<Row> {
    if page == 5 {
        let mut rows: Vec<Row> = pulls.pulls.iter().map(|pull| Row {
            label: format!("{} · {}", pull.key, pull.parts.iter().map(|part| part.name.as_str()).collect::<Vec<_>>().join(", ")),
            detail: format!("Open PR that changes this drawing\n{}\n\n{}\n\nEnter opens this exact PR in the upstream Reviews pane.", pull.key, pull.parts.iter().map(|part| format!("{} · {}", part.id, part.name)).collect::<Vec<_>>().join("\n")),
            target: Target::Pull(pull.key.clone()),
        }).collect();
        if pulls.unread > 0 {
            rows.push(row("PRs whose files could not be read", format!("{} PRs could not be checked. They are not treated as unrelated; refresh to retry the server read.", pulls.unread)));
        }
        return rows;
    }
    let Some(model) = &drawing.model else {
        return vec![row(
            "No readable drawing",
            format!(
                "{}: {}\n\ne edits setup; o opens the upstream System pane.\nNo graphical edit or first-draw launch is available from the upstream HTTP API.",
                drawing.project.name, drawing.project.state
            ),
        )];
    };
    if page == 4 {
        return vec![row(
            format!("{} · sources and structure", text(model, "name")),
            pretty(&serde_json::json!({
                "name": model.get("name"), "root": model.get("root"), "sources": model.get("sources"),
                "scopes": model.get("scopes"), "parentOf": model.get("parentOf"), "owner": model.get("owner"),
                "byScope": model.get("byScope"), "watch": model.get("watch")
            })),
        )];
    }
    if page == 6 {
        return model
            .get("wanted")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|wanted| {
                row(format!("{} · {}", text(wanted, "code"), text(wanted, "question")), pretty(wanted))
            })
            .collect();
    }
    let field = match page {
        0 => "sheets",
        1 => "parts",
        2 => "flows",
        3 => "revisions",
        _ => return Vec::new(),
    };
    model
        .get(field)
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(key, value)| {
            let name = if page == 1 {
                text(value, "name")
            } else if page == 2 {
                text(value, "says")
            } else {
                text(value, "title")
            };
            Row {
                label: format!("{key} · {name}"),
                detail: pretty(value),
                target: if page == 0 { Target::Sheet(key.clone()) } else { Target::None },
            }
        })
        .collect()
}
