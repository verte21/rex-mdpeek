use std::env;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};

use ratatui::style::Color;

#[cfg(target_os = "macos")]
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct RexThemePalette {
    pub theme_id: String,
    pub is_dark: bool,
    pub bg_rgb: (u8, u8, u8),
    pub surface_hex: String,
    pub surface_alt_hex: String,
    pub fg_hex: String,
    pub muted_hex: String,
    pub border_hex: String,
    pub accent_hex: String,
    pub bg: Color,
    pub fg: Color,
    pub surface: Color,
    pub chrome: Color,
    pub border: Color,
    pub muted: Color,
    pub dim: Color,
    pub accent: Color,
    pub heading_primary: Color,
    pub heading_secondary: Color,
    pub heading_tertiary: Color,
    pub code_fg: Color,
    pub code_bg: Color,
    pub link: Color,
    pub selected_bg: Color,
    pub note_accent: Color,
    pub tip_accent: Color,
    pub important_accent: Color,
    pub warning_accent: Color,
    pub caution_accent: Color,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Deserialize)]
struct RexBundleTheme {
    id: String,
    appearance: String,
    source: RexBundleThemeSource,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Deserialize)]
struct RexBundleThemeSource {
    background: String,
    foreground: String,
    #[serde(default)]
    ansi: Vec<String>,
}

pub struct BackgroundThemeWatcher {
    receiver: Option<Receiver<RexThemePalette>>,
    shutdown: Arc<AtomicBool>,
}

impl BackgroundThemeWatcher {
    pub fn spawn(initial_theme_id: String) -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        if env::var_os("REX_MDPEEK_THEME").is_some() {
            return Self {
                receiver: None,
                shutdown,
            };
        }

        #[cfg(target_os = "macos")]
        {
            let (sender, receiver) = mpsc::channel();
            let shutdown_worker = Arc::clone(&shutdown);
            std::thread::spawn(move || {
                let mut current_id = initial_theme_id;
                let cached_themes = load_macos_rex_bundle_themes();
                loop {
                    if shutdown_worker.load(Ordering::Relaxed) {
                        break;
                    }
                    let next_palette = resolve_macos_rex_palette(&cached_themes);
                    if next_palette.theme_id != current_id {
                        current_id = next_palette.theme_id.clone();
                        if sender.send(next_palette).is_err() {
                            break;
                        }
                    }
                    for _ in 0..12 {
                        if shutdown_worker.load(Ordering::Relaxed) {
                            return;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(250));
                    }
                }
            });
            Self {
                receiver: Some(receiver),
                shutdown,
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = initial_theme_id;
            let _ = mpsc::channel::<RexThemePalette>;
            Self {
                receiver: None,
                shutdown,
            }
        }
    }

    pub fn try_recv_updated_palette(&self) -> Option<RexThemePalette> {
        let rx = self.receiver.as_ref()?;
        let mut latest = None;
        while let Ok(palette) = rx.try_recv() {
            latest = Some(palette);
        }
        latest
    }
}

