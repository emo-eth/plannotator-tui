use std::fs::File;
use std::io::Read as _;
use std::path::Path;

use anyhow::{Context, Result, ensure};
use plannotator_tui_schema::Annotation;
use serde_json::{Value, json};

use crate::inbox::client::Client;

const MAX_BYTES: u64 = 128 * 1024 * 1024;

pub(super) fn attach_images(client: &Client, wire: &mut Value, note: &Annotation) -> Result<()> {
    if note.plannotator_tui.images.is_empty() && note.attachments.is_empty() {
        return Ok(());
    }
    let mut images = wire.get("images").and_then(Value::as_array).cloned().unwrap_or_default();
    for path in &note.plannotator_tui.images {
        let before = std::fs::metadata(path).with_context(|| format!("checking annotation image {path}"))?;
        ensure!(
            before.is_file() && before.len() <= MAX_BYTES,
            "Annotation image must be a regular file no larger than 128 MiB: {path}"
        );
        let file = File::open(path).with_context(|| format!("opening annotation image {path}"))?;
        let metadata = file.metadata().with_context(|| format!("checking annotation image {path}"))?;
        ensure!(
            metadata.is_file() && metadata.len() <= MAX_BYTES,
            "Annotation image must be a regular file no larger than 128 MiB: {path}"
        );
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .with_context(|| format!("reading annotation image {path}"))?;
        ensure!(bytes.len() as u64 <= MAX_BYTES, "Annotation image grew beyond 128 MiB: {path}");
        let kind = image_type(&bytes)
            .with_context(|| format!("{path}: Inbox drawings accept PNG, JPEG or WebP bytes"))?;
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("serverSession", client.server_session())
            .finish();
        let saved = client.post_bytes(&format!("/api/inbox/markups?{query}"), kind, &bytes)?;
        let stored = saved.get("path").and_then(Value::as_str).context("Inbox markup upload omitted path")?;
        let name = Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .context("Annotation image has no UTF-8 file name")?;
        images.push(json!({"path":stored,"name":name}));
    }
    for path in &note.attachments {
        let url = url::Url::parse(path).context("parsing annotation image URL")?;
        ensure!(
            url.scheme() == "https" && url.username().is_empty() && url.password().is_none(),
            "Uploaded annotation images must use HTTPS without credentials"
        );
        images.push(json!({"path":path,"name":"attached image"}));
    }
    wire["images"] = Value::Array(images);
    Ok(())
}

fn image_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_type_comes_from_bytes_not_file_name() {
        assert_eq!(image_type(b"\x89PNG\r\n\x1a\nrest"), Some("image/png"));
        assert_eq!(image_type(b"\xff\xd8\xffrest"), Some("image/jpeg"));
        assert_eq!(image_type(b"RIFFxxxxWEBPrest"), Some("image/webp"));
        assert_eq!(image_type(b"<svg onload='script'/>"), None);
    }
}
