use anyhow::{Result, ensure};
use serde_json::Value;

use super::super::client::Client;
use super::Board;
use super::forms::{Field, Form, FormKind, Modal};

impl Board {
    pub(super) fn open_tidy(&mut self, client: &Client) -> Result<()> {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("surface", "board");
        if let Some(project) = &self.project {
            query.append_pair("project", project);
        }
        let plan: Value = client.get_json("GET", &format!("/api/inbox/tidy?{}", query.finish()))?;
        let agents = plan.get("agents").and_then(Value::as_array);
        let mut sessions = Vec::new();
        let mut fields = Vec::new();
        for agent in agents.into_iter().flatten() {
            let Some(session) = agent.get("session").and_then(Value::as_str) else {
                continue;
            };
            let name = agent.get("name").and_then(Value::as_str).unwrap_or(session);
            let reach = agent.get("reach").and_then(Value::as_str).unwrap_or("next_call");
            let items = agent.get("items").and_then(Value::as_array).map_or(0, Vec::len);
            fields.push(Field::choice(
                "Ask agent (Tab next; arrows toggle)",
                vec![
                    (format!("Yes · {name} · {items} items · {reach}"), "yes".to_owned()),
                    (format!("No · {name}"), "no".to_owned()),
                ],
                None,
            ));
            sessions.push(session.to_owned());
        }
        ensure!(!sessions.is_empty(), "No agents have Board work that needs tidying in this project scope");
        if let Some(run) = plan.get("run").filter(|run| !run.is_null()) {
            self.status = Some(format!("Previous Tidy: {run}"));
        }
        self.modal = Some(Modal::Form(Form {
            title: "Tidy Board · choose agents; Enter previews confirmation".to_owned(),
            kind: FormKind::Tidy {
                key: super::super::send::new_key(),
                project: self.project.clone(),
                sessions,
            },
            fields,
            cursor: 0,
        }));
        Ok(())
    }
}
