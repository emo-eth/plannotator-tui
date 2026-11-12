//! Linked machines and devices on the existing Inbox terminal (`M`). Tab switches
//! Machines / Phones / Network / Outbox. `r` reads again; Enter opens details;
//! every mutation first shows its target and consequences and requires `y`.
//! Network reads and pairing checks never publish a listener. A link code does:
//! its confirmation explicitly names Tailscale Serve. Held writes stay held until
//! the server confirms delivery; cancel/dismiss/unlink never optimistically erase words.

mod api;
mod model;
mod recovery;
mod view;

pub(crate) use model::{HeldWrite, Machine, OutboxEntry, UndeliveredWrite, decode_write};
pub(crate) use recovery::save_new_draft;

use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, ensure};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::ListState;

use crate::app::compose::{Compose, ComposeAction};
use crate::delivery::{Clipboard, Delivery as _};

use super::client::Client;
use api::{Access, Agent, Api};
use model::{Checks, Device, Lan, MachineOffer, PairingOffer, PairingView, Tailnet, UpdateDetails};

pub(crate) const ASK_UPDATE: &str = "Please update Plannotator on this machine. Run these two commands, one at a time, in this order:\n\n1. `plannotator update`\n2. `plannotator inbox restart`\n\nRun step 2 only if step 1 succeeds (\"already up to date\" counts as success). If `plannotator` is not found, use the full path `~/.local/bin/plannotator` (on Windows, `$env:LOCALAPPDATA\\plannotator\\plannotator.exe`).\n\nThe restart stops the Inbox for a few seconds. If your reply fails right after it, wait a few seconds and try again.\n\nDon't change anything else, and don't try to fix an error. Reply here when it's done, with the version the restart printed, or with the error.";

const REFRESH: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Machines,
    Phones,
    Network,
    Outbox,
}

impl Tab {
    fn next(self, backwards: bool) -> Self {
        match (self, backwards) {
            (Self::Machines, false) | (Self::Network, true) => Self::Phones,
            (Self::Phones, false) | (Self::Outbox, true) => Self::Network,
            (Self::Network, false) | (Self::Machines, true) => Self::Outbox,
            _ => Self::Machines,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Machines => "Machines",
            Self::Phones => "Phones",
            Self::Network => "Network",
            Self::Outbox => "Outbox",
        }
    }
}

#[derive(Debug, Clone)]
enum Action {
    OfferMachine,
    Link(String),
    Unlink(Machine),
    PairPhone,
    Revoke(Device),
    Tailnet(bool),
    Lan(bool),
    Cancel(OutboxEntry),
    Dismiss(OutboxEntry),
    Update(Machine),
    AskUpdate { machine: Machine, agent: Agent, key: String },
    RememberAsked { machine: String, thread: Option<String>, agent: String },
    RestoreDraft { id: String, body: String },
}

impl Action {
    fn confirmation(&self) -> String {
        match self {
            Self::OfferMachine => "Show a one-use machine link code? This publishes the Inbox's device door over your tailnet with Tailscale Serve. It stays published while a code is open or a machine is linked. Any previous code is replaced.".into(),
            Self::Link(input) => format!("Link the other machine both ways over your tailnet? This publishes this computer's door if needed.\n\n{input}"),
            Self::Unlink(machine) => format!("Unlink {} on both machines? {} held write(s) will no longer be sent. A write already attempted may have been delivered; its words and notice remain in Outbox. If that machine is asleep it learns about the unlink next time it contacts this Inbox.", machine.name, machine.waiting),
            Self::PairPhone => "Create a one-use phone pairing code? This replaces any current code. Scan it in Plannotator on the phone. Six digits work only over Tailscale; Wi-Fi requires the QR code.".into(),
            Self::Revoke(device) => format!("Remove {} ({})? Its key stops working immediately. Pair it again to restore access.", device.name, device.platform),
            Self::Tailnet(on) => format!("Turn phone/device Tailscale access {}? {}", switch(*on), if *on { "Publishes the authenticated device door with Tailscale Serve, not to the public Internet." } else { "Phones using only this route lose access. Linked machines may keep their own door published." }),
            Self::Lan(on) => format!("Turn local-network phone access {}? {}", switch(*on), if *on { "Starts a TLS listener reachable on this Wi-Fi/Ethernet network and advertises it using Bonjour when available." } else { "Phones using only this Wi-Fi route lose access." }),
            Self::Cancel(entry) => format!("Take back this held write to {}? Cancellation succeeds only if it never left; otherwise the server refuses. Server-exposed words will be saved as a new Draft explicitly labelled as a recovered preview. The server may truncate long outbox words: this is not guaranteed to recover the full original.\n\n{}\n\n{}", entry.machine_name.as_deref().unwrap_or(&entry.machine), entry.route, entry.words.as_deref().unwrap_or("(no words)")),
            Self::Dismiss(entry) => format!("Dismiss this delivery notice? This does NOT cancel or resend the write. Copy any words you need first.\n\n{}\n\n{}", entry.description(None), entry.words.as_deref().unwrap_or("(no words)")),
            Self::Update(machine) => format!("Start/retry the update or restart on {}? It installs that machine's own newest release, then restarts its Inbox. Open reviews may delay the restart. This action is never queued for an asleep machine.", machine.name),
            Self::AskUpdate { machine, agent, .. } => format!("Ask {} on {} to update Plannotator and restart its Inbox? The agent will receive:\n\n{ASK_UPDATE}", agent.host, machine.name),
            Self::RememberAsked { .. } => "The update request was already sent or held. Retry only recording its status on this Inbox? This does not send the agent another message.".into(),
            Self::RestoreDraft { .. } => "The write is already cancelled. Retry saving the server-exposed words as a new Draft? Long outbox words may be truncated; this is not guaranteed to be the full original. This does not send anything or overwrite an existing draft.".into(),
        }
    }
}

