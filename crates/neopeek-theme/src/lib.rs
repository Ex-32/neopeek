//! neopeek-theme: RON theme model for the waterfall/HUD colors
//! (PLAN.md §6.1, §6.2). Pure data + parsing + resolution — no GPU work;
//! the wgpu plumbing lives in `neopeek-render`, which consumes
//! [`Theme`] each frame (M7).
//!
//! Colors are `#rrggbb[aa]` hex strings, deserialized to linear
//! `[f32; 4]` with 1/255 scaling — the same scale sndpeek used.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// A resolved color, `[r, g, b, a]` in 0…1.
pub type Color = [f32; 4];

/// Depth-dimming ramp stop for the filled-surface mode.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RampStop {
    pub pos: f32,
    #[serde(deserialize_with = "de_hex")]
    pub color: Color,
}

/// Waterfall color set (PLAN.md §6.1). `future` is parsed and carried for
/// the upcoming filled-surface mode; the M4 draw path uses past/now_line.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaterfallTheme {
    #[serde(deserialize_with = "de_hex")]
    pub past: Color,
    #[serde(deserialize_with = "de_hex")]
    pub future: Color,
    #[serde(deserialize_with = "de_hex")]
    pub now_line: Color,
    /// Default state of the `r` rainbow toggle at theme application.
    #[serde(default)]
    pub rainbow: bool,
    /// dB magnitude mode mapping window; the M4 shader formula
    /// `(20·log10(|X|/8) + 80) / 80` equals floor −80 / ceil 0.
    pub db_floor: f32,
    pub db_ceil: f32,
    /// Only rendered by the filled-surface mode (M8); empty = absent.
    #[serde(default)]
    pub surface_ramp: Vec<RampStop>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaveformTheme {
    #[serde(deserialize_with = "de_hex")]
    pub color: Color,
    /// Parsed now; actual stroke width lands with the M8 line-width work.
    pub width: f32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LissajousTheme {
    #[serde(deserialize_with = "de_hex")]
    pub color: Color,
    /// Figure scale; applied as the `liss_scale` base on theme application.
    pub scale: f32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetersTheme {
    /// Gradient at fill 0 and 1.
    #[serde(deserialize_with = "de_hex")]
    pub grad_low: Color,
    #[serde(deserialize_with = "de_hex")]
    pub grad_high: Color,
    pub peak_hold_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HudTheme {
    /// Body text color.
    #[serde(deserialize_with = "de_hex")]
    pub text: Color,
    /// Keybind-dump / secondary lines.
    #[serde(deserialize_with = "de_hex")]
    pub dim: Color,
    pub font_size: f32,
    /// Overlay text.
    #[serde(deserialize_with = "de_hex")]
    pub accent: Color,
}

/// A full theme (PLAN.md §6.1).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    pub name: String,
    #[serde(deserialize_with = "de_hex")]
    pub background: Color,
    pub waterfall: WaterfallTheme,
    pub waveform: WaveformTheme,
    pub lissajous: LissajousTheme,
    pub meters: MetersTheme,
    pub hud: HudTheme,
}

/// `#rgb` / `#rrggbb` / `#rrggbbaa` → `[f32; 4]`; missing alpha = opaque.
pub fn parse_hex(s: &str) -> Option<Color> {
    let digits = s.strip_prefix('#')?;
    if !digits.is_ascii() {
        return None;
    }
    let run = |d: &str| u8::from_str_radix(d, 16).ok().map(|v| v as f32 / 255.0);
    let rgb6 = |d: &str, a: f32| {
        Some([
            run(&d[0..2])?,
            run(&d[2..4])?,
            run(&d[4..6])?,
            a,
        ])
    };
    match digits.len() {
        3 => {
            // CSS-style short hex: each digit doubles (#abc = #aabbcc).
            let wide: String = digits.chars().flat_map(|c| [c, c]).collect();
            rgb6(&wide, 1.0)
        }
        6 => rgb6(digits, 1.0),
        8 => {
            Some([
                run(&digits[0..2])?,
                run(&digits[2..4])?,
                run(&digits[4..6])?,
                run(&digits[6..8])?,
            ])
        }
        _ => None,
    }
}

fn de_hex<'de, D>(d: D) -> Result<Color, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s: &str = Deserialize::deserialize(d)?;
    parse_hex(s).ok_or_else(|| serde::de::Error::custom(format!("bad hex color {s:?}")))
}

impl Theme {
    /// Parses embedded-RON / file content; the single parse path used by
    /// everything (built-ins included, PLAN.md §6.2).
    pub fn parse(ron: &str) -> Result<Self, String> {
        ron::from_str(ron).map_err(|err| format!("theme parse: {err}"))
    }

    /// RGB (opaque) view of a color, for shader uniforms.
    pub fn rgb(c: &Color) -> [f32; 3] {
        [c[0], c[1], c[2]]
    }

    /// Byte-triplet for cosmic-text.
    pub fn rgb8(c: &Color) -> (u8, u8, u8) {
        (
            (c[0] * 255.0).round() as u8,
            (c[1] * 255.0).round() as u8,
            (c[2] * 255.0).round() as u8,
        )
    }

    /// The filled-surface ramp stops, falling back to `past → future`
    /// when the theme defines none.
    pub fn surface_ramp_stops(&self) -> Vec<RampStop> {
        if !self.waterfall.surface_ramp.is_empty() {
            self.waterfall.surface_ramp.clone()
        } else {
            vec![
                RampStop { pos: 0.0, color: self.waterfall.past },
                RampStop { pos: 1.0, color: self.waterfall.future },
            ]
        }
    }

    /// Ready-to-upload RGBA8 ramp data (256 samples) for the surface
    /// pipeline (M8).
    pub fn ramp_data(&self) -> Vec<u8> {
        ramp_to_rgba8(&self.surface_ramp_stops(), 256)
    }
}

/// Builds the filled-surface 1D ramp as RGBA8 bytes (`width` samples),
/// interpolating stops by `pos` (sorted internally; ends clamped).
/// The render crate uploads this into a 1D-ish texture (M8).
pub fn ramp_to_rgba8(stops: &[RampStop], width: usize) -> Vec<u8> {
    assert!(width >= 2, "ramp needs at least 2 samples");
    let mut stops: Vec<RampStop> = stops.to_vec();
    stops.sort_by(|a, b| a.pos.partial_cmp(&b.pos).unwrap_or(std::cmp::Ordering::Equal));
    let mut out = Vec::with_capacity(width * 4);
    if stops.len() == 1 {
        // Constant ramp.
        let c: [u8; 4] = stops[0].color.map(|v| (v * 255.0).round() as u8);
        for _ in 0..width {
            out.extend_from_slice(&c);
        }
        return out;
    }
    for i in 0..width {
        let t = i as f32 / (width - 1) as f32;
        // Segment containing t.
        let seg = match stops.binary_search_by(|s| s.pos.partial_cmp(&t).unwrap()) {
            Ok(_) | Err(0) => 0, // exact hit or before first → first stop
            Err(e) => (e - 1).min(stops.len() - 2),
        };
        let (a, b) = (&stops[seg], &stops[seg + 1.min(stops.len() - 1)]);
        let span = (b.pos - a.pos).max(f32::EPSILON);
        let f = ((t - a.pos) / span).clamp(0.0, 1.0);
        for c in 0..4 {
            let v = a.color[c] + (b.color[c] - a.color[c]) * f;
            out.push((v * 255.0).round() as u8);
        }
    }
    out
}

pub const EMBER_RON: &str = include_str!("../assets/themes/Ember.ron");
pub const ICE_RON: &str = include_str!("../assets/themes/Ice.ron");
pub const TERMINAL_RON: &str = include_str!("../assets/themes/Terminal.ron");

pub const BUILTIN_NAMES: [&str; 3] = ["Ember", "Ice", "Terminal"];

/// Embedded RON for a built-in, case-insensitive.
pub fn builtin_ron(name: &str) -> Option<&'static str> {
    match name.to_ascii_lowercase().as_str() {
        "ember" => Some(EMBER_RON),
        "ice" => Some(ICE_RON),
        "terminal" => Some(TERMINAL_RON),
        _ => None,
    }
}

