//! Wire models for the Inbox's linked machines, phones, pairing, and held writes.

use std::fmt;

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Machine {
    pub id: String,
    pub name: String,
    #[serde(rename = "self")]
    pub is_self: bool,
    pub state: String,
    pub since: String,
    pub reason: Option<String>,
    pub version: Option<String>,
    pub address: Option<String>,
    pub both_ways: bool,
    pub waiting: usize,
    pub list_cursor: Option<u64>,
    #[serde(flatten)]
    pub capabilities: MachineCapabilities,
    pub update: Option<MachineUpdate>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct MachineCapabilities {
    pub snoozes: bool,
    pub snooze_old: bool,
    pub launch: Option<bool>,
    pub message_deletions: bool,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct MachineUpdate {
    pub phase: String,
    pub target: Option<String>,
    pub from: Option<String>,
    pub can_update: bool,
    pub self_update: bool,
    pub asked: Option<UpdateAsked>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct UpdateAsked {
    pub thread: Option<String>,
    pub agent: String,
    pub at: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct UpdateDetails {
    pub too_old: bool,
    pub current: Option<String>,
    pub update: Option<AvailableUpdate>,
    pub platform: Option<String>,
    pub command: Option<String>,
    pub log: Option<String>,
    pub open_reviews: Option<usize>,
    pub failure: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AvailableUpdate {
    pub state: String,
    pub version: String,
    pub action: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Machines {
    #[serde(rename = "self")]
    pub self_id: String,
    pub machines: Vec<Machine>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MachineOffer {
    pub link: String,
    pub code: String,
    pub expires_at: String,
    pub machine: MachineName,
    pub tailnet: Option<String>,
    pub tailnet_error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MachineName {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct OutboxEntry {
    pub key: String,
    pub machine: String,
    pub route: String,
    pub state: String,
    pub queued_at: String,
    pub settled_at: Option<String>,
    pub status: Option<u16>,
    pub answer: Value,
    pub notice: Option<OutboxNotice>,
    pub dismissed: bool,
    pub words: Option<String>,
    pub thread: Option<String>,
    pub machine_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct OutboxNotice {
    pub kind: String,
    pub at: String,
}

impl OutboxEntry {
    pub(crate) fn pending(&self) -> bool {
        matches!(self.state.as_str(), "queued" | "sending")
    }

    /// A queued/sending write is never described as delivered. Unlinking an uncertain
    /// write must retain the distinction between definitely unsent and possibly delivered.
    pub(crate) fn description(&self, machine: Option<&Machine>) -> String {
        let name =
            machine.map(|m| m.name.as_str()).or(self.machine_name.as_deref()).unwrap_or("the other machine");
        if self.pending() {
            return match machine.map(|m| m.state.as_str()) {
                Some("needs_update") => format!("Will send when {name} is updated"),
                Some("offline" | "revoked") => format!("Will send when {name} is back"),
                _ => format!("Sending to {name}; waiting for its confirmation"),
            };
        }
        if self.state == "delivered" {
            return format!("Delivered to {name}");
        }
        if self.state == "cancelled" {
            return format!("Cancelled; never sent to {name}");
        }
        match self.notice.as_ref().map(|notice| notice.kind.as_str()) {
            Some("may_have_been_delivered") => format!("May have been delivered. Check on {name}."),
            Some("not_delivered") => format!("Not delivered: {name} was unlinked before it went."),
            Some("refused") => format!(
                "Not delivered: {}",
                self.answer.get("error").and_then(Value::as_str).unwrap_or("the other machine refused it")
            ),
            _ => format!("Not confirmed by {name} ({})", self.state),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Outbox {
    pub waiting: Vec<OutboxEntry>,
    pub notices: Vec<OutboxEntry>,
}

/// A successful HTTP transport is not a successful remote write. Callers may remove
/// their local pending operation only after recognizing this as an outbox hand-off;
/// they must not label it delivered or regenerate its idempotency key.
#[derive(Debug, Clone)]
pub(crate) struct HeldWrite {
    pub entry: OutboxEntry,
    pub machine: Option<Machine>,
}

impl fmt::Display for HeldWrite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.entry.description(self.machine.as_ref()))
    }
}

impl std::error::Error for HeldWrite {}

/// A replayed key whose outbox operation settled without confirmed delivery.
/// Unlike `HeldWrite` this is terminal: callers retain the receipt and words, but
/// must not describe it as queued or remember an update request as sent.
#[derive(Debug, Clone)]
pub(crate) struct UndeliveredWrite {
    pub entry: OutboxEntry,
    pub machine: Option<Machine>,
}

impl fmt::Display for UndeliveredWrite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.entry.description(self.machine.as_ref()))
    }
}

impl std::error::Error for UndeliveredWrite {}

/// Decode forwarded 202 envelopes before typed deserialization, including replays of
/// settled keys. This also works when an API caller asks for arbitrary JSON (`Value`).
pub(crate) fn decode_write<T: DeserializeOwned>(mut value: Value) -> Result<T> {
    if let Some(outbox) = value.get_mut("outbox").filter(|entry| entry.is_object()) {
        let entry: OutboxEntry =
            serde_json::from_value(outbox.take()).context("decoding held Inbox write")?;
        let machine = value
            .get_mut("machine")
            .filter(|m| !m.is_null())
            .map(|m| serde_json::from_value(m.take()))
            .transpose()
            .context("decoding held write's machine")?;
        if entry.pending() {
            return Err(HeldWrite { entry, machine }.into());
        }
        if entry.state == "delivered" {
            value = entry.answer;
        } else {
            return Err(UndeliveredWrite { entry, machine }.into());
        }
    }
    serde_json::from_value(value).context("decoding Inbox write response")
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Device {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub created_at: String,
    pub last_seen_at: String,
    pub revoked_at: Option<String>,
    pub carriage: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Tailnet {
    pub on: bool,
    pub address: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Lan {
    pub on: bool,
    pub address: Option<String>,
    pub fingerprint: Option<String>,
    pub bonjour: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PairingOffer {
    pub offer: PairingCode,
    pub link: String,
    pub computer: Computer,
    pub addresses: Addresses,
    pub reachable: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PairingCode {
    pub code: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Computer {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Addresses {
    pub tailnet: Option<String>,
    pub lan: Option<String>,
    pub fingerprint: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PairingView {
    pub expires_at: String,
    pub state: String,
    pub tries_left: usize,
    #[serde(default)]
    pub attempts: Vec<PairingAttempt>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PairingAttempt {
    pub via: String,
    pub result: String,
    pub at: String,
    pub tries_left: Option<usize>,
    pub device: Option<String>,
}

impl PairingAttempt {
    pub(crate) fn description(&self) -> String {
        match self.result.as_str() {
            "wrong_code" => format!(
                "Wrong six digits over {}; {} tries left. Check the digits or scan the current QR.",
                self.via,
                self.tries_left.unwrap_or(0)
            ),
            "code_over_wifi" => {
                "Over Wi-Fi, pairing requires scanning the QR in Plannotator, not typing the six digits."
                    .into()
            }
            "old_code" => "A phone used an older code. Scan the code now on screen.".into(),
            "expired" => "A phone arrived after the code closed. Make and scan a new code.".into(),
            "redeemed" => format!(
                "Paired over {}{}",
                self.via,
                self.device.as_ref().map_or(String::new(), |id| format!(" ({id})"))
            ),
            other => format!("{other} over {}", self.via),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Checks {
    pub wifi: WifiChecks,
    pub tailscale: TailscaleChecks,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct WifiChecks {
    pub address: Option<String>,
    pub network: Option<String>,
    pub openssl: bool,
    pub certificate: bool,
    pub bonjour: bool,
    pub firewall: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct TailscaleChecks {
    pub state: String,
    pub login: Option<String>,
    pub https_certs: Option<bool>,
    pub port_free: Option<bool>,
    pub https_port: Option<u16>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ChecksResponse {
    pub checks: Checks,
    pub lan: Lan,
    pub tailnet: Tailnet,
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests assert by panicking")]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn held_write_is_not_success_even_for_untyped_callers() {
        for state in ["queued", "sending"] {
            let error =
                decode_write::<Value>(json!({"outbox": {"key": "k", "state": state}})).expect_err("held");
            assert_eq!(error.downcast_ref::<HeldWrite>().expect("typed held error").entry.key, "k");
        }
    }

    #[test]
    fn replayed_delivery_unwraps_but_cancelled_never_succeeds() {
        assert_eq!(
            decode_write::<Value>(json!({"outbox": {"state": "delivered", "answer": {"ok": true}}}))
                .expect("delivery"),
            json!({"ok": true})
        );
        for state in ["cancelled", "refused", "parked", "dropped"] {
            let error = decode_write::<Value>(
                json!({"outbox": {"key":"saved-key", "state": state, "words":"keep me"}}),
            )
            .expect_err("not delivered");
            let receipt = error.downcast_ref::<UndeliveredWrite>().expect("typed terminal receipt");
            assert_eq!(receipt.entry.state, state);
            assert_eq!(receipt.entry.key, "saved-key");
            assert_eq!(receipt.entry.words.as_deref(), Some("keep me"));
        }
    }

    #[test]
    fn uncertain_unlink_does_not_claim_never_sent() {
        let entry: OutboxEntry = serde_json::from_value(
            json!({"state":"dropped", "notice":{"kind":"may_have_been_delivered", "at":"now"}}),
        )
        .expect("entry");
        assert!(entry.description(None).starts_with("May have been delivered"));
    }
}
