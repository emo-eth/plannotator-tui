//! Folder and clone picks are made from the server's actual filesystem view, never local guesses.

use std::fmt::Write as _;

use anyhow::{Result, ensure};
use ratatui::DefaultTerminal;
use serde_json::{Value, json};

use super::super::client::Client;
use super::{
    api,
    model::{CloneTarget, Folders, LaunchCatalog},
    screen::{self, Choice},
};

pub(super) fn browse(
    client: &Client,
    terminal: &mut DefaultTerminal,
    machine: Option<&str>,
    initial: &str,
) -> Result<Option<Value>> {
    let mut path = initial.to_owned();
    loop {
        let folders: Folders =
            api::get(client, machine, &format!("launch/folders?{}", api::query(&[("path", &path)])))?;
        let mut labels = vec!["Type or paste another full path".into()];
        let start = folders.exists
            && folders.start != Some(false)
            && folders.state.as_deref().is_none_or(|s| s == "folder");
        labels.push(if start { format!("Start in {}", folders.path) } else { "Cannot start here".into() });
        let create =
            !folders.exists && folders.blocked.is_none() && folders.state.as_deref() == Some("missing");
        labels.push(if create {
            format!("Create {} and start", folders.path)
        } else {
            "Create a folder by typing a new path".into()
        });
        labels.push("Parent folder".into());
        labels.extend(folders.children.iter().map(|name| {
            format!(
                "{name}{}{}",
                if folders.locked.contains(name) { " [locked]" } else { "" },
                if folders.nostart.contains(name) { " [browse only]" } else { "" }
            )
        }));
        let detail = format!(
            "{}\nState: {} · writable: {}\n{}",
            folders.path,
            folders.state.as_deref().unwrap_or(if folders.exists { "folder" } else { "missing" }),
            folders.writable.map_or("unknown", |v| if v { "yes" } else { "no" }),
            folders.blocked.as_ref().map_or_else(String::new, ToString::to_string)
        );
        match screen::choose(terminal, "Choose folder", &detail, &labels, false)? {
            Choice::Cancel => return Ok(None),
            Choice::Selected(0) => {
                if let Some(value) = screen::text(
                    terminal,
                    "Folder path",
                    "Use ~/… or a full path. Hidden/system folders are refused by the server.",
                    &path,
                )? {
                    path = value.trim().into();
                }
            }
            Choice::Selected(1) if start => return Ok(Some(json!({"folder": folders.path}))),
            Choice::Selected(2) if create => {
                return Ok(Some(json!({"folder": folders.path, "create": true})));
            }
            Choice::Selected(3) => {
                path = folders.nearest.map_or_else(|| parent(&folders.path), |n| n.path);
            }
            Choice::Selected(i) if i >= 4 => {
                if let Some(name) = folders.children.get(i - 4) {
                    if folders.locked.contains(name) {
                        screen::notice(terminal, "Folder locked", "That folder cannot be opened.")?;
                    } else {
                        path = format!("{}/{}", folders.path.trim_end_matches('/'), name);
                    }
                }
            }
            _ => {}
        }
    }
}

fn parent(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit_once('/')
        .map_or_else(|| "~".into(), |(parent, _)| if parent.is_empty() { "/".into() } else { parent.into() })
}

