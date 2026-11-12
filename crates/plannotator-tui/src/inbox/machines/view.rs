use std::fmt::Write as _;

use anyhow::{Context, Result};
use qrcode::{Color as QrColor, QrCode};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};

use super::model::{Checks, Machine, UpdateDetails};
use super::{Pane, Tab, switch};

pub(super) fn qr_lines(link: &str) -> Result<Vec<String>> {
    let code = QrCode::new(link.as_bytes()).context("encoding phone pairing QR")?;
    let width = code.width() + 8;
    let dark = |x: usize, y: usize| {
        x >= 4 && y >= 4 && x < width - 4 && y < width - 4 && code[(x - 4, y - 4)] == QrColor::Dark
    };
    Ok((0..width)
        .step_by(2)
        .map(|y| {
            (0..width)
                .map(|x| match (dark(x, y), y + 1 < width && dark(x, y + 1)) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    (false, false) => ' ',
                })
                .collect()
        })
        .collect())
}

pub(super) fn draw(frame: &mut Frame<'_>, pane: &mut Pane) {
    let area = frame.area();
    frame.render_widget(Clear, area);
    let [header, content, status, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(4),
        Constraint::Length(4),
        Constraint::Length(2),
    ])
    .areas(area);
    let title = format!("Machines & devices  ·  {}  ·  Tab changes section", pane.tab.title());
    let tabs = [Tab::Machines, Tab::Phones, Tab::Network, Tab::Outbox].map(|tab| {
        Span::styled(
            format!(" {} ", tab.title()),
            if tab == pane.tab {
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            },
        )
    });
    frame.render_widget(Paragraph::new(vec![Line::from(title), Line::from(Vec::from(tabs))]), header);
    if let Some(action) = &pane.pending {
        frame.render_widget(
            Paragraph::new(action.confirmation()).wrap(Wrap { trim: false }).scroll((pane.scroll, 0)).block(
                Block::default()
                    .title(" Confirm · y sends · n/Esc cancels · arrows scroll ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Yellow)),
            ),
            content,
        );
    } else if let Some(compose) = &pane.link {
        let inner = Block::default()
            .title(" Link: paste plannotator://link?… or address 123456 · Enter reviews · Esc cancels ")
            .borders(Borders::ALL);
        let rect = inner.inner(content);
        let (lines, row, col) = compose.wrapped(usize::from(rect.width.max(1)));
        frame.render_widget(inner, content);
        frame.render_widget(Paragraph::new(lines.join("\n")), rect);
        if row < usize::from(rect.height) {
            frame.set_cursor_position((rect.x.saturating_add(col as u16), rect.y.saturating_add(row as u16)));
        }
    } else if pane.show_offer {
        offer(frame, content, pane);
    } else if let Some((id, details)) = &pane.update {
        let machine = pane.machines.iter().find(|m| &m.id == id);
        let text = update_text(machine, details, pane.agent.as_ref().map(|a| a.host.as_str()));
        frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }).scroll((pane.scroll, 0))
            .block(Block::default().title(" Update · u update/restart · a ask running agent · A start agent · c copy ask · Esc back ").borders(Borders::ALL)), content);
    } else {
        draw_section(frame, content, pane);
    }
    frame.render_widget(
        Paragraph::new(pane.status.as_str())
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(Color::Yellow)),
        status,
    );
    let help = match pane.tab {
        Tab::Machines => "j/k select · l link · o show link code · x unlink · u update details",
        Tab::Phones => "j/k select · p pair/show code · x remove phone",
        Tab::Network => {
            "j/k select · Enter toggle · r check again · c copy Wi-Fi fingerprint/tailnet address"
        }
        Tab::Outbox => "j/k select · x cancel if never sent · d dismiss notice · c copy held words",
    };
    frame.render_widget(
        Paragraph::new(format!("{help}\nr refresh (GET only) · PgUp/PgDn scroll details · Esc/q back")),
        footer,
    );
}