impl Drop for BackgroundThemeWatcher {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

pub fn initial_rex_palette() -> RexThemePalette {
    if let Ok(explicit) = env::var("REX_MDPEEK_THEME") {
        let normalized = explicit.trim().to_ascii_lowercase();
        if normalized == "light" || normalized.contains("light") {
            return default_light_palette();
        }
        if normalized == "dark" || normalized.contains("dark") {
            return default_dark_palette();
        }
    }

    if let Ok(colorfgbg) = env::var("COLORFGBG")
        && let Some(bg_code) = colorfgbg.rsplit(';').next()
        && matches!(bg_code.trim(), "7" | "15")
    {
        return default_light_palette();
    }

    default_dark_palette()
}

pub fn default_dark_palette() -> RexThemePalette {
    build_palette(
        "tokyo-night-storm",
        true,
        (26, 27, 38),
        (192, 202, 245),
        (247, 118, 142),
        (158, 206, 106),
        (224, 175, 104),
        (122, 162, 247),
        (187, 154, 247),
        (125, 207, 255),
    )
}

pub fn default_light_palette() -> RexThemePalette {
    build_palette(
        "flexoki-light",
        false,
        (255, 252, 240),
        (16, 15, 15),
        (175, 48, 41),
        (102, 128, 11),
        (173, 131, 1),
        (32, 94, 166),
        (94, 64, 157),
        (36, 131, 123),
    )
}

pub fn parse_hex_rgb(hex: &str) -> Option<(u8, u8, u8)> {
    let raw = hex.trim().strip_prefix('#')?;
    match raw.len() {
        3 | 4 => {
            let r = u8::from_str_radix(&raw[0..1].repeat(2), 16).ok()?;
            let g = u8::from_str_radix(&raw[1..2].repeat(2), 16).ok()?;
            let b = u8::from_str_radix(&raw[2..3].repeat(2), 16).ok()?;
            Some((r, g, b))
        }
        6 | 8 => {
            let r = u8::from_str_radix(&raw[0..2], 16).ok()?;
            let g = u8::from_str_radix(&raw[2..4], 16).ok()?;
            let b = u8::from_str_radix(&raw[4..6], 16).ok()?;
            Some((r, g, b))
        }
        _ => None,
    }
}

pub fn format_rgb_hex((r, g, b): (u8, u8, u8)) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

pub fn blend_rgb(base: (u8, u8, u8), tint: (u8, u8, u8), weight: f32) -> (u8, u8, u8) {
    let w = weight.clamp(0.0, 1.0);
    let r = ((base.0 as f32) * (1.0 - w) + (tint.0 as f32) * w).round() as u8;
    let g = ((base.1 as f32) * (1.0 - w) + (tint.1 as f32) * w).round() as u8;
    let b = ((base.2 as f32) * (1.0 - w) + (tint.2 as f32) * w).round() as u8;
    (r, g, b)
}

fn to_color((r, g, b): (u8, u8, u8)) -> Color {
    Color::Rgb(r, g, b)
}

#[cfg(target_os = "macos")]
fn read_macos_default(domain: &str, key: &str) -> Option<String> {
    let output = std::process::Command::new("defaults")
        .args(["read", domain, key])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

#[cfg(target_os = "macos")]
fn detect_macos_dark_mode() -> bool {
    let output = std::process::Command::new("defaults")
        .args(["read", "-g", "AppleInterfaceStyle"])
        .output()
        .ok();
    output
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .eq_ignore_ascii_case("dark")
        })
        .unwrap_or(true)
}

#[cfg(target_os = "macos")]
fn load_macos_rex_bundle_themes() -> Vec<RexBundleTheme> {
    let candidate_paths = [
        "/Applications/Rex.app/Contents/Resources/themes.json",
        "/Applications/Rex Beta.app/Contents/Resources/themes.json",
    ];
    for candidate in candidate_paths {
        if std::path::Path::new(candidate).is_file()
            && let Ok(bytes) = std::fs::read(candidate)
            && let Ok(themes) = serde_json::from_slice::<Vec<RexBundleTheme>>(&bytes)
        {
            return themes;
        }
    }
    Vec::new()
}

