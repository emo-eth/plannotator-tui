use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use serde_json::{Map, Value, json};

use super::super::{client::Client, settings};

#[derive(Debug)]
struct Changed {
    before: Value,
    wrote: Value,
}

#[derive(Debug)]
pub(super) struct Appearance {
    pub model: Value,
    changed: BTreeMap<&'static str, Changed>,
}

impl Appearance {
    pub(super) fn load(client: &Client) -> Result<Self> {
        let model = settings::read(client)?;
        ensure!(
            model.get("tour").is_some_and(Value::is_object),
            "This Inbox does not provide the tour settings contract; update the Inbox first"
        );
        Ok(Self { model, changed: BTreeMap::new() })
    }

    pub(super) fn value(&self, field: &str) -> Value {
        field_value(&self.model, field)
    }

    pub(super) fn choose(&mut self, client: &Client, field: &'static str, direction: isize) -> Result<()> {
        let latest = settings::read(client)?;
        let options = match field {
            "mode" => vec![json!("light"), json!("dark"), json!("system")],
            "light" => vec![json!("plannotator"), json!("plannotator-modern"), json!("catppuccin")],
            "size" => vec![json!(12), json!(13), json!(14), json!(15)],
            _ => anyhow::bail!("Unknown tour appearance field"),
        };
        let old = field_value(&latest, field);
        let at = options.iter().position(|option| option == &old).unwrap_or(0);
        let next = (at as isize + direction).rem_euclid(options.len() as isize) as usize;
        let value = options.get(next).context("No appearance option")?.clone();
        let fields: &[&'static str] =
            if field == "light" { &["light", "dark"] } else { std::slice::from_ref(&field) };
        let mut patch = Map::new();
        for &name in fields {
            let before = field_value(&latest, name);
            self.changed
                .entry(name)
                .and_modify(|changed| changed.wrote = value.clone())
                .or_insert_with(|| Changed { before, wrote: value.clone() });
            patch_field(&mut patch, name, value.clone());
        }
        let answer = write(client, Value::Object(patch))?;
        self.model = latest;
        merge(&mut self.model, answer);
        Ok(())
    }

    pub(super) fn restore(&mut self, client: &Client) -> Result<()> {
        if self.changed.is_empty() {
            return Ok(());
        }
        let latest = settings::read(client).context("Reading appearance before restoring tour changes")?;
        let patch = restore_patch(&self.changed, &latest);
        self.model = latest;
        if !patch.as_object().is_none_or(Map::is_empty) {
            merge(&mut self.model, write(client, patch)?);
        }
        self.changed.clear();
        Ok(())
    }

    pub(super) fn stamp(&mut self, client: &Client, decision: &'static str) -> Result<()> {
        let change = if decision == "finished" {
            json!({"finished": true, "notify_again": true})
        } else {
            json!({(decision): true})
        };
        let answer = write(client, json!({"tour": change}))?;
        let state = answer.get("tour").context("Tour settings response omitted tour state")?;
        let field = format!("{decision}_at");
        ensure!(
            state.get(&field).and_then(Value::as_str).is_some_and(|value| !value.is_empty()),
            "Tour settings response did not confirm {decision}"
        );
        merge(&mut self.model, answer);
        Ok(())
    }
}

fn write(client: &Client, mut patch: Value) -> Result<Value> {
    patch
        .as_object_mut()
        .context("Tour settings patch is not an object")?
        .insert("serverSession".to_owned(), json!(client.server_session()));
    client.post_json("/api/inbox/settings", &patch).context("Saving Inbox tour settings")
}

fn field_value(model: &Value, field: &str) -> Value {
    if field == "size" {
        model.get("text_size").filter(|value| !value.is_null()).cloned().unwrap_or(json!(13))
    } else {
        model
            .get("theme")
            .and_then(|theme| theme.get(field))
            .filter(|value| !value.is_null())
            .cloned()
            .unwrap_or_else(|| json!(if field == "mode" { "system" } else { "plannotator" }))
    }
}

fn patch_field(patch: &mut Map<String, Value>, field: &str, value: Value) {
    if field == "size" {
        patch.insert("text_size".to_owned(), value);
    } else {
        let theme = patch.entry("theme").or_insert_with(|| json!({}));
        if let Some(theme) = theme.as_object_mut() {
            theme.insert(field.to_owned(), value);
        }
    }
}

fn restore_patch(changed: &BTreeMap<&'static str, Changed>, latest: &Value) -> Value {
    let mut patch = Map::new();
    for (&field, change) in changed {
        let now = field_value(latest, field);
        if change.before != change.wrote && (now == change.before || now == change.wrote) {
            patch_field(&mut patch, field, change.before.clone());
        }
    }
    Value::Object(patch)
}

fn merge(model: &mut Value, answer: Value) {
    if let (Some(model), Value::Object(answer)) = (model.as_object_mut(), answer) {
        model.extend(answer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_only_restores_its_own_fields_not_another_windows_edits() {
        let changed = BTreeMap::from([
            ("mode", Changed { before: json!("system"), wrote: json!("light") }),
            ("light", Changed { before: json!("plannotator"), wrote: json!("catppuccin") }),
            ("size", Changed { before: json!(13), wrote: json!(15) }),
        ]);
        assert_eq!(
            restore_patch(
                &changed,
                &json!({"theme": {"mode": "dark", "light": "catppuccin"}, "text_size": 15})
            ),
            json!({"theme": {"light": "plannotator"}, "text_size": 13})
        );
    }
}