fn switch(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

fn expired(at: &str) -> bool {
    time::OffsetDateTime::parse(at, &time::format_description::well_known::Rfc3339)
        .is_ok_and(|deadline| deadline <= time::OffsetDateTime::now_utc())
}

#[derive(Default)]
struct Pane {
    tab: Tab,
    list: ListState,
    machines: Vec<Machine>,
    devices: Vec<Device>,
    outbox: Vec<OutboxEntry>,
    /// Cancelled writes vanish from GET /outbox. Retain their words until explicitly
    /// dismissed or the person closes the pane; no optimistic cancellation.
    cancelled: Vec<OutboxEntry>,
    access: Option<Access>,
    lan: Option<Lan>,
    tailnet: Option<Tailnet>,
    checks: Option<Checks>,
    pairing: Option<PairingView>,
    phone_offer: Option<PairingOffer>,
    machine_offer: Option<MachineOffer>,
    qr: Vec<String>,
    show_offer: bool,
    update: Option<(String, UpdateDetails)>,
    agent: Option<Agent>,
    compose_machine: Option<String>,
    link: Option<Compose>,
    pending: Option<Action>,
    status: String,
    scroll: u16,
}

impl Pane {
    fn local(&self) -> bool {
        self.access.as_ref().is_some_and(Access::local)
    }
    fn owner(&self) -> bool {
        self.access.as_ref().is_some_and(Access::owner)
    }
    fn phones(&self) -> bool {
        self.access.as_ref().is_some_and(|a| a.phones)
    }

    fn len(&self) -> usize {
        match self.tab {
            Tab::Machines => self.machines.len(),
            Tab::Phones => self.devices.len(),
            Tab::Network => 2,
            Tab::Outbox => self.outbox.len() + self.cancelled.len(),
        }
    }

    fn machine(&self) -> Option<&Machine> {
        self.list.selected().and_then(|i| self.machines.get(i))
    }
    fn device(&self) -> Option<&Device> {
        self.list.selected().and_then(|i| self.devices.get(i))
    }
    fn entry(&self) -> Option<&OutboxEntry> {
        self.list.selected().and_then(|i| self.outbox.iter().chain(&self.cancelled).nth(i))
    }

    fn select(&mut self, delta: isize) {
        let max = self.len().saturating_sub(1);
        self.list.select(Some(self.list.selected().unwrap_or(0).saturating_add_signed(delta).min(max)));
        self.scroll = 0;
    }

    fn stage(&mut self, action: Action) -> Result<()> {
        ensure!(self.owner(), "Only this Inbox's owner can change machines, devices, or held writes.");
        if matches!(
            action,
            Action::OfferMachine
                | Action::Link(_)
                | Action::Unlink(_)
                | Action::PairPhone
                | Action::Revoke(_)
                | Action::Tailnet(_)
                | Action::Lan(_)
        ) {
            ensure!(
                self.local(),
                "Linking, pairing and device switches work only on this computer's own screen."
            );
        }
        if matches!(action, Action::PairPhone) {
            ensure!(
                self.lan.as_ref().is_some_and(|s| s.on) || self.tailnet.as_ref().is_some_and(|s| s.on),
                "Turn on Wi-Fi or Tailscale in Network before creating a pairing code. Checks are advisory; only the switches gate pairing."
            );
        }
        self.pending = Some(action);
        self.scroll = 0;
        Ok(())
    }

    fn refresh(&mut self, api: &Api<'_>, fresh: bool) {
        let mut errors = Vec::new();
        if self.access.is_none() || fresh {
            match api.access() {
                Ok(access) => self.access = Some(access),
                Err(e) => errors.push(format!("Settings: {e:#}")),
            }
        }
        match api.machines() {
            Ok(next) => {
                if next.machines.iter().filter(|m| !m.is_self).count()
                    > self.machines.iter().filter(|m| !m.is_self).count()
                {
                    self.machine_offer = None;
                }
                self.machines = next.machines;
                if self.machine_offer.as_ref().is_some_and(|offer| expired(&offer.expires_at)) {
                    self.machine_offer = None;
                }
            }
            Err(e) => errors.push(format!("Machines: {e:#}")),
        }
        if self.tab == Tab::Outbox || fresh {
            match api.outbox() {
                Ok(next) => {
                    self.outbox = next.waiting;
                    for entry in next.notices {
                        if !self.outbox.iter().any(|e| e.key == entry.key) {
                            self.outbox.push(entry);
                        }
                    }
                    self.outbox.sort_by(|a, b| a.queued_at.cmp(&b.queued_at));
                }
                Err(e) => errors.push(format!("Outbox: {e:#}")),
            }
        }
        if self.phones()
            && (matches!(self.tab, Tab::Phones | Tab::Network) || self.phone_offer.is_some() || fresh)
        {
            match api.devices() {
                Ok(devices) => self.devices = devices,
                Err(e) => errors.push(format!("Phones: {e:#}")),
            }
            match api.pairing() {
                Ok(offer) => {
                    if self.phone_offer.as_ref().is_some_and(|ours| {
                        offer
                            .as_ref()
                            .is_none_or(|o| o.state != "open" || o.expires_at != ours.offer.expires_at)
                    }) {
                        self.phone_offer = None;
                        self.qr.clear();
                    }
                    self.pairing = offer;
                }
                Err(e) => errors.push(format!("Pairing: {e:#}")),
            }
            if self.local() && (fresh || self.lan.is_none() || self.tailnet.is_none()) {
                match api.checks(fresh) {
                    Ok(next) => {
                        self.checks = Some(next.checks);
                        self.lan = Some(next.lan);
                        self.tailnet = Some(next.tailnet);
                    }
                    Err(e) => {
                        errors.push(format!("Pairing checks: {e:#}"));
                        match api.lan() {
                            Ok(lan) => self.lan = Some(lan),
                            Err(e) => errors.push(format!("Wi-Fi: {e:#}")),
                        }
                        match api.tailnet() {
                            Ok(tailnet) => self.tailnet = Some(tailnet),
                            Err(e) => errors.push(format!("Tailnet: {e:#}")),
                        }
                    }
                }
            }
            if !fresh && self.tab == Tab::Network && self.local() {
                match api.lan() {
                    Ok(lan) => self.lan = Some(lan),
                    Err(e) => errors.push(format!("Wi-Fi: {e:#}")),
                }
                match api.tailnet() {
                    Ok(tailnet) => self.tailnet = Some(tailnet),
                    Err(e) => errors.push(format!("Tailnet: {e:#}")),
                }
            }
        }
        if let Some((id, details)) = &mut self.update
            && self.machines.iter().any(|machine| machine.id == *id && machine.state == "online")
        {
            match api.update_details(id) {
                Ok(next) => *details = next,
                Err(e) => errors.push(format!("Update: {e:#}")),
            }
            if details.too_old
                || self
                    .machines
                    .iter()
                    .find(|machine| machine.id == *id)
                    .and_then(|m| m.update.as_ref())
                    .is_some_and(|u| !u.can_update)
            {
                match api.agent_to_ask(id) {
                    Ok(agent) => self.agent = agent,
                    Err(e) => errors.push(format!("Agents there: {e:#}")),
                }
            }
        }
        self.select(0);
        if !errors.is_empty() {
            self.status = errors.join(" | ");
        }
    }

    fn open_update(&mut self, api: &Api<'_>) -> Result<()> {
        let machine = self.machine().ok_or_else(|| anyhow!("Select a linked machine."))?;
        ensure!(!machine.is_self, "Use Inbox Settings to update this machine.");
        let id = machine.id.clone();
        let mut details = UpdateDetails {
            current: machine.version.clone(),
            too_old: machine.state == "needs_update",
            ..UpdateDetails::default()
        };
        let mut warning = None;
        if machine.state == "online" {
            match api.update_details(&id) {
                Ok(next) => details = next,
                Err(error) => warning = Some(format!("Update details: {error:#}")),
            }
        }
        let old = details.too_old || machine.update.as_ref().is_some_and(|u| !u.can_update);
        self.agent = if old && machine.state == "online" {
            match api.agent_to_ask(&id) {
                Ok(agent) => agent,
                Err(error) => {
                    warning = Some(format!("Agent list: {error:#}"));
                    None
                }
            }
        } else {
            None
        };
        self.update = Some((id, details));
        if let Some(warning) = warning {
            self.status = warning;
        }
        self.scroll = 0;
        Ok(())
    }

    fn execute(&mut self, api: &Api<'_>) -> Result<()> {
        let Some(action) = self.pending.clone() else {
            return Ok(());
        };
        let message = match &action {
            Action::OfferMachine => {
                self.machine_offer = Some(api.offer_machine()?);
                self.phone_offer = None;
                self.show_offer = true;
                "One-use link code opened; Tailscale publication status is shown with it.".into()
            }
            Action::Link(input) => format!("Linked with {}.", api.link_machine(input)?.name),
            Action::Unlink(machine) => {
                let answer = api.unlink(&machine.id)?;
                if answer.told {
                    format!(
                        "Unlinked {} on both machines. Any unsent words remain in Outbox.",
                        answer.unlinked.name
                    )
                } else {
                    format!(
                        "Unlinked here. {} could not be reached; it drops the link on its next contact. This computer remains reachable for it for up to 7 days until it hears. Unsent words remain in Outbox.",
                        answer.unlinked.name
                    )
                }
            }
            Action::PairPhone => {
                let offer = api.pair_phone()?;
                self.qr = view::qr_lines(&offer.link)?;
                self.phone_offer = Some(offer);
                self.machine_offer = None;
                self.show_offer = true;
                "Pairing code opened. Scan in the Plannotator app; never type the code over Wi-Fi.".into()
            }
            Action::Revoke(device) => format!("Removed {}.", api.revoke(&device.id)?.name),
            Action::Tailnet(on) => {
                self.tailnet = Some(api.set_tailnet(*on)?);
                format!("Phone tailnet access {}.", switch(*on))
            }
            Action::Lan(on) => {
                self.lan = Some(api.set_lan(*on)?);
                format!("Wi-Fi access {}.", switch(*on))
            }
            Action::Cancel(entry) => {
                let cancelled = api.cancel(&entry.key)?;
                let words = cancelled.words.clone().or_else(|| entry.words.clone());
                self.cancelled.retain(|e| e.key != cancelled.key);
                self.cancelled.push(OutboxEntry { words: words.clone(), ..cancelled });
                if let Some(words) = words.filter(|words| !words.is_empty()) {
                    let words = format!(
                        "[Recovered held-write preview: the server may have truncated these words. Check the original before sending.]\n\n{words}"
                    );
                    let id = recovery::draft_id();
                    self.pending = Some(Action::RestoreDraft { id: id.clone(), body: words.clone() });
                    recovery::save_draft(api.0, &id, &words)?;
                    "Cancelled; never sent. Server-exposed words are in a new Draft and retained here. Long outbox words may be truncated.".into()
                } else {
                    "Cancelled; the server confirmed it never left. This write carried no words.".into()
                }
            }
            Action::Dismiss(entry) => {
                api.dismiss(&entry.key)?;
                self.cancelled.retain(|e| e.key != entry.key);
                "Delivery notice dismissed. No write was cancelled or resent.".into()
            }
            Action::Update(machine) => format!(
                "{}: {}. Waiting for that machine's own status.",
                machine.name,
                api.start_update(&machine.id)?.outcome
            ),
            Action::AskUpdate { machine, agent, key } => {
                let (thread, status) = match api.ask_update(&machine.id, agent, key) {
                    Ok(thread) => {
                        (Some(thread), format!("Asked {} on {} to update.", agent.host, machine.name))
                    }
                    Err(error) => {
                        if let Some(held) = error.downcast_ref::<HeldWrite>() {
                            self.outbox.retain(|entry| entry.key != held.entry.key);
                            self.outbox.push(held.entry.clone());
                            (None, held.to_string())
                        } else {
                            return Err(error);
                        }
                    }
                };
                self.pending = Some(Action::RememberAsked {
                    machine: machine.id.clone(),
                    thread: thread.clone(),
                    agent: agent.host.clone(),
                });
                api.update_asked(&machine.id, thread.as_deref(), &agent.host)?;
                status
            }
            Action::RememberAsked { machine, thread, agent } => {
                api.update_asked(machine, thread.as_deref(), agent)?;
                "Update request status recorded; no second message sent.".into()
            }
            Action::RestoreDraft { id, body } => {
                recovery::save_draft(api.0, id, body)?;
                "Server-exposed cancelled words saved in a new Draft. Long outbox words may be truncated. Nothing was sent.".into()
            }
        };
        self.pending = None;
        self.refresh(api, true);
        self.status = message;
        self.scroll = 0;
        Ok(())
    }

    fn copy(&mut self) -> Result<()> {
        let text = if self.show_offer {
            self.phone_offer
                .as_ref()
                .map(|o| o.link.as_str())
                .or_else(|| self.machine_offer.as_ref().map(|o| o.link.as_str()))
        } else if self.update.is_some() {
            Some(ASK_UPDATE)
        } else {
            match self.tab {
                Tab::Outbox => self.entry().and_then(|e| e.words.as_deref()),
                Tab::Machines => self.machine().and_then(|m| m.address.as_deref()),
                Tab::Network => {
                    if self.list.selected() == Some(1) {
                        self.tailnet.as_ref().and_then(|s| s.address.as_deref())
                    } else {
                        self.lan.as_ref().and_then(|s| s.fingerprint.as_deref())
                    }
                }
                Tab::Phones => None,
            }
        }
        .ok_or_else(|| anyhow!("Nothing to copy in this selection."))?;
        Clipboard.deliver(text).map_err(|e| anyhow!("Clipboard: {e}"))?;
        self.status = "Copied to the terminal clipboard.".into();
        Ok(())
    }

    fn key(&mut self, key: KeyEvent, api: &Api<'_>) -> Result<bool> {
        if key.kind == KeyEventKind::Release {
            return Ok(false);
        }
        if let Some(compose) = &mut self.link {
            match compose.handle_key(key) {
                ComposeAction::Save => {
                    let input = compose.value();
                    api::link_input(&input)?;
                    self.stage(Action::Link(input))?;
                    self.link = None;
                }
                ComposeAction::Cancel => self.link = None,
                ComposeAction::Edited => {}
            }
            return Ok(false);
        }
        if self.pending.is_some() {
            match key.code {
                KeyCode::Char('y') => self.execute(api)?,
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.pending = None;
                    self.scroll = 0;
                }
                KeyCode::PageDown | KeyCode::Down => self.scroll = self.scroll.saturating_add(1),
                KeyCode::PageUp | KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
                _ => {}
            }
            return Ok(false);
        }
        if key.code == KeyCode::Esc || key.code == KeyCode::Char('q') {
            if self.show_offer {
                self.show_offer = false;
                self.scroll = 0;
            } else if self.update.take().is_none() {
                return Ok(true);
            }
            return Ok(false);
        }
        if key.code == KeyCode::Char('c') {
            self.copy()?;
            return Ok(false);
        }
        if key.code == KeyCode::Char('r') {
            self.refresh(api, true);
            return Ok(false);
        }
        if self.show_offer {
            match key.code {
                KeyCode::Down | KeyCode::PageDown => {
                    self.scroll =
                        self.scroll.saturating_add(if key.code == KeyCode::PageDown { 10 } else { 1 });
                }
                KeyCode::Up | KeyCode::PageUp => {
                    self.scroll =
                        self.scroll.saturating_sub(if key.code == KeyCode::PageUp { 10 } else { 1 });
                }
                KeyCode::Char('n') if self.tab == Tab::Phones => self.stage(Action::PairPhone)?,
                KeyCode::Char('n') => self.stage(Action::OfferMachine)?,
                _ => {}
            }
            return Ok(false);
        }
        if let Some((id, details)) = &self.update {
            let machine = self
                .machines
                .iter()
                .find(|m| m.id == *id)
                .ok_or_else(|| anyhow!("Machine is no longer linked."))?
                .clone();
            let old = details.too_old || machine.update.as_ref().is_some_and(|u| !u.can_update);
            match key.code {
                KeyCode::Char('u') => {
                    ensure!(!old, "This machine needs an update by an agent or on the machine itself.");
                    self.stage(Action::Update(machine))?;
                }
                KeyCode::Char('a') => {
                    ensure!(old, "This machine supports its own update. Use u.");
                    let agent = self.agent.clone().ok_or_else(|| anyhow!("No running agent there. Start one with New message, or run the displayed commands on that machine."))?;
                    self.stage(Action::AskUpdate { machine, agent, key: super::send::new_key() })?;
                }
                KeyCode::Char('A') => {
                    ensure!(
                        old && self.agent.is_none(),
                        "Use a to ask the running agent, or u for a supported self-update."
                    );
                    ensure!(self.owner(), "Only this Inbox's owner can start an agent on a linked machine.");
                    ensure!(
                        machine.capabilities.launch == Some(true),
                        "This machine does not report agent launch as available. Run the displayed commands there."
                    );
                    self.compose_machine = Some(machine.id);
                }
                KeyCode::Down | KeyCode::PageDown => self.scroll = self.scroll.saturating_add(3),
                KeyCode::Up | KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(3),
                _ => {}
            }
            return Ok(false);
        }
        self.section_key(key, api)?;
        Ok(false)
    }

    fn section_key(&mut self, key: KeyEvent, api: &Api<'_>) -> Result<()> {
        match key.code {
            KeyCode::Tab | KeyCode::BackTab => {
                self.tab = self
                    .tab
                    .next(key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT));
                self.list.select(Some(0));
                self.scroll = 0;
                self.refresh(api, false);
            }
            KeyCode::Up | KeyCode::Char('k') => self.select(-1),
            KeyCode::Down | KeyCode::Char('j') => self.select(1),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(8),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(8),
            KeyCode::Char('l') if self.tab == Tab::Machines => {
                ensure!(self.local(), "Linking requires this computer's own screen.");
                self.link = Some(Compose::default());
            }
            KeyCode::Char('o') if self.tab == Tab::Machines => {
                if self.machine_offer.is_some() {
                    self.show_offer = true;
                } else {
                    self.stage(Action::OfferMachine)?;
                }
            }
            KeyCode::Char('u') | KeyCode::Enter if self.tab == Tab::Machines => self.open_update(api)?,
            KeyCode::Char('x') if self.tab == Tab::Machines => {
                if let Some(machine) = self.machine().cloned() {
                    ensure!(!machine.is_self, "This is this computer, not a linked machine.");
                    self.stage(Action::Unlink(machine))?;
                }
            }
            KeyCode::Char('p') if self.tab == Tab::Phones => {
                ensure!(self.phones(), "Phones are disabled in this Inbox. Enable them in Settings first.");
                if self.phone_offer.is_some() {
                    self.show_offer = true;
                } else {
                    self.stage(Action::PairPhone)?;
                }
            }
            KeyCode::Char('x') if self.tab == Tab::Phones => {
                if let Some(device) = self.device().cloned() {
                    self.stage(Action::Revoke(device))?;
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') if self.tab == Tab::Network => {
                ensure!(self.phones(), "Phones are disabled in this Inbox. Enable them in Settings first.");
                if self.list.selected() == Some(1) {
                    let on = self
                        .tailnet
                        .as_ref()
                        .ok_or_else(|| anyhow!("Tailnet state has not loaded; r retries the read."))?
                        .on;
                    self.stage(Action::Tailnet(!on))?;
                } else {
                    let on = self
                        .lan
                        .as_ref()
                        .ok_or_else(|| anyhow!("Wi-Fi state has not loaded; r retries the read."))?
                        .on;
                    self.stage(Action::Lan(!on))?;
                }
            }
            KeyCode::Char('x') if self.tab == Tab::Outbox => {
                if let Some(entry) = self.entry().cloned() {
                    ensure!(
                        entry.pending(),
                        "Only waiting writes can be cancelled. This one is {}.",
                        entry.state
                    );
                    self.stage(Action::Cancel(entry))?;
                }
            }
            KeyCode::Char('d') if self.tab == Tab::Outbox => {
                if let Some(entry) = self.entry().cloned() {
                    ensure!(
                        !entry.pending(),
                        "Dismiss does not cancel a pending write. Use x to request cancellation."
                    );
                    self.stage(Action::Dismiss(entry))?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// Modal on the caller's terminal; never changes raw mode, initializes another
/// terminal, starts a server, or performs a mutation while opening/refreshing.
pub(crate) fn run(client: &Client, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
    let api = Api(client);
    let mut pane = Pane::default();
    pane.list.select(Some(0));
    pane.refresh(&api, true);
    let mut refreshed = Instant::now();
    loop {
        terminal.draw(|frame| view::draw(frame, &mut pane))?;
        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(key) => match pane.key(key, &api) {
                    Ok(true) => return Ok(()),
                    Ok(false) => {}
                    Err(error) => {
                        pane.status = format!(
                            "{error:#}{}",
                            if pane.pending.is_some() {
                                " · Not confirmed. y retries the same action; n leaves it alone."
                            } else {
                                ""
                            }
                        );
                    }
                },
                Event::Paste(text) => {
                    if let Some(compose) = &mut pane.link {
                        compose.insert_text(&text);
                    }
                }
                _ => {}
            }
        }
        if let Some(machine) = pane.compose_machine.take() {
            match super::launch::compose_outcome_on(
                client,
                terminal,
                Some(&machine),
                ASK_UPDATE,
                Some("Update Plannotator"),
                false,
            ) {
                Ok(super::launch::ComposeOutcome::Cancelled) => {}
                Ok(outcome) => {
                    let (thread, status) = match outcome {
                        super::launch::ComposeOutcome::Started(started) => (
                            Some(started.thread_id),
                            "Update request sent; waiting for that machine's own status.",
                        ),
                        super::launch::ComposeOutcome::Held => {
                            (None, "Update request held for that machine; NOT delivered yet.")
                        }
                        super::launch::ComposeOutcome::Cancelled => continue,
                    };
                    pane.pending = Some(Action::RememberAsked {
                        machine: machine.clone(),
                        thread: thread.clone(),
                        agent: "An agent".into(),
                    });
                    match api.update_asked(&machine, thread.as_deref(), "An agent") {
                        Ok(()) => {
                            pane.pending = None;
                            pane.status = status.into();
                        }
                        Err(error) => {
                            pane.status = format!(
                                "{status} Recording that status failed: {error:#}. y retries only the status record."
                            );
                        }
                    }
                }
                Err(error) => pane.status = format!("{error:#}"),
            }
        }
        if refreshed.elapsed() >= REFRESH && pane.pending.is_none() && pane.link.is_none() {
            let scroll = pane.scroll;
            pane.refresh(&api, false);
            pane.scroll = scroll;
            refreshed = Instant::now();
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests assert by panicking")]
mod tests {
    use super::*;

    #[test]
    fn pairing_gates_on_switch_not_advisory_checks() {
        let mut pane = Pane {
            access: Some(Access { phones: true, via: Some("local".into()), guest: None }),
            ..Pane::default()
        };
        assert!(pane.stage(Action::PairPhone).is_err());
        pane.lan = Some(Lan { on: true, error: Some("advisory failure".into()), ..Lan::default() });
        pane.stage(Action::PairPhone).expect("switch is on");
        assert!(matches!(pane.pending, Some(Action::PairPhone)));
    }

    #[test]
    fn remote_readers_cannot_stage_local_only_writes() {
        let mut pane = Pane {
            access: Some(Access { phones: true, via: Some("tailnet".into()), guest: Some(false) }),
            ..Pane::default()
        };
        assert!(pane.stage(Action::OfferMachine).is_err());
        assert!(pane.stage(Action::Lan(true)).is_err());
        assert!(pane.pending.is_none());
    }

    #[test]
    fn unlink_confirmation_names_held_write_loss() {
        let text =
            Action::Unlink(Machine { name: "Mini".into(), waiting: 2, ..Machine::default() }).confirmation();
        assert!(text.contains("2 held write(s)"));
        assert!(text.contains("may have been delivered"));
    }
}
