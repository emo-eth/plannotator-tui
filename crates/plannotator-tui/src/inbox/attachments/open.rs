use std::process::{Command, Stdio};

use anyhow::{Context, Result, ensure};
use url::Url;

use super::{Attachment, AttachmentKind, Version, id};
use crate::inbox::client::Client;

impl Client {
    pub(crate) fn attachment_url(&self, attachment: &Attachment, version: Version) -> Result<Url> {
        let route = match attachment.kind {
            AttachmentKind::Html => "page",
            AttachmentKind::Image => "image",
            AttachmentKind::Video => "video",
            AttachmentKind::Audio => "audio",
            AttachmentKind::Pdf | AttachmentKind::File => "download",
            _ => "",
        };
        let suffix = if route.is_empty() { String::new() } else { format!("/{route}") };
        let path = format!("/api/inbox/attachments/{}{suffix}{}", id(&attachment.id)?, version.query());
        Url::parse(self.url())?.join(&path).context("constructing attachment viewer URL")
    }
}

pub(crate) fn open_browser(target: &str) -> Result<()> {
    let url = browser_url(target)?;
    let mut command = browser_command(&url);
    let output = command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).output().context(
        "starting the external browser; open the displayed URL manually if no browser is installed",
    )?;
    ensure!(
        output.status.success(),
        "External browser refused {}: {}",
        url,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

fn browser_url(target: &str) -> Result<Url> {
    ensure!(!target.chars().any(char::is_control), "Browser URL contains control characters");
    let url = Url::parse(target).context("parsing external browser URL")?;
    ensure!(
        matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
        "Only HTTP(S) browser URLs can be opened"
    );
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "Browser URL must not contain credentials"
    );
    Ok(url)
}

#[cfg(target_os = "macos")]
fn browser_command(url: &Url) -> Command {
    let mut command = Command::new("open");
    command.arg(url.as_str());
    command
}

#[cfg(target_os = "windows")]
fn browser_command(url: &Url) -> Command {
    let mut command = Command::new("rundll32.exe");
    command.args(["url.dll,FileProtocolHandler", url.as_str()]);
    command
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn browser_command(url: &Url) -> Command {
    let mut command = Command::new("xdg-open");
    command.arg(url.as_str());
    command
}

pub(crate) fn prototype_url(
    client: &Client,
    target: &str,
    attachments: &[Attachment],
    root: &str,
    base: Option<&str>,
    thread_id: &str,
    checked_sites: &[(String, String)],
) -> Result<Option<Url>> {
    if let Ok(mut url) = Url::parse(target.trim())
        && !is_windows_absolute(target)
    {
        let _ = browser_url(url.as_str())?;
        url.set_fragment(None);
        let Some((site_id, _)) = checked_sites.iter().find(|(_, site)| site == url.as_str()) else {
            return Ok(None);
        };
        let path = format!("/api/inbox/threads/{}/live/{}/frame", id(thread_id)?, id(site_id)?);
        return Url::parse(client.url())?.join(&path).map(Some).context("constructing prototype frame URL");
    }
    let Some(attachment) = attachment_for_link(target, attachments, root, base) else { return Ok(None) };
    if attachment.kind != AttachmentKind::Html {
        return Ok(None);
    }
    client.attachment_url(attachment, attachment.readable_version()).map(Some)
}

pub(crate) fn attachment_for_link<'a>(
    href: &str,
    attachments: &'a [Attachment],
    root: &str,
    base: Option<&str>,
) -> Option<&'a Attachment> {
    let raw = href.trim().split(['?', '#']).next().filter(|s| !s.is_empty())?;
    let decoded = decode_path(raw);
    let candidates = std::iter::once(raw).chain(decoded.as_deref().filter(|value| *value != raw));
    for candidate in candidates.clone() {
        let paths = if candidate.starts_with(['/', '\\']) || is_windows_absolute(candidate) {
            vec![normalize_path(candidate)]
        } else {
            base.into_iter()
                .chain(std::iter::once(root))
                .filter(|dir| !dir.is_empty())
                .map(|dir| normalize_path(&format!("{dir}/{candidate}")))
                .collect()
        };
        for path in paths {
            if let Some(attachment) = attachments.iter().rev().find(|attachment| {
                normalize_path(&attachment.path) == path
                    || attachment.named_path.as_ref().is_some_and(|named| normalize_path(named) == path)
            }) {
                return Some(attachment);
            }
        }
    }
    for candidate in candidates {
        if candidate.contains(['/', '\\']) {
            continue;
        }
        let mut named = attachments.iter().filter(|attachment| attachment.name == candidate);
        let first = named.next();
        if first.is_some() && named.next().is_none() {
            return first;
        }
    }
    None
}

fn is_windows_absolute(path: &str) -> bool {
    let mut bytes = path.bytes();
    bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
        && bytes.next() == Some(b':')
        && matches!(bytes.next(), Some(b'/' | b'\\'))
}

fn normalize_path(path: &str) -> String {
    let slashed = path.replace('\\', "/");
    let (drive, rest) = if slashed.as_bytes().get(1) == Some(&b':')
        && slashed.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
    {
        (slashed.get(..2).unwrap_or("").to_ascii_lowercase(), slashed.get(2..).unwrap_or(""))
    } else {
        (String::new(), slashed.as_str())
    };
    let absolute = rest.starts_with('/');
    let mut parts = Vec::new();
    for part in rest.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|part| *part != "..") => {
                parts.pop();
            }
            ".." if absolute => {}
            _ => parts.push(part),
        }
    }
    format!("{drive}{}{}", if absolute { "/" } else { "" }, parts.join("/"))
}

fn decode_path(path: &str) -> Option<String> {
    let mut decoded = Vec::with_capacity(path.len());
    let mut bytes = path.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = char::from(bytes.next()?).to_digit(16)?;
            let low = char::from(bytes.next()?).to_digit(16)?;
            decoded.push(u8::try_from(high * 16 + low).ok()?);
        } else {
            decoded.push(byte);
        }
    }
    String::from_utf8(decoded).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_handoff_rejects_files_credentials_and_controls() {
        for target in [
            "file:///tmp/a.html",
            "javascript:alert(1)",
            "https://name:secret@example.com",
            "https://example.com/\n",
        ] {
            assert!(browser_url(target).is_err());
        }
        assert!(browser_url("https://example.com/path?q=a%26b").is_ok());
    }

    #[test]
    fn link_matching_normalizes_without_opening_local_paths() {
        assert_eq!(normalize_path("C:\\project\\x\\..\\page.html"), "c:/project/page.html");
        assert_eq!(normalize_path("/root/../../page.html"), "/page.html");
        assert_eq!(decode_path("my%20mock.html").as_deref(), Some("my mock.html"));
        assert!(decode_path("bad%ZZ").is_none());
    }
}