pub fn builtin(name: &str) -> Option<Theme> {
    builtin_ron(name).and_then(|ron| ron::from_str(ron).ok())
}

/// A resolved theme plus where it came from (the custom theme's path is
/// kept for the document watcher).
#[derive(Debug, Clone)]
pub struct LoadedTheme {
    pub theme: Theme,
    pub path: Option<PathBuf>,
}

impl LoadedTheme {
    pub fn builtin(name: &str) -> Self {
        let theme = builtin(name).expect("built-in RON must parse");
        Self { theme, path: None }
    }
}

impl Default for LoadedTheme {
    fn default() -> Self {
        Self::builtin("Ember")
    }
}

/// `$XDG_CONFIG_HOME/neopeek/themes` (directories crate).
pub fn user_theme_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.config_dir().join("neopeek").join("themes"))
}

/// Resolution per PLAN.md §6.2:
/// `--theme <name|path>` → existing path → `$XDG…/themes/<name>.ron` →
/// built-in name → warn + Ember fallback. Never fails.
///
/// Returns the resolved theme, the custom file path (if it was one) and a
/// warning string to log when a fallback happened.
pub fn resolve(arg: Option<&str>) -> (LoadedTheme, Option<String>) {
    match arg {
        None => (LoadedTheme::default(), None),
        Some(arg) => {
            let path = Path::new(arg);
            if path.is_file() {
                match load_file(path) {
                    Ok(theme) => return (LoadedTheme { theme, path: Some(path.into()) }, None),
                    Err(msg) => return fallback_msg(msg),
                }
            }
            if let Some(dir) = user_theme_dir() {
                if !arg.contains('/') && !arg.contains('\\') {
                    let cand = dir.join(format!("{arg}.ron"));
                    if cand.is_file() {
                        match load_file(&cand) {
                            Ok(theme) => {
                                return (LoadedTheme { theme, path: Some(cand) }, None)
                            }
                            Err(msg) => return fallback_msg(msg),
                        }
                    }
                }
            }
            if let Some(theme) = builtin(arg) {
                return (LoadedTheme { theme, path: None }, None);
            }
            fallback_msg(format!("no theme file, no built-in named {arg:?}"))
        }
    }
}

fn fallback_msg(msg: String) -> (LoadedTheme, Option<String>) {
    tracing::warn!("{msg}; falling back to default theme");
    (LoadedTheme::default(), Some(msg))
}

pub fn load_file(path: &Path) -> Result<Theme, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("theme read {}: {err}", path.display()))?;
    Theme::parse(&text).map_err(|msg| format!("theme {}: {msg}", path.display()))
}

pub mod cycler;
pub mod watch;

// The resolution + hex tests live in tests.rs (inline-module style).
#[cfg(test)]
mod tests;