fn draw_section(frame: &mut Frame<'_>, area: Rect, pane: &mut Pane) {
    let [list, details] =
        Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(area);
    let rows = section_rows(pane);
    frame.render_stateful_widget(
        List::new(rows)
            .block(Block::default().title(pane.tab.title()).borders(Borders::ALL))
            .highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
            .highlight_symbol("› "),
        list,
        &mut pane.list,
    );
    let mut text = if pane.local() {
        String::new()
    } else {
        "This connection is not confirmed local. Linking, pairing and phone switches are unavailable here.\n\n".to_owned()
    };
    match pane.tab {
        Tab::Machines => append_machine_details(&mut text, pane),
        Tab::Phones => {
            if !pane.phones() {
                text.push_str("Phones are disabled in this Inbox. Enable Phones in Settings first.\n");
            }
            if let Some(device) = pane.device() {
                let _ = writeln!(
                    text,
                    "{}\nPlatform: {}\nPaired: {}\nLast seen: {}\nRevoked: {}\nCarriage: {}\n\nRemove invalidates this phone's own key immediately.\n",
                    device.name,
                    device.platform,
                    device.created_at,
                    device.last_seen_at,
                    device.revoked_at.as_deref().unwrap_or("no"),
                    device.carriage
                );
            } else {
                text.push_str("No phone paired yet.\n\n1. Network: enable Wi-Fi and/or Tailscale.\n2. p: create a one-use pairing QR.\n3. Scan in the Plannotator phone app (not Camera).\n\n");
            }
            pairing_status(&mut text, pane);
            text.push_str("\nWi-Fi: use the same non-guest network and allow Local Network access on the phone. VPNs may hide nearby computers.\nTailscale: the phone's Tailscale app must be on and signed into the same tailnet.\n\nThe browser's separate 'Over your tailnet' switch lives in Settings; phone pairing does not need it.");
        }
        Tab::Network => {
            if !pane.phones() {
                text.push_str("Phone routes are disabled. Enable Phones in Settings first.\n\n");
            }
            if let Some(lan) = &pane.lan {
                let _ = writeln!(
                    text,
                    "Wi-Fi: {}\nAddress: {}\nBonjour discovery: {}\nCertificate SHA-256:\n{}",
                    switch(lan.on),
                    lan.address.as_deref().unwrap_or("not published"),
                    lan.bonjour,
                    lan.fingerprint.as_deref().unwrap_or("none")
                );
                if let Some(error) = &lan.error {
                    let _ = writeln!(text, "Wi-Fi error: {error}");
                }
            }
            if let Some(tailnet) = &pane.tailnet {
                let _ = writeln!(
                    text,
                    "\nPhone Tailscale: {}\nAddress: {}",
                    switch(tailnet.on),
                    tailnet.address.as_deref().unwrap_or("not published")
                );
                if let Some(error) = &tailnet.error {
                    let _ = writeln!(text, "Tailscale error: {error}");
                }
            }
            if let Some(checks) = &pane.checks {
                text.push_str(&checks_text(checks));
            }
            text.push_str("\nChecks are advisory. A failed check does not block pairing when a way is switched on.\n\nPhone: same Wi-Fi, Local Network permission on, no isolating guest network/VPN; or Tailscale connected to the same tailnet.\n\nThis controls the phone/device door. The browser's owner-only tailnet window is a separate switch in Settings.");
        }
        Tab::Outbox => {
            if let Some(entry) = pane.entry() {
                text.push_str(&entry.description(pane.machines.iter().find(|m| m.id == entry.machine)));
                let _ = write!(
                    text,
                    "\n\nKey: {}\nRoute: {}\nQueued: {}\nSettled: {}\nThread: {}\nHTTP status: {}\n\n{}",
                    entry.key,
                    entry.route,
                    entry.queued_at,
                    entry.settled_at.as_deref().unwrap_or("not yet"),
                    entry.thread.as_deref().unwrap_or("not supplied"),
                    entry.status.map_or_else(|| "none".into(), |s| s.to_string()),
                    entry.words.as_deref().unwrap_or("This write carried no words.")
                );
                if let Some(error) = entry.answer.get("error").and_then(serde_json::Value::as_str) {
                    let _ = write!(text, "\n\nServer: {error}");
                }
                text.push_str("\n\nQueued/sending is not delivery. Cancel only succeeds before the write ever left. Dismiss hides a settled notice; it does not cancel or resend. Copy words before dismissing.");
            } else {
                text.push_str("No held writes or undismissed delivery notices.\n\nA remote write is delivered only after that machine's Inbox confirms it. Waiting entries and words survive asleep/unlinked machines on the server.");
            }
        }
    }
    frame.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .scroll((pane.scroll, 0))
            .block(Block::default().title(" Details ").borders(Borders::ALL)),
        details,
    );
}

