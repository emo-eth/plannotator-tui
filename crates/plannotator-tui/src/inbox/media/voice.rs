//! Explicit ffmpeg microphone capture and Inbox voice transcription.
//!
//! Recording starts only after the person chooses dictation. Audio never touches
//! disk: ffmpeg writes mono 24 kHz PCM WAV to a pipe, the bytes stay in memory,
//! and are sent via POST to this Inbox's `/api/inbox/voice/transcribe`. Esc discards
//! the take. No background recording.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use serde_json::Value;

use crate::inbox::client::Client;

const RECORD_LIMIT: Duration = Duration::from_secs(120);
const MAX_WAV_BYTES: usize = 6 * 1024 * 1024;
const SAMPLE_RATE: u32 = 24_000;

pub(super) fn transcribe(client: &Client, terminal: &mut DefaultTerminal) -> Result<String> {
    ensure_ffmpeg()?;
    let devices = list_input_devices()?;
    ensure!(!devices.is_empty(), "no microphone input device was found");
    let device = choose_device(terminal, &devices)?;
    let mut recording = Recording::start(&device)?;
    let started = Instant::now();
    let status = format!(
        "Recording from “{}”. Capture is local and in memory; nothing is written to disk.\nAudio is sent only to this Inbox after you stop.",
        device.label
    );
    loop {
        let elapsed = started.elapsed();
        terminal.draw(|frame| {
            let [title, body, footer] = Layout::vertical([
                Constraint::Length(2),
                Constraint::Min(1),
                Constraint::Length(2),
            ])
            .areas(frame.area());
            frame.render_widget(
                Paragraph::new(Line::from("Media · microphone dictation").style(Style::new().add_modifier(Modifier::BOLD))),
                title,
            );
            frame.render_widget(
                Paragraph::new(format!(
                    "{status}\n\nRecording: {} / 2:00\n\nEnter or Space stops and transcribes.\nEsc discards the take.",
                    clock(elapsed)
                ))
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL)),
                body,
            );
            frame.render_widget(
                Paragraph::new("No background recording · Esc discards · clip never leaves this Inbox"),
                footer,
            );
        })?;
        if elapsed >= RECORD_LIMIT {
            break;
        }
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key)
                    if key.kind == KeyEventKind::Press
                        && matches!(key.code, KeyCode::Enter | KeyCode::Char(' ')) =>
                {
                    break;
                }
                Event::Key(key) if key.kind == KeyEventKind::Press && key.code == KeyCode::Esc => {
                    recording.discard();
                    bail!("voice dictation cancelled");
                }
                _ => {}
            }
        }
        if let Some(error) = recording.poll_error()? {
            recording.discard();
            bail!(error);
        }
    }
    let elapsed = started.elapsed().min(RECORD_LIMIT);
    let wav = recording.finish()?;
    validate_wav(&wav)?;
    terminal.draw(|frame| {
        frame.render_widget(
            Paragraph::new(format!(
                "Recording stopped ({}). Sending the clip to this Inbox for transcription…\n\nThe clip remains only in memory and is never forwarded to a linked machine.",
                clock(elapsed)
            ))
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title("Voice transcription")),
            frame.area(),
        );
    })?;
    // prepare_file_write injects the live serverSession; do not forward audio to linked machines.
    let response = client.post_bytes("/api/inbox/voice/transcribe", "audio/wav", &wav)?;
    if let Some(text) =
        response.get("text").and_then(Value::as_str).map(str::trim).filter(|text| !text.is_empty())
    {
        return Ok(text.to_owned());
    }
    let code = response.get("code").and_then(Value::as_str).unwrap_or("unknown voice failure");
    let reason = response.get("reason").and_then(Value::as_str).unwrap_or("");
    if reason.is_empty() {
        bail!("Inbox did not transcribe this recording ({code})");
    }
    bail!("Inbox did not transcribe this recording ({code}; {reason})")
}

#[derive(Debug, Clone)]
struct InputDevice {
    /// Value passed to ffmpeg's `-i`.
    target: String,
    label: String,
}

fn ensure_ffmpeg() -> Result<()> {
    let output = Command::new("ffmpeg")
        .args(["-version"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("voice dictation needs ffmpeg on PATH; install ffmpeg and grant microphone access")?;
    ensure!(output.success(), "voice dictation needs a working ffmpeg on PATH");
    Ok(())
}

fn choose_device(terminal: &mut DefaultTerminal, devices: &[InputDevice]) -> Result<InputDevice> {
    if let [device] = devices {
        return Ok(device.clone());
    }
    let mut selected = 0usize;
    loop {
        terminal.draw(|frame| {
            let [title, body, footer] =
                Layout::vertical([Constraint::Length(2), Constraint::Min(1), Constraint::Length(2)])
                    .areas(frame.area());
            frame.render_widget(
                Paragraph::new(
                    Line::from("Media · choose microphone").style(Style::new().add_modifier(Modifier::BOLD)),
                ),
                title,
            );
            let items: Vec<ListItem<'_>> =
                devices.iter().map(|device| ListItem::new(device.label.as_str())).collect();
            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL).title("Input devices"))
                .highlight_symbol("› ")
                .highlight_style(Style::new().add_modifier(Modifier::BOLD));
            let mut state = ListState::default().with_selected(Some(selected));
            frame.render_stateful_widget(list, body, &mut state);
            frame.render_widget(Paragraph::new("↑/↓ choose · Enter start recording · Esc cancel"), footer);
        })?;
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => bail!("voice dictation cancelled"),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                selected = selected.saturating_add(1).min(devices.len().saturating_sub(1));
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                return devices.get(selected).cloned().context("selected microphone disappeared");
            }
            _ => {}
        }
    }
}