pub(super) fn clone_repo(
    client: &Client,
    terminal: &mut DefaultTerminal,
    machine: Option<&str>,
) -> Result<Option<Value>> {
    let Some(url) = screen::text(
        terminal,
        "Clone repository",
        "Paste an HTTPS/SSH git URL. The server validates and normalizes it; credentials are never added here.",
        "",
    )?
    else {
        return Ok(None);
    };
    let Some(into) = screen::text(
        terminal,
        "Clone into",
        "Parent folder (~/… or full path); blank uses this machine's default.",
        "",
    )?
    else {
        return Ok(None);
    };
    let mut pairs = vec![("url", url.trim())];
    if !into.trim().is_empty() {
        pairs.push(("into", into.trim()));
    }
    let target: CloneTarget = api::get(client, machine, &format!("launch/clone?{}", api::query(&pairs)))?;
    ensure!(target.writable || target.state == "already", "Cannot clone into {}", target.into);
    let reach: Value =
        api::get(client, machine, &format!("launch/clone-check?{}", api::query(&[("url", url.trim())])))?;
    let detail = format!(
        "Repository: {}\nFolder: {}\n{}\nReachability: {}\n{}",
        url.trim(),
        target.path,
        if target.state == "already" {
            "Already cloned: start in this folder."
        } else {
            "Clone here, then start the agent."
        },
        reach.get("reach").and_then(Value::as_str).unwrap_or("unknown"),
        target.taken.map_or_else(String::new, |p| format!("{p} is taken; using the path above."))
    );
    if !screen::confirm(terminal, "Repository destination", &detail, "Use this repository")? {
        return Ok(None);
    }
    Ok(Some(if target.state == "already" {
        json!({"folder": target.path})
    } else {
        json!({"folder": target.path, "clone": {"url": url.trim()}})
    }))
}

pub(super) fn choose(
    client: &Client,
    terminal: &mut DefaultTerminal,
    machine: Option<&str>,
    catalog: &LaunchCatalog,
    home: &str,
    local: bool,
) -> Result<Option<(Value, Option<String>)>> {
    let mut labels: Vec<String> = catalog
        .projects
        .iter()
        .map(|p| {
            format!(
                "{} · {}{}",
                p.name,
                p.root,
                if p.slow {
                    " [drive not responding]"
                } else if !p.exists {
                    " [missing]"
                } else {
                    ""
                }
            )
        })
        .collect();
    let projects = labels.len();
    labels.extend(["New scratch folder".into(), "Another folder…".into(), "Clone a repository…".into()]);
    let finder = labels.len();
    if local && machine.is_none() && catalog.finder {
        labels.push("Choose in Finder…".into());
    }
    let mut detail = "The folder belongs to the selected machine.\nCreate and Clone are confirmed before anything is started.\n".to_owned();
    for drive in &catalog.drives {
        let _ = writeln!(
            detail,
            "Drive: {} · {}{}",
            drive.name,
            drive.path,
            if drive.network { " [network]" } else { "" }
        );
    }
    loop {
        match screen::choose(terminal, "Start agent in…", &detail, &labels, false)? {
            Choice::Cancel => return Ok(None),
            Choice::Selected(i) if i < projects => {
                if let Some(project) = catalog.projects.get(i) {
                    ensure!(
                        project.exists && !project.slow,
                        "That project folder is missing or its drive is not responding"
                    );
                    return Ok(Some((json!({"project_id": project.id}), Some(project.id.clone()))));
                }
            }
            Choice::Selected(i) if i == projects => {
                let scratch = catalog
                    .scratch
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("This Inbox does not offer scratch folders"))?;
                if screen::confirm(
                    terminal,
                    "New scratch folder",
                    &format!(
                        "Create a new folder under {}. Deleting this session later removes the scratch folder and everything in it.",
                        scratch.root
                    ),
                    "Use scratch folder",
                )? {
                    return Ok(Some((json!({"scratch": true}), Some("scratch".into()))));
                }
            }
            Choice::Selected(i) if i == projects + 1 => {
                return Ok(browse(client, terminal, machine, home)?.map(|v| (v, None)));
            }
            Choice::Selected(i) if i == projects + 2 => {
                return Ok(clone_repo(client, terminal, machine)?.map(|v| (v, None)));
            }
            Choice::Selected(i) if i == finder && local && machine.is_none() && catalog.finder => {
                let answer = api::post(client, None, "launch/choose-folder", &api::keyed())?;
                if let Some(path) = answer.get("path").and_then(Value::as_str) {
                    return Ok(browse(client, terminal, None, path)?.map(|v| (v, None)));
                }
            }
            _ => {}
        }
    }
}
