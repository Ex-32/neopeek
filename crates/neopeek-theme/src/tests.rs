//! Theme parsing/resolution tests (headless; PLAN.md §6).

use std::path::PathBuf;

use super::*;

const M1_BG: Color = [0.043, 0.043, 0.063, 1.0];
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
fn ember_defaults_match_original_hardcoded_values() {
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
    // M6 HUD text (207, 211, 220) and hold.
    assert_eq!(Theme::rgb8(&t.hud.text), (207, 211, 220));
    assert_eq!(t.meters.peak_hold_ms, 800);
    // M6 gradient endpoints.
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
