//! Theme parsing/resolution tests (headless; PLAN.md §6).

use std::path::PathBuf;

use super::*;

/// New Ember defaults are hand-tuned for contrast (not historical parity
/// with the M1–M6 sndpeek palette); pinned here to catch accidental drift.
const M1_BG: Color = [5.0 / 255.0, 5.0 / 255.0, 8.0 / 255.0, 1.0];
const M4_PAST: Color = [0.4, 1.0, 0.4, 1.0];
const M4_NOW: Color = [0.4, 1.0, 1.0, 1.0];
const M5_WAVE: Color = [0.4, 0.4, 1.0, 1.0];
const M5_LISS: Color = [1.0, 1.0, 0.5, 1.0];

fn hex_close(a: Color, b: Color) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.004)
}

#[test]
fn hex_deserializer() {
    // 6-digit opaque.
    assert_eq!(parse_hex("#66ff66"), Some([0x66 as f32 / 255.0, 1.0, 0x66 as f32 / 255.0, 1.0]));
    // 8-digit alpha.
    assert_eq!(parse_hex("#abcdef80").unwrap()[3], 0x80 as f32 / 255.0);
    // 3-digit expands by digit doubling.
    assert_eq!(parse_hex("#abc"), Some([0xaa as f32 / 255.0, 0xbb as f32 / 255.0, 0xcc as f32 / 255.0, 1.0]));
    // rejects.
    assert_eq!(parse_hex("#ff"), None);
    assert_eq!(parse_hex("#zzzzzz"), None);
    assert_eq!(parse_hex("66ff66"), None);
    assert_eq!(parse_hex(""), None);
    assert_eq!(parse_hex("#ff000000000"), None);
}

#[test]
fn invalid_ron_rejected() {
    // Bad hex (1-digit background).
    let err = Theme::parse(
        r##"(name: "x", background: "#1", waterfall: (past: "#fff", future: "#fff", now_line: "#fff", db_floor: -80.0, db_ceil: 0.0), waveform: (color: "#fff", width: 1.0), lissajous: (color: "#fff", scale: 1.0), meters: (grad_low: "#fff", grad_high: "#fff", peak_hold_ms: 1), hud: (text: "#fff", dim: "#fff", font_size: 14.0, accent: "#fff"))"##,
    );
    assert!(err.is_err(), "1-digit hex must be rejected");
    // Missing field.
    let err = Theme::parse(
        r##"(name: "x", background: "#0b0b10", waveform: (color: "#fff", width: 1.0), lissajous: (color: "#fff", scale: 1.0), meters: (grad_low: "#fff", grad_high: "#fff", peak_hold_ms: 1), hud: (text: "#fff", dim: "#fff", font_size: 14.0, accent: "#fff"))"##,
    );
    assert!(err.is_err(), "missing waterfall section must be rejected");
    // Random junk.
    assert!(Theme::parse("not ron at all").is_err());
}

#[test]
fn surface_ramp_optional_and_typed() {
    let t = Theme::parse(EMBER_RON).unwrap();
    assert!(t.waterfall.surface_ramp.is_empty());
    // With a ramp.
    let ron = EMBER_RON.replace(
        "db_ceil: 0.0,",
        concat!(
            "db_ceil: 0.0,\n        surface_ramp: [",
            "(pos: 0.0, color: \"#05070f\"),",
            "(pos: 1.0, color: \"#ffe3a0\")",
            "],"
        ),
    );
    let t = Theme::parse(&ron).unwrap();
    assert_eq!(t.waterfall.surface_ramp.len(), 2);
    assert_eq!(t.waterfall.surface_ramp[0].pos, 0.0);
}

#[test]
fn tty_defaults_pin_vga_palette_and_heat_ramp() {
    let t = Theme::parse(TTY_RON).unwrap();
    assert_eq!(t.name, "TTY");
    // Pure-black background.
    assert!(hex_close(t.background, [0.0, 0.0, 0.0, 1.0]));
    // Waterfall heritage: past bright green, now bright yellow, future
    // bright red (VGA colors).
    assert!(hex_close(t.waterfall.past, [0x55 as f32 / 255.0, 1.0, 0x55 as f32 / 255.0, 1.0]), "past");
    assert!(hex_close(t.waterfall.now_line, [1.0, 1.0, 0x55 as f32 / 255.0, 1.0]), "now_line");
    assert!(hex_close(t.waterfall.future, [1.0, 0x55 as f32 / 255.0, 0x55 as f32 / 255.0, 1.0]), "future");
    assert_eq!(t.waterfall.db_floor, -80.0);
    assert_eq!(t.waterfall.db_ceil, 0.0);
    // HUD: white text, grey dim, cyan accent.
    assert!(hex_close(t.hud.text, [1.0, 1.0, 1.0, 1.0]), "text");
    assert!(hex_close(t.hud.dim, [0xAA as f32 / 255.0, 0xAA as f32 / 255.0, 0xAA as f32 / 255.0, 1.0]), "dim");
    assert!(hex_close(t.hud.accent, [0x55 as f32 / 255.0, 1.0, 1.0, 1.0]), "accent");
    assert_eq!(t.hud.font_size, 14.0);
    // Filled-surface heat ramp built from the TTY palette, ends black→white.
    let ramp = &t.waterfall.surface_ramp;
    assert_eq!(ramp.len(), 5);
    assert!(hex_close(ramp[0].color, [0.0, 0.0, 0.0, 1.0]), "ramp black start");
    assert!(hex_close(ramp[4].color, [1.0, 1.0, 1.0, 1.0]), "ramp white end");
    assert!(hex_close(ramp[1].color, [0xAA as f32 / 255.0, 0.0, 0.0, 1.0]), "ramp dark red");
    // Bright-red and yellow stops land mid-ramp.
    assert!(hex_close(ramp[2].color, [1.0, 0x55 as f32 / 255.0, 0x55 as f32 / 255.0, 1.0]), "ramp bright red");
    assert!(hex_close(ramp[3].color, [1.0, 1.0, 0x55 as f32 / 255.0, 1.0]), "ramp yellow");
}

