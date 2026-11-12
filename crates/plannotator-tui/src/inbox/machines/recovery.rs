//! Cancellation recovery creates a new compose draft rather than overwriting an
//! existing reply or compose draft. Retries use the same draft id and base=null.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, ensure};
use serde_json::{Value, json};

use super::super::client::{Client, path_id};

pub(super) fn draft_id() -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let clock = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let state = RandomState::new();
    let mut first = state.build_hasher();
    first.write_u128(clock.as_nanos());
    first.write_u32(std::process::id());
    let mut second = state.build_hasher();
    second.write_u64(first.finish());
    second.write_u8(1);
    let mut value = ((clock.as_millis() & ((1 << 48) - 1)) << 80)
        | (u128::from(first.finish()) << 16)
        | u128::from(second.finish() & 0xffff);
    let mut digits = [b'0'; 26];
    for digit in digits.iter_mut().rev() {
        *digit = ALPHABET.get((value & 31) as usize).copied().unwrap_or(b'0');
        value >>= 5;
    }
    let mut id = String::with_capacity(30);
    id.push_str("drf_");
    id.extend(digits.map(char::from));
    id
}

pub(super) fn save_draft(client: &Client, id: &str, body: &str) -> Result<()> {
    ensure!(!body.is_empty(), "There are no words to restore into Drafts.");
    let path = format!("/api/inbox/drafts/{}", path_id(id)?);
    let result: Result<Value> = client.post_json(
        &path,
        &json!({
            "serverSession":client.server_session(), "body":body,
            "to":null, "project":null, "files":[], "base":null,
        }),
    );
    match result {
        Ok(answer) => {
            ensure!(
                answer.get("draft").and_then(|d| d.get("id")).and_then(Value::as_str) == Some(id),
                "Draft save did not confirm the new draft id."
            );
            Ok(())
        }
        Err(error) => {
            // An unanswered save may have landed. Read this exact id, never mint a
            // second one or overwrite a draft another window has since changed.
            if let Ok(answer) = client.get_json::<Value>("GET", "/api/inbox/drafts") {
                let saved = answer.get("drafts").and_then(Value::as_array).is_some_and(|drafts| {
                    drafts.iter().any(|draft| {
                        draft.get("id").and_then(Value::as_str) == Some(id)
                            && draft.get("body").and_then(Value::as_str) == Some(body)
                            && draft.get("to").is_none_or(Value::is_null)
                            && draft.get("project").is_none_or(Value::is_null)
                            && draft.get("files").and_then(Value::as_array).is_none_or(Vec::is_empty)
                    })
                });
                if saved {
                    return Ok(());
                }
            }
            Err(error)
        }
    }
}

/// A caller cancelling a held launch can restore its editable words to a new
/// compose draft. This does not cancel, send, launch, or overwrite any existing draft.
pub(crate) fn save_new_draft(client: &Client, body: &str) -> Result<String> {
    let id = draft_id();
    save_draft(client, &id, body)?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_ids_have_server_ulid_shape() {
        let first = draft_id();
        let second = draft_id();
        assert_ne!(first, second);
        assert_eq!(first.len(), 30);
        assert!(first.starts_with("drf_"));
        assert!(first.as_bytes().get(4).is_some_and(|b| (b'0'..=b'7').contains(b)));
        assert!(first.bytes().skip(4).all(|b| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&b)));
    }
}
