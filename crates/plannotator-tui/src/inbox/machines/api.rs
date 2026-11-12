use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::super::client::{Client, path_id};
use super::model::{
    ChecksResponse, Device, Lan, Machine, MachineOffer, Machines, Outbox, OutboxEntry, PairingOffer,
    PairingView, Tailnet, UpdateDetails,
};

/// These are the window routes, not the privileged control or device routes.
/// Every POST carries the session read by Client, so a restarted server refuses it.
pub(crate) struct Api<'a>(pub(crate) &'a Client);

#[derive(Debug, Deserialize)]
pub(crate) struct Access {
    #[serde(default)]
    pub phones: bool,
    pub via: Option<String>,
    pub guest: Option<bool>,
}

impl Access {
    pub(crate) fn local(&self) -> bool {
        self.via.as_deref() != Some("tailnet")
    }

    pub(crate) fn owner(&self) -> bool {
        self.local() || self.guest == Some(false)
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct Unlinked {
    pub unlinked: super::model::MachineName,
    pub told: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateStarted {
    pub outcome: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Agent {
    pub host: String,
    pub session: String,
    pub busy: Option<bool>,
    pub last_seen_at: Option<String>,
}

impl Api<'_> {
    pub(crate) fn access(&self) -> Result<Access> {
        self.0.get_json("GET", "/api/inbox/settings")
    }

    pub(crate) fn machines(&self) -> Result<Machines> {
        self.0.get_json("GET", "/api/inbox/machines")
    }

    pub(crate) fn outbox(&self) -> Result<Outbox> {
        self.0.get_json("GET", "/api/inbox/outbox")
    }

    pub(crate) fn devices(&self) -> Result<Vec<Device>> {
        #[derive(Deserialize)]
        struct Response {
            devices: Vec<Device>,
        }
        self.0.get_json::<Response>("GET", "/api/inbox/devices").map(|r| r.devices)
    }

    pub(crate) fn pairing(&self) -> Result<Option<PairingView>> {
        #[derive(Deserialize)]
        struct Response {
            offer: Option<PairingView>,
        }
        self.0.get_json::<Response>("GET", "/api/inbox/pairing").map(|r| r.offer)
    }

    pub(crate) fn checks(&self, fresh: bool) -> Result<ChecksResponse> {
        self.0.get_json(
            "GET",
            if fresh { "/api/inbox/pairing/checks?fresh=1" } else { "/api/inbox/pairing/checks" },
        )
    }

    pub(crate) fn tailnet(&self) -> Result<Tailnet> {
        #[derive(Deserialize)]
        struct Response {
            tailnet: Tailnet,
        }
        self.0.get_json::<Response>("GET", "/api/inbox/tailnet").map(|r| r.tailnet)
    }

    pub(crate) fn lan(&self) -> Result<Lan> {
        #[derive(Deserialize)]
        struct Response {
            lan: Lan,
        }
        self.0.get_json::<Response>("GET", "/api/inbox/lan").map(|r| r.lan)
    }

    pub(crate) fn offer_machine(&self) -> Result<MachineOffer> {
        self.post("/api/inbox/machines/offer", json!({"tailscale":true}))
    }

    pub(crate) fn link_machine(&self, input: &str) -> Result<Machine> {
        #[derive(Deserialize)]
        struct Response {
            machine: Machine,
        }
        self.post::<Response>("/api/inbox/machines/link", link_input(input)?).map(|r| r.machine)
    }

    pub(crate) fn unlink(&self, id: &str) -> Result<Unlinked> {
        self.post(&format!("/api/inbox/machines/{}/unlink", path_id(id)?), json!({}))
    }

    pub(crate) fn update_details(&self, id: &str) -> Result<UpdateDetails> {
        self.0.get_json("GET", &format!("/api/inbox/machines/{}/update", path_id(id)?))
    }

    pub(crate) fn start_update(&self, id: &str) -> Result<UpdateStarted> {
        self.post(&format!("/api/inbox/machines/{}/update", path_id(id)?), json!({}))
    }

    pub(crate) fn update_asked(&self, id: &str, thread: Option<&str>, agent: &str) -> Result<()> {
        self.post::<Value>(
            &format!("/api/inbox/machines/{}/update/asked", path_id(id)?),
            json!({"thread_id":thread,"agent":agent}),
        )?;
        Ok(())
    }

    pub(crate) fn agent_to_ask(&self, id: &str) -> Result<Option<Agent>> {
        #[derive(Deserialize)]
        struct Group {
            #[serde(default)]
            sessions: Vec<Agent>,
        }
        #[derive(Deserialize)]
        struct Response {
            #[serde(default)]
            projects: Vec<Group>,
        }
        let response: Response =
            self.0.get_json("GET", &format!("/api/inbox/m/{}/sessions", path_id(id)?))?;
        Ok(response.projects.into_iter().flat_map(|g| g.sessions).min_by(|a, b| {
            let rank = |busy| match busy {
                Some(false) => 0,
                None => 1,
                Some(true) => 2,
            };
            rank(a.busy).cmp(&rank(b.busy)).then_with(|| b.last_seen_at.cmp(&a.last_seen_at))
        }))
    }

    /// Key belongs to the staged action, never to the HTTP attempt. An unanswered or
    /// held response is retried with this exact key and never creates another thread.
    pub(crate) fn ask_update(&self, id: &str, agent: &Agent, key: &str) -> Result<String> {
        #[derive(Deserialize)]
        struct Response {
            thread_id: String,
        }
        self.post::<Response>(
            &format!("/api/inbox/m/{}/messages", path_id(id)?),
            json!({
                "to":{"host":agent.host,"session":agent.session},
                "body":super::ASK_UPDATE,
                "thread_name":"Update Plannotator",
                "idempotency_key":key,
            }),
        )
        .map(|r| r.thread_id)
    }

    pub(crate) fn pair_phone(&self) -> Result<PairingOffer> {
        self.post("/api/inbox/pairing", json!({}))
    }

    pub(crate) fn revoke(&self, id: &str) -> Result<Device> {
        #[derive(Deserialize)]
        struct Response {
            device: Device,
        }
        self.post::<Response>(&format!("/api/inbox/devices/{}/revoke", path_id(id)?), json!({}))
            .map(|r| r.device)
    }

    pub(crate) fn set_tailnet(&self, on: bool) -> Result<Tailnet> {
        #[derive(Deserialize)]
        struct Response {
            tailnet: Tailnet,
        }
        self.post::<Response>("/api/inbox/tailnet", json!({"on":on})).map(|r| r.tailnet)
    }

    pub(crate) fn set_lan(&self, on: bool) -> Result<Lan> {
        #[derive(Deserialize)]
        struct Response {
            lan: Lan,
        }
        self.post::<Response>("/api/inbox/lan", json!({"on":on})).map(|r| r.lan)
    }

    pub(crate) fn cancel(&self, key: &str) -> Result<OutboxEntry> {
        let entry = self.outbox_action(key, "cancel")?;
        ensure!(entry.state == "cancelled", "Cancellation was not confirmed. {}", entry.description(None));
        Ok(entry)
    }

    pub(crate) fn dismiss(&self, key: &str) -> Result<OutboxEntry> {
        self.outbox_action(key, "dismiss")
    }

    fn outbox_action(&self, key: &str, action: &str) -> Result<OutboxEntry> {
        #[derive(Deserialize)]
        struct Response {
            entry: OutboxEntry,
        }
        // Keys are arbitrary idempotency keys (often UUIDs), not Inbox ids. URL path
        // segment encoding, unlike path_id, must accept '-' and reject path injection.
        let key =
            url::form_urlencoded::byte_serialize(key.as_bytes()).collect::<String>().replace('+', "%20");
        self.post::<Response>(&format!("/api/inbox/outbox/{key}/{action}"), json!({})).map(|r| r.entry)
    }

    fn post<T: DeserializeOwned>(&self, path: &str, mut body: Value) -> Result<T> {
        body.as_object_mut()
            .context("Inbox write must be an object")?
            .insert("serverSession".into(), Value::String(self.0.server_session().to_owned()));
        let answer: Value = self.0.post_json(path, &body)?;
        super::decode_write(answer)
    }
}

/// One field accepts either the full one-use link or an address followed by six digits.
/// Merely completing the field does not link: the pane then asks for confirmation.
pub(crate) fn link_input(input: &str) -> Result<Value> {
    let input = input.trim();
    if input.starts_with("plannotator://link") {
        let parsed = url::Url::parse(input).context("parsing machine link")?;
        ensure!(parsed.host_str() == Some("link"), "A machine link starts with plannotator://link");
        return Ok(json!({"link":input}));
    }
    let fields: Vec<_> = input.split_whitespace().collect();
    let [address, code] = fields.as_slice() else {
        anyhow::bail!("Paste plannotator://link?… or the tailnet address followed by its six-digit code.");
    };
    ensure!(
        code.len() == 6 && code.bytes().all(|c| c.is_ascii_digit()),
        "The code must contain exactly six digits."
    );
    Ok(json!({"address":address,"code":code}))
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests assert by panicking")]
mod tests {
    use super::*;

    #[test]
    fn link_accepts_pasted_or_typed_code_without_sending() {
        assert_eq!(
            link_input(" mini.tailnet.ts.net:8443 012345 ").expect("typed"),
            json!({"address":"mini.tailnet.ts.net:8443","code":"012345"})
        );
        assert!(link_input("mini 12345").is_err());
        assert!(link_input("mini 12345x").is_err());
        assert!(link_input("plannotator://link?secret=one").is_ok());
    }
}