#[cfg(target_os = "macos")]
fn resolve_macos_rex_palette(themes: &[RexBundleTheme]) -> RexThemePalette {
    let appearance = read_macos_default("com.superlogical.rex", "Workspace.appearance")
        .unwrap_or_else(|| "system".to_string());
    let is_dark = match appearance.to_ascii_lowercase().as_str() {
        "light" => false,
        "dark" => true,
        _ => detect_macos_dark_mode(),
    };
    let theme_key = if is_dark {
        "Workspace.darkThemeID"
    } else {
        "Workspace.lightThemeID"
    };
    let preferred_id = read_macos_default("com.superlogical.rex", theme_key);

    let matched = preferred_id
        .as_deref()
        .and_then(|id| themes.iter().find(|t| t.id == id))
        .or_else(|| {
            let expected = if is_dark { "dark" } else { "light" };
            themes
                .iter()
                .find(|t| t.appearance.eq_ignore_ascii_case(expected))
        });

    if let Some(theme) = matched
        && let (Some(bg), Some(fg)) = (
            parse_hex_rgb(&theme.source.background),
            parse_hex_rgb(&theme.source.foreground),
        )
    {
        let ansi_rgb = |idx: usize, fallback: (u8, u8, u8)| -> (u8, u8, u8) {
            theme
                .source
                .ansi
                .get(idx)
                .and_then(|s| parse_hex_rgb(s))
                .unwrap_or(fallback)
        };
        let red = ansi_rgb(1, if is_dark { (247, 118, 142) } else { (175, 48, 41) });
        let green = ansi_rgb(2, if is_dark { (158, 206, 106) } else { (102, 128, 11) });
        let yellow = ansi_rgb(3, if is_dark { (224, 175, 104) } else { (173, 131, 1) });
        let blue = ansi_rgb(4, if is_dark { (122, 162, 247) } else { (32, 94, 166) });
        let magenta = ansi_rgb(5, if is_dark { (187, 154, 247) } else { (94, 64, 157) });
        let cyan = ansi_rgb(6, if is_dark { (125, 207, 255) } else { (36, 131, 123) });

        return build_palette(
            &theme.id, is_dark, bg, fg, red, green, yellow, blue, magenta, cyan,
        );
    }

    if is_dark {
        default_dark_palette()
    } else {
        default_light_palette()
    }
}

#[allow(clippy::too_many_arguments)]
fn build_palette(
    theme_id: &str,
    is_dark: bool,
    bg: (u8, u8, u8),
    fg: (u8, u8, u8),
    red: (u8, u8, u8),
    green: (u8, u8, u8),
    yellow: (u8, u8, u8),
    blue: (u8, u8, u8),
    magenta: (u8, u8, u8),
    cyan: (u8, u8, u8),
) -> RexThemePalette {
    let surface = blend_rgb(bg, fg, if is_dark { 0.06 } else { 0.05 });
    let raised = blend_rgb(bg, fg, if is_dark { 0.11 } else { 0.09 });
    let chrome = blend_rgb(bg, fg, if is_dark { 0.04 } else { 0.03 });
    let border = blend_rgb(bg, fg, if is_dark { 0.20 } else { 0.22 });
    let muted = blend_rgb(bg, fg, if is_dark { 0.68 } else { 0.62 });
    let dim = blend_rgb(bg, fg, if is_dark { 0.44 } else { 0.42 });
    let code_bg = blend_rgb(bg, blue, if is_dark { 0.08 } else { 0.06 });
    let selected_bg = blend_rgb(bg, blue, if is_dark { 0.22 } else { 0.14 });

    RexThemePalette {
        theme_id: theme_id.to_string(),
        is_dark,
        bg_rgb: bg,
        surface_hex: format_rgb_hex(surface),
        surface_alt_hex: format_rgb_hex(raised),
        fg_hex: format_rgb_hex(fg),
        muted_hex: format_rgb_hex(muted),
        border_hex: format_rgb_hex(border),
        accent_hex: format_rgb_hex(blue),
        bg: to_color(bg),
        fg: to_color(fg),
        surface: to_color(surface),
        chrome: to_color(chrome),
        border: to_color(border),
        muted: to_color(muted),
        dim: to_color(dim),
        accent: to_color(blue),
        heading_primary: to_color(blue),
        heading_secondary: to_color(cyan),
        heading_tertiary: to_color(magenta),
        code_fg: to_color(yellow),
        code_bg: to_color(code_bg),
        link: to_color(cyan),
        selected_bg: to_color(selected_bg),
        note_accent: to_color(blue),
        tip_accent: to_color(green),
        important_accent: to_color(magenta),
        warning_accent: to_color(yellow),
        caution_accent: to_color(red),
    }
}
