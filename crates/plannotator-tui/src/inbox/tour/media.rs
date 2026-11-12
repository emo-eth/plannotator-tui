use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, ensure};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use super::super::client::Client;
use super::fixture::asset_path;

const WIDTH: usize = 160;
const HEIGHT: usize = 90;
const FRAME_BYTES: usize = WIDTH * HEIGHT * 3;
pub(super) const FPS: f64 = 5.0;

const MAX_DOWNLOAD: usize = 8 * 1024 * 1024;
const MAX_PIXELS: usize = FRAME_BYTES * 300;
const MAX_CACHE: usize = 32 * 1024 * 1024;
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Circle {
    pub x: f64,
    pub y: f64,
    pub radius: f64,
}

impl Default for Circle {
    fn default() -> Self {
        Self { x: 0.5, y: 0.5, radius: 0.15 }
    }
}

pub(super) struct Pixels {
    data: Vec<u8>,
}

impl Pixels {
    pub(super) fn frames(&self) -> usize {
        self.data.len() / FRAME_BYTES
    }

    fn color(&self, frame: usize, x: usize, y: usize) -> Color {
        let offset = frame.min(self.frames().saturating_sub(1)) * FRAME_BYTES + (y * WIDTH + x) * 3;
        match self.data.get(offset..offset + 3) {
            Some([red, green, blue]) => Color::Rgb(*red, *green, *blue),
            _ => Color::Black,
        }
    }

    pub(super) fn draw(&self, frame: &mut Frame<'_>, area: Rect, moment: f64, circle: Option<Circle>) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let number = (moment.max(0.0) * FPS).round() as usize;
        let width = usize::from(area.width).min(WIDTH);
        let height = usize::from(area.height).min((width * HEIGHT / WIDTH).div_ceil(2)).max(1);
        let mut lines = Vec::with_capacity(height);
        for y in 0..height {
            let mut spans = Vec::with_capacity(width);
            for x in 0..width {
                let sample = |row: usize| {
                    let nx = x as f64 / width as f64;
                    let ny = row as f64 / (height * 2) as f64;
                    if circle.is_some_and(|circle| {
                        let distance = ((nx - circle.x).powi(2)
                            + ((ny - circle.y) * HEIGHT as f64 / WIDTH as f64).powi(2))
                        .sqrt();
                        (distance - circle.radius).abs() < 0.012
                    }) {
                        Color::Red
                    } else {
                        self.color(number, x * WIDTH / width, row * HEIGHT / (height * 2))
                    }
                };
                spans.push(Span::styled("▀", Style::default().fg(sample(y * 2)).bg(sample(y * 2 + 1))));
            }
            lines.push(Line::from(spans));
        }
        frame.render_widget(Paragraph::new(lines), area);
    }
}

#[derive(Default)]
pub(super) struct Assets {
    pub pictures: BTreeMap<String, Pixels>,
    pub texts: BTreeMap<String, String>,
}

impl Assets {
    pub(super) fn text(&mut self, client: &Client, name: &str) -> Result<()> {
        if !self.texts.contains_key(name) {
            let text = String::from_utf8(download(client, name)?).context("Tour text is not UTF-8")?;
            self.texts.insert(name.to_owned(), text);
        }
        Ok(())
    }

    pub(super) fn pixels(&mut self, client: &Client, name: &str) -> Result<()> {
        if self.pictures.contains_key(name) {
            return Ok(());
        }
        let extension = name.rsplit_once('.').map(|(_, extension)| extension);
        ensure!(matches!(extension, Some("webp" | "mp4")), "Tour asset is not an image or video");
        let bytes = download(client, name)?;
        let scratch = Scratch::new(name, &bytes)?;
        let film = extension == Some("mp4");
        let filter = format!(
            "{}scale={WIDTH}:{HEIGHT}:force_original_aspect_ratio=decrease,pad={WIDTH}:{HEIGHT}:(ow-iw)/2:(oh-ih)/2",
            if film { "fps=5," } else { "" }
        );
        let mut child = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-max_alloc", "67108864", "-threads", "1", "-protocol_whitelist", "file", "-i"])
            .arg(&scratch.file)
            .args(["-an", "-vf", &filter, "-frames:v", if film { "300" } else { "1" }, "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"])
            .stdin(Stdio::null()).stderr(Stdio::piped()).stdout(Stdio::piped()).spawn()
            .context("Tour image/video playback needs ffmpeg on PATH. Install it, retry with r, or explicitly skip this interaction with f")?;
        let stdout = child.stdout.take().context("Tour decoder did not open stdout")?;
        let stderr = child.stderr.take().context("Tour decoder did not open stderr")?;
        let reader = std::thread::spawn(move || bounded_read(stdout, MAX_PIXELS));
        let errors = std::thread::spawn(move || bounded_read(stderr, 65536));
        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {}
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error).context("Waiting for tour media decoder");
                }
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let data = reader.join().map_err(|_| anyhow::anyhow!("Tour decoder reader stopped"))??;
        let errors = errors.join().map_err(|_| anyhow::anyhow!("Tour decoder error reader stopped"))??;
        let status =
            status.context("Tour media decoding exceeded 15 seconds; r retries, f explicitly skips")?;
        ensure!(status.success(), "Decoding tour media failed: {}", String::from_utf8_lossy(&errors));
        ensure!(
            !data.is_empty() && data.len() % FRAME_BYTES == 0,
            "Tour media decoder returned incomplete pixels"
        );
        let used: usize = self.pictures.values().map(|pixels| pixels.data.len()).sum();
        ensure!(used + data.len() <= MAX_CACHE, "Tour media cache exceeds 32 MiB");
        self.pictures.insert(name.to_owned(), Pixels { data });
        Ok(())
    }
}

fn bounded_read(reader: impl Read, limit: usize) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    reader.take(limit as u64 + 1).read_to_end(&mut data)?;
    ensure!(data.len() <= limit, "Tour asset exceeded its {limit}-byte limit");
    Ok(data)
}

fn download(client: &Client, name: &str) -> Result<Vec<u8>> {
    read_route(client, &asset_path(name)?, MAX_DOWNLOAD)
}

pub(super) fn read_route(client: &Client, path: &str, limit: usize) -> Result<Vec<u8>> {
    let base = url::Url::parse(client.url())?;
    let url = base.join(path)?;
    let agent = ureq::AgentBuilder::new().redirects(0).timeout(Duration::from_secs(15)).build();
    let response = agent
        .get(url.as_str())
        .set("Origin", &base.origin().ascii_serialization())
        .set("Sec-Fetch-Site", "same-origin")
        .call()
        .context("Reading tour asset")?;
    bounded_read(response.into_reader(), limit)
}

struct Scratch {
    dir: PathBuf,
    file: PathBuf,
}

impl Scratch {
    fn new(name: &str, bytes: &[u8]) -> Result<Self> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let dir = std::env::temp_dir().join(format!("plannotator-tour-{}-{stamp}", std::process::id()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&dir).context("Creating temporary tour media directory")?;
        let scratch = Self { file: dir.join(name), dir };
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&scratch.file)
            .context("Creating temporary tour media")?
            .write_all(bytes)
            .context("Writing temporary tour media")?;
        Ok(scratch)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