fn section_rows(pane: &Pane) -> Vec<ListItem<'static>> {
    match pane.tab {
        Tab::Machines => pane
            .machines
            .iter()
            .map(|machine| {
                ListItem::new(vec![
                    Line::from(format!(
                        "{}{}",
                        machine.name,
                        if machine.is_self { " (this computer)" } else { "" }
                    )),
                    Line::from(format!("{} · {} waiting", machine.state, machine.waiting)),
                ])
            })
            .collect(),
        Tab::Phones => pane
            .devices
            .iter()
            .map(|device| {
                ListItem::new(vec![
                    Line::from(device.name.clone()),
                    Line::from(format!("{} · {}", device.platform, device.last_seen_at)),
                ])
            })
            .collect(),
        Tab::Network => vec![
            ListItem::new(format!("Wi-Fi / LAN: {}", pane.lan.as_ref().map_or("unknown", |s| switch(s.on)))),
            ListItem::new(format!(
                "Phone Tailscale: {}",
                pane.tailnet.as_ref().map_or("unknown", |s| switch(s.on))
            )),
        ],
        Tab::Outbox => pane
            .outbox
            .iter()
            .chain(&pane.cancelled)
            .map(|entry| {
                ListItem::new(vec![
                    Line::from(format!(
                        "{} · {}",
                        entry
                            .machine_name
                            .as_deref()
                            .or_else(|| pane
                                .machines
                                .iter()
                                .find(|m| m.id == entry.machine)
                                .map(|m| m.name.as_str()))
                            .unwrap_or(&entry.machine),
                        entry.state
                    )),
                    Line::from(entry.route.clone()),
                ])
            })
            .collect(),
    }
}

fn append_machine_details(text: &mut String, pane: &Pane) {
    if let Some(machine) = pane.machine() {
        let _ = writeln!(
            text,
            "{}\n{}\nState since: {}\nVersion: {}\nAddress: {}\nLink both ways: {}\n{} waiting to send\n",
            machine.name,
            machine.state,
            machine.since,
            machine.version.as_deref().unwrap_or("unknown"),
            machine.address.as_deref().unwrap_or("not published"),
            machine.both_ways,
            machine.waiting
        );
        if let Some(reason) = &machine.reason {
            let _ = writeln!(text, "Needs attention: {reason}\n");
        }
        if let Some(update) = &machine.update {
            let _ = writeln!(
                text,
                "Update: {}\nFrom: {} → {}",
                update.phase,
                update.from.as_deref().unwrap_or("unknown"),
                update.target.as_deref().unwrap_or("newest release")
            );
            if let Some(asked) = &update.asked {
                let _ = write!(text, "Asked {} at {}", asked.agent, asked.at);
                if let Some(thread) = &asked.thread {
                    let _ = write!(text, " · thread {thread}");
                }
                text.push('\n');
            }
        }
        let _ = writeln!(
            text,
            "\nSnooze supported: {}\nDelete message words: {}\nLaunch agents: {}",
            machine.capabilities.snoozes,
            machine.capabilities.message_deletions,
            machine.capabilities.launch.map_or("not reported", switch)
        );
        if machine.state == "needs_update" {
            text.push_str(
                "\nThis machine needs an update, not a reconnect. Press u for update instructions.\n",
            );
        }
    } else {
        text.push_str("No linked machines.\n\nl: paste another machine's link code.\no: publish this machine's one-use code with Tailscale Serve.\n\nLinks work both ways; Unlink removes the link from both computers.");
    }
}