struct Recording {
    child: Child,
    reader: Option<thread::JoinHandle<Result<Vec<u8>>>>,
    errors: mpsc::Receiver<String>,
}

impl Recording {
    fn start(device: &InputDevice) -> Result<Self> {
        let mut command = Command::new("ffmpeg");
        command.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
        #[cfg(target_os = "macos")]
        command.args(["-f", "avfoundation", "-i", &device.target]);
        #[cfg(target_os = "linux")]
        command.args(["-f", "pulse", "-i", &device.target]);
        #[cfg(target_os = "windows")]
        command.args(["-f", "dshow", "-i", &device.target]);
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            let _ = device;
            bail!("voice recording is unsupported on this platform");
        }
        command.args(["-ac", "1", "-ar", "24000", "-c:a", "pcm_s16le", "-f", "wav", "pipe:1"]);
        command.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());
        let mut child = command
            .spawn()
            .context("starting ffmpeg microphone capture; install ffmpeg and grant microphone access")?;
        let stdout = child.stdout.take().context("ffmpeg omitted stdout pipe")?;
        let stderr = child.stderr.take().context("ffmpeg omitted stderr pipe")?;
        let (error_tx, errors) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut wav = Vec::new();
            let mut source = stdout.take((MAX_WAV_BYTES as u64).saturating_add(1));
            source.read_to_end(&mut wav).context("reading recorded WAV from ffmpeg")?;
            if wav.len() > MAX_WAV_BYTES {
                bail!("voice clip exceeds the Inbox 6 MiB limit");
            }
            Ok(wav)
        });
        thread::spawn(move || {
            let mut detail = String::new();
            let _ = stderr.take(8 * 1024).read_to_string(&mut detail);
            let detail = detail.trim();
            if !detail.is_empty() {
                let _ = error_tx.send(detail.to_owned());
            }
        });
        Ok(Self { child, reader: Some(reader), errors })
    }

    fn poll_error(&mut self) -> Result<Option<String>> {
        match self.errors.try_recv() {
            Ok(message) => Ok(Some(format!("ffmpeg microphone capture failed: {message}"))),
            Err(mpsc::TryRecvError::Empty) => {
                if let Some(status) = self.child.try_wait().context("checking ffmpeg microphone capture")?
                    && !status.success()
                {
                    return Ok(Some(format!("ffmpeg microphone capture exited early ({status})")));
                }
                Ok(None)
            }
            Err(mpsc::TryRecvError::Disconnected) => Ok(None),
        }
    }

    fn discard(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }

    fn finish(mut self) -> Result<Vec<u8>> {
        // Stop capture; the WAV header was written up front, so a short clip is still valid PCM.
        let _ = self.child.kill();
        let status = self.child.wait().context("waiting for ffmpeg microphone capture")?;
        let wav = self
            .reader
            .take()
            .context("ffmpeg capture reader missing")?
            .join()
            .map_err(|_| anyhow::anyhow!("ffmpeg capture reader panicked"))??;
        if let Ok(message) = self.errors.try_recv() {
            ensure!(wav.len() > 44, "ffmpeg did not capture microphone audio: {message}");
        }
        // kill often yields a non-zero status; accept a valid clip regardless.
        if wav.len() <= 44 && !status.success() {
            bail!("ffmpeg did not capture microphone audio ({status})");
        }
        Ok(wav)
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn validate_wav(bytes: &[u8]) -> Result<()> {
    ensure!(bytes.len() > 44, "microphone returned an empty WAV clip");
    ensure!(
        bytes.get(0..4) == Some(b"RIFF") && bytes.get(8..12) == Some(b"WAVE"),
        "ffmpeg returned an invalid WAV clip"
    );
    ensure!(
        bytes.get(20..22) == Some(&[1, 0]) && bytes.get(22..24) == Some(&[1, 0]),
        "voice clip must be mono PCM WAV"
    );
    ensure!(
        bytes.get(24..28) == Some(&SAMPLE_RATE.to_le_bytes()),
        "voice clip must use the Inbox 24 kHz sample rate"
    );
    ensure!(bytes.len() <= MAX_WAV_BYTES, "voice clip exceeds the Inbox 6 MiB limit");
    Ok(())
}

fn clock(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn list_input_devices() -> Result<Vec<InputDevice>> {
    #[cfg(target_os = "macos")]
    {
        list_avfoundation_devices()
    }
    #[cfg(target_os = "linux")]
    {
        list_pulse_devices()
    }
    #[cfg(target_os = "windows")]
    {
        list_dshow_devices()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        bail!("voice recording is unsupported on this platform");
    }
}

#[cfg(target_os = "macos")]
fn list_avfoundation_devices() -> Result<Vec<InputDevice>> {
    let output = Command::new("ffmpeg")
        .args(["-hide_banner", "-f", "avfoundation", "-list_devices", "true", "-i", ""])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("listing AVFoundation microphones with ffmpeg")?;
    let text = String::from_utf8_lossy(&output.stderr);
    let mut devices = Vec::new();
    let mut in_audio = false;
    for line in text.lines() {
        if line.contains("AVFoundation audio devices") {
            in_audio = true;
            continue;
        }
        if line.contains("AVFoundation video devices") {
            in_audio = false;
            continue;
        }
        if !in_audio {
            continue;
        }
        // [AVFoundation indev @ …] [0] Built-in Microphone
        let Some(after) = line.split_once("] [") else { continue };
        let rest = after.1;
        let Some((index, name)) = rest.split_once(']') else { continue };
        let index = index.trim();
        let name = name.trim();
        if index.chars().all(|c| c.is_ascii_digit()) && !name.is_empty() {
            devices.push(InputDevice { target: format!(":{index}"), label: format!("{index} · {name}") });
        }
    }
    if devices.is_empty() {
        // Fall back to the system default audio input when listing is empty.
        devices.push(InputDevice {
            target: ":0".to_owned(),
            label: "default audio input (AVFoundation :0)".to_owned(),
        });
    }
    Ok(devices)
}

#[cfg(target_os = "linux")]
fn list_pulse_devices() -> Result<Vec<InputDevice>> {
    // Prefer PulseAudio source names when pactl is present; otherwise default.
    if let Ok(output) = Command::new("pactl")
        .args(["list", "short", "sources"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        && output.status.success()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        let mut devices = Vec::new();
        for line in text.lines() {
            let mut parts = line.split_whitespace();
            let Some(_index) = parts.next() else { continue };
            let Some(name) = parts.next() else { continue };
            if name.ends_with(".monitor") {
                continue;
            }
            devices.push(InputDevice { target: name.to_owned(), label: name.to_owned() });
        }
        if !devices.is_empty() {
            return Ok(devices);
        }
    }
    Ok(vec![InputDevice { target: "default".to_owned(), label: "default PulseAudio source".to_owned() }])
}

#[cfg(target_os = "windows")]
fn list_dshow_devices() -> Result<Vec<InputDevice>> {
    let output = Command::new("ffmpeg")
        .args(["-hide_banner", "-list_devices", "true", "-f", "dshow", "-i", "dummy"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("listing DirectShow microphones with ffmpeg")?;
    let text = String::from_utf8_lossy(&output.stderr);
    let mut devices = Vec::new();
    let mut expect_audio = false;
    for line in text.lines() {
        if line.contains("(audio)") {
            expect_audio = true;
            // "None" DirectShow audio device "Microphone (Realtek)"
            if let Some(name) = dshow_quoted_name(line) {
                devices.push(InputDevice { target: format!("audio={name}"), label: name });
            }
            continue;
        }
        if line.contains("(video)") {
            expect_audio = false;
            continue;
        }
        if expect_audio && let Some(name) = dshow_quoted_name(line) {
            devices.push(InputDevice { target: format!("audio={name}"), label: name });
        }
    }
    ensure!(
        !devices.is_empty(),
        "no DirectShow audio capture device was found; grant microphone access or install a driver"
    );
    Ok(devices)
}

#[cfg(target_os = "windows")]
fn dshow_quoted_name(line: &str) -> Option<String> {
    let start = line.find('"')?;
    let rest = &line[start + 1..];
    let end = rest.find('"')?;
    let name = rest[..end].trim();
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, reason = "tests assert by panicking")]
#[allow(
    clippy::indexing_slicing,
    reason = "WAV fixtures use fixed offsets in a buffer sized for the complete header"
)]
mod tests {
    use super::{clock, validate_wav};
    use std::time::Duration;

    #[test]
    fn validates_only_inbox_wav_format() {
        let mut bytes = vec![0; 46];
        bytes[0..4].copy_from_slice(b"RIFF");
        bytes[8..12].copy_from_slice(b"WAVE");
        bytes[20..22].copy_from_slice(&[1, 0]);
        bytes[22..24].copy_from_slice(&[1, 0]);
        bytes[24..28].copy_from_slice(&24_000_u32.to_le_bytes());
        assert!(validate_wav(&bytes).is_ok());
        bytes[24] = 1;
        assert!(validate_wav(&bytes).is_err());
    }

    #[test]
    fn displays_recording_clock() {
        assert_eq!(clock(Duration::from_secs(125)), "2:05");
    }
}
