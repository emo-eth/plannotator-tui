use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct Project {
    pub name: String,
    pub root: String,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Summary {
    pub id: String,
    pub title: String,
    pub project: Project,
    pub thread: Option<String>,
    pub state: String,
    pub updated: String,
    pub latest: u64,
    pub seen: Option<u64>,
    pub changed: bool,
    pub drawing: bool,
    pub waiting: usize,
}

#[derive(Deserialize)]
pub(super) struct ShapeList {
    pub shapes: Vec<Summary>,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Record {
    pub id: String,
    pub title: String,
    pub project: Project,
    pub thread: Option<String>,
    pub state: String,
    pub latest: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Part {
    pub view: String,
    pub id: String,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Reference {
    pub n: u64,
    pub kind: String,
    pub title: String,
    pub target: String,
    pub line: Option<usize>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Version {
    pub n: u64,
    pub at: String,
    pub title: String,
    pub claim: String,
    pub note: Option<String>,
    pub views: Value,
    #[serde(default)]
    pub parts: Vec<Part>,
    #[serde(default)]
    pub refs: Vec<Reference>,
    pub stack: Option<Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Note {
    pub id: String,
    pub at: String,
    pub kind: String,
    pub v: u64,
    pub view: Option<String>,
    pub part: Option<String>,
    pub quote: Option<String>,
    pub text: String,
    pub from: Value,
    pub re: Option<String>,
    pub delivered: Option<String>,
    pub picture: Option<String>,
    pub code: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Session {
    pub state: String,
    pub agent: String,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Loaded {
    pub shape: Record,
    pub version: Version,
    pub seen: Option<u64>,
    pub notes: Vec<Note>,
    pub session: Session,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Stamp {
    pub n: u64,
    pub at: String,
    pub note: Option<String>,
    pub base: Option<u64>,
}

#[derive(Deserialize)]
pub(super) struct History {
    pub versions: Vec<Stamp>,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct SystemProject {
    pub id: String,
    pub name: String,
    pub repo: Option<String>,
    pub state: String,
    pub sheets: usize,
    pub wanted: usize,
}

#[derive(Deserialize)]
pub(super) struct SystemProjects {
    pub projects: Vec<SystemProject>,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Drawing {
    pub project: SystemProject,
    pub model: Option<Value>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub(super) struct Pulls {
    pub pulls: Vec<Touch>,
    pub unread: usize,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Touch {
    pub key: String,
    pub parts: Vec<TouchedPart>,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct TouchedPart {
    pub id: String,
    pub name: String,
}

#[derive(Deserialize)]
pub(super) struct SetupProjects {
    pub projects: Vec<SetupProject>,
}

#[derive(Deserialize)]
pub(super) struct SetupProject {
    pub id: String,
    pub name: String,
    pub root: String,
    pub repo: String,
    pub drawing: String,
    pub setup: Option<Value>,
}

/// Upstream deliberately hides the old What's changed/What to know views.
pub(super) fn visible_part(part: &Part) -> bool {
    matches!(part.view.as_str(), "header" | "picture" | "how")
}

pub(super) fn text<'a>(value: &'a Value, field: &str) -> &'a str {
    value.get(field).and_then(Value::as_str).unwrap_or("")
}