fn offer(frame: &mut Frame<'_>, area: Rect, pane: &Pane) {
    if let Some(offer) = &pane.phone_offer {
        let width = pane.qr.first().map_or(0, |line| line.chars().count());
        let height = pane.qr.len();
        let needed_height = height.saturating_add(8);
        let mut text = format!(
            "Pair {} · expires {}\nSix digits (Tailscale only): {}\nTailnet: {}\nWi-Fi: {}\nFingerprint: {}\n",
            offer.computer.name,
            offer.offer.expires_at,
            offer.offer.code,
            offer.addresses.tailnet.as_deref().unwrap_or("none"),
            offer.addresses.lan.as_deref().unwrap_or("none"),
            offer.addresses.fingerprint.as_deref().unwrap_or("none")
        );
        if offer.reachable == Some(false) {
            text.push_str("WARNING: the code has no currently reachable address. Fix a Network route, then make a new code.\n");
        }
        if usize::from(area.width) >= width && usize::from(area.height) >= needed_height {
            let qr = Rect::new(area.x + (area.width - width as u16) / 2, area.y, width as u16, height as u16);
            frame.render_widget(
                Paragraph::new(pane.qr.join("\n")).style(Style::default().fg(Color::Black).bg(Color::White)),
                qr,
            );
            let detail = Rect::new(area.x, area.y + height as u16, area.width, area.height - height as u16);
            text.push_str("Scan in the Plannotator app · c copies link · n new code · Esc hides code\n");
            pairing_status(&mut text, pane);
            frame.render_widget(
                Paragraph::new(text).wrap(Wrap { trim: false }).scroll((pane.scroll, 0)),
                detail,
            );
        } else {
            let _ = writeln!(
                text,
                "\nEnlarge terminal: QR needs at least {width} columns and {} total rows. It is NOT shown cropped because a partial QR cannot pair.\n\n{}\n\nc copies the full pairing link. Wi-Fi requires QR scanning; six digits only work over Tailscale. n replaces this code.",
                needed_height + 8,
                offer.link
            );
            frame.render_widget(
                Paragraph::new(text).wrap(Wrap { trim: false }).scroll((pane.scroll, 0)),
                area,
            );
        }
    } else if let Some(offer) = &pane.machine_offer {
        let text = format!(
            "One-use link code for {} ({})\nExpires: {}\n\n{}\n\nOr address: {}\nSix digits: {}\n\n{}\n\nPaste into Machines on the other computer. This Inbox stays published on your tailnet while a code is open or a machine is linked.\n\nc copies link · n replaces code · Esc hides it",
            offer.machine.name,
            offer.machine.id,
            offer.expires_at,
            offer.link,
            offer.tailnet.as_deref().unwrap_or("not published"),
            offer.code,
            offer.tailnet_error.as_deref().unwrap_or("Tailscale publication has no reported error.")
        );
        frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }).scroll((pane.scroll, 0)), area);
    } else {
        let mut text = "This code is no longer open.\n\n".to_owned();
        pairing_status(&mut text, pane);
        text.push_str("\nNo phone arrived? Check the same Wi-Fi (not guest or mobile data), Local Network permission and any VPN. For Tailscale check the app is on, on the same tailnet. Scan in Plannotator, not Camera.\n\nn creates a new one · Esc returns");
        frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }).scroll((pane.scroll, 0)), area);
    }
}

fn pairing_status(text: &mut String, pane: &Pane) {
    if let Some(view) = &pane.pairing {
        let _ = writeln!(
            text,
            "Last pairing code: {} · expires {} · {} tries left",
            view.state, view.expires_at, view.tries_left
        );
        for attempt in view.attempts.iter().rev() {
            let _ = writeln!(text, "{}: {}", attempt.at, attempt.description());
        }
    }
}

fn checks_text(checks: &Checks) -> String {
    let wifi = &checks.wifi;
    let tailscale = &checks.tailscale;
    let mut text = format!(
        "\nPAIRING CHECKS (r checks again)\nNetwork: {} · {}\nCertificate available: {} · OpenSSL: {}\nBonjour: {} · firewall: {}\n",
        wifi.network.as_deref().unwrap_or("name unavailable"),
        wifi.address.as_deref().unwrap_or("no Wi-Fi/Ethernet address"),
        wifi.certificate,
        wifi.openssl,
        wifi.bonjour,
        wifi.firewall
    );
    if wifi.address.is_none() {
        text.push_str("Join a network, then check again.\n");
    }
    if !wifi.certificate && !wifi.openssl {
        text.push_str("Install OpenSSL to make the certificate (Debian/Ubuntu: sudo apt install openssl).\n");
    }
    if !wifi.bonjour {
        text.push_str(
            "No Bonjour: nearby phones will not list this computer by name; QR pairing still works.\n",
        );
    }
    match wifi.firewall.as_str() {
        "on" => text
            .push_str("If macOS asks whether Plannotator may accept incoming connections, choose Allow.\n"),
        "block_all" => text.push_str(
            "Allow Plannotator in System Settings → Network → Firewall before phones can connect.\n",
        ),
        _ => {}
    }
    let _ = writeln!(
        text,
        "\nTailscale: {} · login {}",
        tailscale.state,
        tailscale.login.as_deref().unwrap_or("not reported")
    );
    match tailscale.state.as_str() {
        "cli_missing" => text.push_str("Install the Tailscale CLI (Tailscale → Settings → Install CLI), or download Tailscale and sign in with tailscale up.\n"),
        "no_answer" | "starting" => text.push_str("Check again in a moment.\n"),
        "stopped" => text.push_str("Open Tailscale, then check again.\n"),
        "signed_out" => text.push_str("Open Tailscale and sign in, then check again.\n"),
        "needs_approval" => text.push_str("Approve this computer in the tailnet admin console.\n"),
        _ => {}
    }
    if let Some(error) = &tailscale.error {
        let _ = writeln!(text, "Tailscale: {error}");
    }
    if let Some(certs) = tailscale.https_certs {
        text.push_str(if certs {
            "HTTPS certificates: on\n"
        } else {
            "Enable HTTPS Certificates in Tailscale admin console → DNS.\n"
        });
    }
    if let Some(free) = tailscale.port_free {
        let _ = writeln!(
            text,
            "Device HTTPS port {}: {}",
            tailscale.https_port.map_or_else(|| "8443–8450".into(), |p| p.to_string()),
            if free { "available" } else { "held by other Serve mappings" }
        );
        if !free {
            text.push_str("tailscale serve status lists mappings. Free an unused port, or toggle phone Tailscale off/on to choose another (phones then pair again).\n");
        }
    }
    text
}

