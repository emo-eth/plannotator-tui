use std::path::PathBuf;

use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};

pub(crate) const DEFAULT_SPLIT_RATIO: f32 = 0.35;
pub(crate) const MIN_THREADS_WIDTH: u16 = 28;
pub(crate) const MIN_DETAIL_WIDTH: u16 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InboxLayout {
    pub(crate) header: Rect,
    pub(crate) main: Rect,
    pub(crate) threads: Rect,
    pub(crate) detail: Rect,
    pub(crate) footer: Rect,
    pub(crate) divider_col: u16,
}

pub(crate) fn compute_layout(area: Rect, split_ratio: f32, reply_mode: bool) -> InboxLayout {
    let header_height = 1u16;
    let bottom_height = if reply_mode { 5u16 } else { 1u16 };
    let main_height = area.height.saturating_sub(header_height + bottom_height);

    let header = Rect::new(area.x, area.y, area.width, header_height);
    let main = Rect::new(area.x, area.y + header_height, area.width, main_height);
    let footer =
        Rect::new(area.x, area.y + area.height.saturating_sub(bottom_height), area.width, bottom_height);

    let total_width = main.width;
    let desired = (f64::from(total_width) * f64::from(split_ratio)).round() as u16;
    let min_threads = MIN_THREADS_WIDTH.min(total_width);
    let max_threads = total_width.saturating_sub(MIN_DETAIL_WIDTH).max(min_threads);
    let threads_width = desired.clamp(min_threads, max_threads);
    let detail_width = total_width.saturating_sub(threads_width);

    let threads = Rect::new(main.x, main.y, threads_width, main.height);
    let detail = Rect::new(main.x + threads_width, main.y, detail_width, main.height);
    let divider_col = threads.right();

    InboxLayout { header, main, threads, detail, footer, divider_col }
}

#[allow(dead_code)]
pub(crate) fn layout_panes(width: u16, height: u16, split_ratio: f32) -> (Rect, Rect) {
    let layout = compute_layout(Rect::new(0, 0, width, height), split_ratio, false);
    (layout.threads, layout.detail)
}

pub(crate) fn drag_to_ratio(column: u16, total_width: u16) -> f32 {
    if total_width == 0 {
        return DEFAULT_SPLIT_RATIO;
    }
    let min_threads = MIN_THREADS_WIDTH.min(total_width);
    let max_threads = total_width.saturating_sub(MIN_DETAIL_WIDTH).max(min_threads);
    let clamped = column.clamp(min_threads, max_threads);
    f32::from(clamped) / f32::from(total_width)
}

pub(crate) fn adjust_split_ratio(current_ratio: f32, delta_cols: i16, total_width: u16) -> f32 {
    if total_width == 0 {
        return DEFAULT_SPLIT_RATIO;
    }
    let current_cols = (f64::from(total_width) * f64::from(current_ratio)).round() as i32;
    let new_cols = current_cols + i32::from(delta_cols);
    let min_threads = i32::from(MIN_THREADS_WIDTH.min(total_width));
    let max_threads = i32::from(total_width.saturating_sub(MIN_DETAIL_WIDTH).max(min_threads as u16));
    let clamped = new_cols.clamp(min_threads, max_threads);
    clamped as f32 / f32::from(total_width)
}

pub(crate) fn inbox_config_path() -> PathBuf {
    if let Some(explicit) = std::env::var("PLANNOTATOR_TUI_INBOX_CONFIG").ok().filter(|s| !s.is_empty()) {
        return PathBuf::from(explicit);
    }
    #[cfg(windows)]
    if let Some(appdata) = std::env::var("APPDATA").ok().map(PathBuf::from).filter(|p| p.is_absolute()) {
        return appdata.join("plannotator-tui").join("inbox.toml");
    }
    let home = std::env::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let xdg = std::env::var("XDG_CONFIG_HOME").ok().map(PathBuf::from).filter(|p| p.is_absolute());
    xdg.unwrap_or_else(|| home.join(".config")).join("plannotator-tui").join("inbox.toml")
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct InboxConfigFile {
    #[serde(default)]
    split_ratio: Option<f32>,
    #[serde(default)]
    ui: Option<InboxConfigUi>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct InboxConfigUi {
    #[serde(default)]
    split_ratio: Option<f32>,
}

pub(crate) fn load_split_ratio() -> Option<f32> {
    let path = inbox_config_path();
    let content = std::fs::read_to_string(&path).ok()?;
    let config: InboxConfigFile = toml::from_str(&content).ok()?;
    config.ui.and_then(|u| u.split_ratio).or(config.split_ratio)
}

pub(crate) fn save_split_ratio(ratio: f32) {
    let path = inbox_config_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let toml_str = format!("[ui]\nsplit_ratio = {ratio:.4}\n");
    let _ = std::fs::write(&path, toml_str);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_layout_160_60() {
        let (threads, detail) = layout_panes(160, 60, DEFAULT_SPLIT_RATIO);
        assert_eq!(threads, Rect::new(0, 1, 56, 58));
        assert_eq!(detail, Rect::new(56, 1, 104, 58));
    }

    #[test]
    fn test_layout_80_24() {
        let (threads, detail) = layout_panes(80, 24, DEFAULT_SPLIT_RATIO);
        assert_eq!(threads, Rect::new(0, 1, 28, 22));
        assert_eq!(detail, Rect::new(28, 1, 52, 22));
    }

    #[test]
    fn test_divider_keys_move_within_min_max() {
        let width = 160u16;
        let mut ratio = DEFAULT_SPLIT_RATIO;
        assert_eq!((f64::from(width) * f64::from(ratio)).round() as u16, 56);

        // Move left by 2 columns
        ratio = adjust_split_ratio(ratio, -2, width);
        assert_eq!((f64::from(width) * f64::from(ratio)).round() as u16, 54);

        // Repeatedly move left; must clamp at min width = 28
        for _ in 0..50 {
            ratio = adjust_split_ratio(ratio, -2, width);
        }
        let min_cols = (f64::from(width) * f64::from(ratio)).round() as u16;
        assert_eq!(min_cols, 28);

        // Repeatedly move right; must clamp at max width = 160 - 20 = 140
        for _ in 0..100 {
            ratio = adjust_split_ratio(ratio, 2, width);
        }
        let max_cols = (f64::from(width) * f64::from(ratio)).round() as u16;
        assert_eq!(max_cols, 140);

        // Reset to default
        ratio = DEFAULT_SPLIT_RATIO;
        assert_eq!((f64::from(width) * f64::from(ratio)).round() as u16, 56);
    }

    #[test]
    fn test_drag_to_ratio_mapping() {
        let width = 160u16;

        // Normal range
        let ratio_mid = drag_to_ratio(80, width);
        assert!((ratio_mid - 0.5).abs() < 1e-4);

        let ratio_default = drag_to_ratio(56, width);
        assert!((ratio_default - 0.35).abs() < 1e-4);

        // Below minimum (min 28 cols)
        let ratio_clamped_low = drag_to_ratio(10, width);
        assert_eq!((f64::from(width) * f64::from(ratio_clamped_low)).round() as u16, 28);

        // Above maximum (max 140 cols = 160 - 20)
        let ratio_clamped_high = drag_to_ratio(155, width);
        assert_eq!((f64::from(width) * f64::from(ratio_clamped_high)).round() as u16, 140);
    }
}
