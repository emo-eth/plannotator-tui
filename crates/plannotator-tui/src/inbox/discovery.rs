#![allow(clippy::all, clippy::pedantic)]

//! Server discovery for Plannotator Inbox.
//!
//! Reads `~/.plannotator/inbox/inbox.json` to find `url` and `serverSession`.
//! Supports overrides via `--url` CLI argument and `PLANNOTATOR_INBOX_URL` env var.
//! Re-reads on 409 Conflict.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct InboxConfigFile {
    pub(crate) v: u32,
    pub(crate) pid: u32,
    pub(crate) port: u16,
    pub(crate) url: String,
    pub(crate) version: String,
    pub(crate) token: String,
    #[serde(rename = "serverSession")]
    pub(crate) server_session: String,
    #[serde(rename = "startedAt")]
    pub(crate) started_at: String,
    #[serde(default)]
    pub(crate) tailscale: Option<TailscaleConfig>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct TailscaleConfig {
    pub(crate) url: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) https_port: Option<u16>,
    pub(crate) proxy_port: Option<u16>,
}

/// Resolved connection info to the Inbox server.
#[derive(Debug, Clone)]
pub(crate) struct InboxConnectionInfo {
    pub(crate) url: String,
    pub(crate) server_session: String,
    pub(crate) config_path: PathBuf,
}

/// Path to `~/.plannotator/inbox/inbox.json`.
pub(crate) fn inbox_config_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"));
    home.join(".plannotator").join("inbox").join("inbox.json")
}

pub(crate) fn read_config_file(path: &Path) -> Result<InboxConfigFile> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("reading inbox config from {}", path.display()))?;
    let config: InboxConfigFile = serde_json::from_str(&content)
        .with_context(|| format!("parsing inbox config from {}", path.display()))?;
    Ok(config)
}

/// Discover Inbox server connection parameters.
pub(crate) use discover as discover_inbox;

pub(crate) fn discover(url_override: Option<&str>) -> Result<InboxConnectionInfo> {
    let config_path = inbox_config_path();
    let config = read_config_file(&config_path).ok();

    let server_session = config.as_ref().map(|c| c.server_session.clone()).unwrap_or_default();

    let mut url = if let Some(u) = url_override {
        u.to_owned()
    } else if let Ok(u) = std::env::var("PLANNOTATOR_INBOX_URL") {
        u
    } else if let Some(c) = &config {
        c.url.clone()
    } else {
        "http://localhost:60774".to_owned()
    };

    if !url.ends_with('/') {
        url.push('/');
    }

    Ok(InboxConnectionInfo { url, server_session, config_path })
}

/// Re-read `serverSession` from the config file on disk.
pub(crate) fn reload_server_session(path: &Path) -> Result<String> {
    let config = read_config_file(path)?;
    Ok(config.server_session)
}