#[test]
fn ember_defaults_hand_tuned_regression_pin() {
    let t = Theme::parse(EMBER_RON).unwrap();
    assert_eq!(t.name, "Ember");
    assert!(hex_close(t.background, M1_BG), "{:?} vs {:?}", t.background, M1_BG);
    assert!(hex_close(t.waterfall.past, M4_PAST), "past");
    assert!(hex_close(t.waterfall.now_line, M4_NOW), "now");
    assert!(hex_close(t.waveform.color, M5_WAVE));
    assert!(hex_close(t.lissajous.color, M5_LISS));
    // M4 shader dB formula range.
    assert_eq!(t.waterfall.db_floor, -80.0);
    assert_eq!(t.waterfall.db_ceil, 0.0);
    // M4/M5 scales.
    assert_eq!(t.lissajous.scale, 0.45);
    // Hand-tuned HUD text (brightened for contrast) and hold.
    assert_eq!(Theme::rgb8(&t.hud.text), (230, 233, 239));
    assert_eq!(Theme::rgb8(&t.hud.dim), (138, 147, 165));
    assert_eq!(t.meters.peak_hold_ms, 800);
    // Gradient endpoints (unchanged green→red family).
    assert!(hex_close(t.meters.grad_low, [0.35, 1.0, 0.3, 1.0]), "grad_low");
    assert!(hex_close(t.meters.grad_high, [1.0, 0.3, 0.3, 1.0]), "grad_high");
}

#[test]
fn all_builtins_parse_with_distinct_names() {
    for (i, name) in BUILTIN_NAMES.iter().enumerate() {
        let t = builtin(name).expect("built-in must parse");
        assert_eq!(&t.name, name);
        for other in BUILTIN_NAMES.iter().skip(i + 1) {
            assert_ne!(&t.name, other);
        }
    }
    assert!(builtin_ron("EMBER").is_some(), "case-insensitive");
    assert!(builtin("nonexistent").is_none());
}

fn tmp_ron(content: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "neopeek-theme-test-{}-{}.ron",
        std::process::id(),
        content.len()
    ));
    std::fs::write(&p, content).unwrap();
    p
}

#[test]
fn ramp_interpolates_and_sorts() {
    use crate::{parse_hex, ramp_to_rgba8, RampStop};
    let stops = |v: &[(f32, &str)]| -> Vec<RampStop> {
        v.iter()
            .map(|&(pos, c)| RampStop { pos, color: parse_hex(c).unwrap() })
            .collect()
    };
    // Two stops: black → white; the middle sample is gray.
    let ramp = ramp_to_rgba8(&stops(&[(0.0, "#000000"), (1.0, "#ffffff")]), 3);
    assert_eq!(&ramp[0..4], &[0, 0, 0, 255]);
    assert_eq!(&ramp[4..8], &[128, 128, 128, 255]);
    assert_eq!(&ramp[8..12], &[255, 255, 255, 255]);
    // Unsorted input sorts; clamps beyond the ends.
    let ramp = ramp_to_rgba8(&stops(&[(1.0, "#ff0000"), (0.0, "#00ff00")]), 2);
    assert_eq!(&ramp[0..4], &[0, 255, 0, 255]);
    assert_eq!(&ramp[4..8], &[255, 0, 0, 255]);
    // Single stop: constant ramp.
    let ramp = ramp_to_rgba8(&stops(&[(0.5, "#123456")]), 4);
    assert!(ramp.chunks(4).all(|c| c == [0x12, 0x34, 0x56, 255]));
}

#[test]
fn resolve_missing_and_malformed_fall_back_to_ember() {
    // No arg → Ember, no warning.
    let (l, warn) = resolve(None);
    assert_eq!(l.theme.name, "Ember");
    assert_eq!(warn, None);
    // Builtin name (any case) → builtin.
    let (l, warn) = resolve(Some("ICE"));
    assert_eq!(l.theme.name, "Ice");
    assert_eq!(warn, None);
    // Unknown name → resolved default + warning.
    let (l, warn) = resolve(Some("nope"));
    assert_eq!(l.theme.name, "Ember");
    assert!(warn.is_some());
    // Malformed file → fallback with warning.
    let bad = tmp_ron("this is not ron");
    let (l, warn) = resolve(Some(bad.to_str().unwrap()));
    assert_eq!(l.theme.name, "Ember");
    assert!(warn.is_some());
    std::fs::remove_file(bad).ok();
}

#[test]
fn resolve_file_path_loads_without_warn() {
    // A valid theme file with a distinct name.
    let ron = EMBER_RON.replace("name: \"Ember\"", "name: \"Custom\"");
    let p = tmp_ron(&ron);
    let (l, warn) = resolve(Some(p.to_str().unwrap()));
    assert_eq!(warn, None);
    assert_eq!(l.theme.name, "Custom");
    assert_eq!(l.path.as_deref(), Some(p.as_path()));
    std::fs::remove_file(p).ok();
}