fn update_text(machine: Option<&Machine>, details: &UpdateDetails, agent: Option<&str>) -> String {
    let name = machine.map_or("Linked machine", |m| m.name.as_str());
    let mut text = format!(
        "Update {name}\nCurrent: {}\nPlatform: {}\nOpen reviews: {}\n",
        details.current.as_deref().unwrap_or("unknown"),
        details.platform.as_deref().unwrap_or("not reported"),
        details.open_reviews.map_or_else(|| "not reported".into(), |n| n.to_string())
    );
    if let Some(machine) = machine {
        let _ = writeln!(text, "Machine: {}", machine.state);
        if let Some(update) = &machine.update {
            let _ = writeln!(
                text,
                "Update phase: {}\n{} → {}",
                update.phase,
                update.from.as_deref().unwrap_or("unknown"),
                update.target.as_deref().unwrap_or("newest release")
            );
            if let Some(asked) = &update.asked {
                let _ = writeln!(text, "Already asked {} at {}.", asked.agent, asked.at);
            }
            if update.phase == "not_back" {
                text.push_str("\nIf it stays away, run on that machine: plannotator inbox --background\n");
                if let Some(before) = update.from.as_deref().filter(|version| {
                    version.split('.').count() == 3
                        && version.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                }) {
                    let _ = writeln!(
                        text,
                        "If it does not start, reinstall the previous version:\ncurl -fsSL https://plannotator.ai/install.sh | bash -s -- --version v{before}\nPowerShell:\n& ([scriptblock]::Create((irm https://plannotator.ai/install.ps1))) -Version v{before}"
                    );
                }
            }
        }
    }
    if let Some(update) = &details.update {
        let _ = writeln!(text, "Release: {} · {} · {}", update.version, update.state, update.action);
    }
    if let Some(command) = &details.command {
        let _ = writeln!(text, "\nCommand supplied by that machine:\n{command}");
    }
    if let Some(log) = &details.log {
        let _ = writeln!(text, "\nLog on that machine: {log}");
    }
    if let Some(failure) = &details.failure {
        let _ = writeln!(text, "\nFailure: {failure}");
    }
    let old = details.too_old || machine.and_then(|m| m.update.as_ref()).is_some_and(|u| !u.can_update);
    if old {
        text.push_str("\nThis Inbox predates remote self-update.\n");
        if let Some(agent) = agent {
            let _ = writeln!(text, "a asks {agent} to update it (confirmation shows the exact request).");
        } else {
            text.push_str("No agent running there. A opens New message on that machine to start one when launch is available.\n");
        }
        text.push_str("\nOr run on that computer:\nplannotator update && plannotator inbox restart\nFull path:\n~/.local/bin/plannotator update && ~/.local/bin/plannotator inbox restart\nWindows PowerShell: run plannotator update, then only if successful plannotator inbox restart.\n");
    } else {
        text.push_str("\nu starts/retries its update or restarts its installed version, after confirmation. An offline machine never queues this command. The machine's own state is authoritative; closing this pane does not stop the update.\n");
    }
    text
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests assert by panicking")]
mod tests {
    use super::*;

    #[test]
    fn qr_keeps_complete_quiet_zone_and_rectangular_rows() {
        let lines = qr_lines("plannotator://pair?secret=test").expect("QR");
        let width = lines.first().expect("first row").chars().count();
        assert!(lines.iter().all(|line| line.chars().count() == width));
        assert!(lines.iter().take(2).all(|line| line.trim().is_empty()));
        assert!(lines.iter().rev().take(2).all(|line| line.trim().is_empty()));
    }
}
