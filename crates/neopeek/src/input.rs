//! Input mapping (PLAN.md §4.3): keys + mouse handling stay in the
//! binary; it owns the [`ViewSettings`] and the shell [`Camera`].

use neopeek_render::camera::{Camera, SnapView};
use neopeek_render::views::ViewSettings;

/// One key-press outcome: quit flag, plus sndpeek-parity feedback — an
/// optional one-line description of the setting the key just changed
/// (`None` when nothing user-visible changed; the caller logs it at
/// INFO, PLAN.md §4.3). Descriptions use the `p` dump's field names so
/// the console language stays consistent.
pub type KeyPress = (bool, Option<String>);

fn on_off(on: bool) -> &'static str {
    if on {
        "on"
    } else {
        "off"
    }
}

/// Applies one logical character key press. Named keys (`PageUp`/`PageDown`)
/// arrive through the same interface; `app.rs` routes them by string.
pub fn handle_key(settings: &mut ViewSettings, key: &str) -> KeyPress {
    match key {
        // View toggles
        "1" => {
            settings.toggle_waveform();
            (false, Some(format!("waveform = {}", on_off(settings.waveform_on))))
        }
        "2" => {
            settings.toggle_lissajous();
            (false, Some(format!("lissajous = {}", on_off(settings.lissajous_on))))
        }
        "3" | "w" => {
            settings.toggle_waterfall();
            (false, Some(format!("waterfall = {}", on_off(settings.waterfall_on))))
        }
        // Magnitude / color modes
        "d" => {
            settings.toggle_db();
            (false, Some(format!("magnitude = {}", if settings.db { "dB" } else { "linear" })))
        }
        "S" => {
            settings.toggle_surface();
            (false, Some(format!("surface = {}", on_off(settings.surface))))
        }
        "r" => {
            settings.toggle_rainbow();
            (false, Some(format!("rainbow = {}", on_off(settings.rainbow))))
        }
        // Auto-scale waterfall height to the content (a)
        "a" => {
            settings.toggle_auto_scale();
            (false, Some(format!("auto_scale = {}", on_off(settings.auto_scale))))
        }
        // Width of the waveform/Lissajous quads (< / >)
        "<" => {
            settings.scale_strip_width(false);
            (false, Some(format!("strip_width = {:.4}", settings.strip_width)))
        }
        ">" => {
            settings.scale_strip_width(true);
            (false, Some(format!("strip_width = {:.4}", settings.strip_width)))
        }
        // Pseudo-Lissajous delay (y; Y is the snap-side view)
        "y" => {
            settings.cycle_liss_delay();
            (false, Some(format!("liss_delay = {}", settings.liss_delay)))
        }
        // Freq-axis log factor (c/v, sndpeek `g_log_factor` scaling)
        "c" => {
            settings.adjust_log_factor(true);
            (false, Some(format!("log_factor = {:.3}", settings.log_factor)))
        }
        "v" => {
            settings.adjust_log_factor(false);
            (false, Some(format!("log_factor = {:.3}", settings.log_factor)))
        }
        // Waterfall depth (PageUp/PageDown arrive as named keys via app.rs)
        "PageUp" => {
            settings.adjust_depth(true);
            (false, Some(format!("depth = {} layers", settings.depth)))
        }
        "PageDown" => {
            settings.adjust_depth(false);
            (false, Some(format!("depth = {} layers", settings.depth)))
        }
        // FFT size ladder (o/n): up = more (interpolated) bins. The
        // renderer grid rebuild + DSP plan swap are synced by app.rs.
        "o" => {
            settings.adjust_fft_size(true);
            (false, Some(format!("fft_size = {}", settings.fft_size)))
        }
        "n" => {
            settings.adjust_fft_size(false);
            (false, Some(format!("fft_size = {}", settings.fft_size)))
        }
        // Waterfall layout
        "u" => {
            settings.scale_spacing(true);
            (false, Some(format!("spacing = {:.4}", settings.spacing)))
        }
        "i" => {
            settings.scale_spacing(false);
            (false, Some(format!("spacing = {:.4}", settings.spacing)))
        }
        "j" => {
            settings.move_z_offset(false); // +z (toward camera)
            (false, Some(format!("z_offset = {:+.3}", settings.z_offset)))
        }
        "k" => {
            settings.move_z_offset(true); // −z (toward horizon)
            (false, Some(format!("z_offset = {:+.3}", settings.z_offset)))
        }
        // Scales
        "-" => {
            settings.scale_spectrum(false);
            (false, Some(format!("freq_scale = {:.3}", settings.freq_scale)))
        }
        "=" => {
            settings.scale_spectrum(true);
            (false, Some(format!("freq_scale = {:.3}", settings.freq_scale)))
        }
        "_" => {
            settings.scale_waveform(false);
            (false, Some(format!("wave_scale = {:.3}", settings.wave_scale)))
        }
        "+" => {
            settings.scale_waveform(true);
            (false, Some(format!("wave_scale = {:.3}", settings.wave_scale)))
        }
        "l" => {
            settings.scale_lissajous(false);
            (false, Some(format!("liss_scale = {:.3}", settings.liss_scale)))
        }
        "L" => {
            settings.scale_lissajous(true);
            (false, Some(format!("liss_scale = {:.3}", settings.liss_scale)))
        }
        // Freeze: keeps rendering, stops ingesting (f/z)
        "f" | "z" => {
            settings.toggle_freeze();
            (false, Some(format!("frozen = {}", on_off(settings.frozen))))
        }
        // Mute: also stops ingesting (listener-only build)
        "m" => {
            settings.toggle_mute();
            (false, Some(format!("muted = {}", on_off(settings.muted))))
        }
        // HUD
        "h" => {
            settings.toggle_hints();
            (false, Some(format!("keybinds = {}", on_off(settings.show_hints))))
        }
        "H" => {
            settings.toggle_hud();
            (false, Some(format!("hud = {}", on_off(settings.hud_on))))
        }
        // Quit
        "q" => (true, None),
        _ => (false, None),
    }
}

/// Camera keys (uppercase/bracket family).
pub fn handle_camera_key(camera: &mut Camera, key: &str) -> KeyPress {
    match key {
        // `[`/`]` eye tilt 0.025 steps (sndpeek)
        "[" => {
            camera.orbit(0.0, -TILT_STEP);
            (false, Some(format!("camera tilt = {:+.3} rad", camera.elevation)))
        }
        "]" => {
            camera.orbit(0.0, TILT_STEP);
            (false, Some(format!("camera tilt = {:+.3} rad", camera.elevation)))
        }
        // F reset/fit; X/Y/Z snap top/side/front
        "F" => {
            camera.reset();
            (false, Some("camera reset".to_owned()))
        }
        "X" => {
            camera.snap(SnapView::Top);
            (false, Some("camera: top view".to_owned()))
        }
        "Y" => {
            camera.snap(SnapView::Side);
            (false, Some("camera: side view".to_owned()))
        }
        "Z" => {
            camera.snap(SnapView::Front);
            (false, Some("camera: front view".to_owned()))
        }
        _ => (false, None),
    }
}

/// The full key map, for the startup INFO dump ([`keybind_block`]).
const KEYBINDS: &[(&str, &str)] = &[
    ("drag", "orbit"),
    ("shift-drag", "pan"),
    ("wheel", "zoom"),
    ("[ ]", "camera tilt"),
    ("F", "camera reset"),
    ("X Y Z", "camera snap top/side/front"),
    ("1", "waveform on/off"),
    ("2", "lissajous on/off"),
    ("3 w", "waterfall on/off"),
    ("d", "dB/linear magnitude"),
    ("S", "surface on/off"),
    ("r", "rainbow on/off"),
    ("a", "auto-scale waterfall gain (content-adaptive)"),
    ("< >", "waveform/lissajous strip width"),
    ("y", "lissajous delay cycle"),
    ("u i", "slice spacing"),
    ("j k", "waterfall z-offset"),
    ("PageUp PageDown", "waterfall history layers (depth)"),
    ("o n", "FFT size up/down (512-8192 ladder)"),
    ("c v", "freq-axis log factor"),
    ("-", "spectrum scale down"),
    ("=", "spectrum scale up"),
    ("_", "waveform scale down"),
    ("+", "waveform scale up"),
    ("l L", "lissajous scale"),
    ("f z", "freeze"),
    ("m", "mute"),
    ("h", "keybind hints"),
    ("H", "whole HUD"),
    ("T", "next theme"),
    ("p", "print settings"),
    ("q / Esc", "quit"),
];

/// The keybind list as a terminal-friendly block: one row per bind,
/// keys right-aligned — logged once at startup (sndpeek parity).
pub fn keybind_block() -> String {
    let mut out = String::new();
    for (keys, action) in KEYBINDS {
        out.push_str(&format!("{keys:>10}  {action}\n"));
    }
    out
}


/// Camera tilt step (`[`/`]`, sndpeek `g_inc_val_kb`).
const TILT_STEP: f32 = 0.025;

#[cfg(test)]
mod tests {
    use super::*;
    use neopeek_render::camera::{ELEV_MAX, SnapView as SV};
    use neopeek_render::views::ViewSettings;

    #[test]
    fn toggles_come_back_around() {
        let mut s = ViewSettings::default();
        assert!(s.waveform_on);
        assert!(!handle_key(&mut s, "1").0);
        assert!(!s.waveform_on);
        handle_key(&mut s, "w");
        assert!(!s.waterfall_on);
        handle_key(&mut s, "3");
        assert!(s.waterfall_on);
        handle_key(&mut s, "2");
        assert!(!s.lissajous_on);
    }

    #[test]
    fn mode_keys_flow_into_settings() {
        let mut s = ViewSettings::default();
        handle_key(&mut s, "d");
        assert!(s.db);
        handle_key(&mut s, "r");
        assert!(s.rainbow);
        let before = s.spacing;
        handle_key(&mut s, "u"); // wider
        assert!(s.spacing > before);
        handle_key(&mut s, "i");
        assert!((s.spacing - before).abs() < 1e-3, "spacing roundtrip {} → {}", before, s.spacing);

        let z0 = s.z_offset;
        handle_key(&mut s, "j"); // +0.1
        assert!((s.z_offset - (z0 + 0.1)).abs() < 1e-5);
        handle_key(&mut s, "k"); // −0.1
        assert!((s.z_offset - z0).abs() < 1e-5);

        let ls = s.liss_scale;
        handle_key(&mut s, "L");
        assert!(s.liss_scale > ls);
        handle_key(&mut s, "l");
        assert!((s.liss_scale - ls).abs() < 1e-2, "back to {ls}? got {}", s.liss_scale);

        let f0 = s.freq_scale;
        handle_key(&mut s, "=");
        assert!(s.freq_scale > f0);
        handle_key(&mut s, "-");
        assert!((s.freq_scale - f0).abs() < 1e-3, "back to {f0}? got {}", s.freq_scale);

        let w0 = s.wave_scale;
        handle_key(&mut s, "_");
        assert!(s.wave_scale < w0);
        handle_key(&mut s, "+");
        assert!((s.wave_scale - w0).abs() < 1e-3, "back to {w0}? got {}", s.wave_scale);

        assert!(!s.frozen);
        handle_key(&mut s, "f");
        assert!(s.frozen);
        handle_key(&mut s, "z");
        assert!(!s.frozen);
        // m/h/H toggles (mute = pause ingestion, hints, whole HUD).
        assert!(!s.muted);
        handle_key(&mut s, "m");
        assert!(s.muted);
        handle_key(&mut s, "m");
        assert!(!s.muted);
        handle_key(&mut s, "h");
        assert!(!s.show_hints);
        handle_key(&mut s, "h");
        assert!(s.show_hints);
        handle_key(&mut s, "H");
        assert!(!s.hud_on);
        handle_key(&mut s, "H");
        assert!(s.hud_on);
        // S surface, <> width, y delay.
        assert!(!s.surface);
        handle_key(&mut s, "S");
        assert!(s.surface);
        handle_key(&mut s, "S");
        assert!(!s.surface);
        let w0 = s.strip_width;
        handle_key(&mut s, ">");
        assert!(s.strip_width > w0, "width up");
        // ×0.8 then ×1.25 can round-trip to exactly w0; test down from a
        // fresh default instead.
        let mut down = ViewSettings::default();
        handle_key(&mut down, "<");
        assert!(down.strip_width < w0, "width down");
        assert_eq!(s.liss_delay, 512);
        handle_key(&mut s, "y");
        assert_eq!(s.liss_delay, 256);
        handle_key(&mut s, "y");
        assert_eq!(s.liss_delay, 128);
    }

    #[test]
    fn quit_keys() {
        let mut s = ViewSettings::default();
        let (quit, desc) = handle_key(&mut s, "q");
        assert!(quit);
        assert!(desc.is_none());
        assert!(!handle_key(&mut s, "p").0); // unmapped → stays
    }

    /// Depth keys report the sndpeek-style per-key feedback line.
    #[test]
    fn depth_keys_log_layers_description() {
        let mut s = ViewSettings::default();
        let (_, desc) = handle_key(&mut s, "PageUp");
        assert_eq!(desc.as_deref(), Some("depth = 60 layers"));
        assert_eq!(s.depth, 60);
        let (_, desc) = handle_key(&mut s, "PageDown");
        assert_eq!(desc.as_deref(), Some("depth = 48 layers"));
        assert_eq!(s.depth, 48);
        // Clamped at both ends.
        while s.depth < 256 {
            handle_key(&mut s, "PageUp");
        }
        let (_, desc) = handle_key(&mut s, "PageUp");
        assert_eq!(desc.as_deref(), Some("depth = 256 layers"));
        while s.depth > 8 {
            handle_key(&mut s, "PageDown");
        }
        let (_, desc) = handle_key(&mut s, "PageDown");
        assert_eq!(desc.as_deref(), Some("depth = 8 layers"));
        // Not quit keys.
        assert!(!handle_key(&mut s, "PageDown").0);
    }

    /// `o`/`n` FFT-size keys: one ladder rung per press, clamped at the
    /// ends, with the sndpeek-style INFO line.
    #[test]
    fn fft_size_keys_step_and_report() {
        let mut s = ViewSettings::default();
        let (_, desc) = handle_key(&mut s, "o");
        assert_eq!(desc.as_deref(), Some("fft_size = 4096"));
        assert_eq!(s.fft_size, 4096);
        let (_, desc) = handle_key(&mut s, "n");
        assert_eq!(desc.as_deref(), Some("fft_size = 2048"));
        while s.fft_size < 8192 {
            handle_key(&mut s, "o");
        }
        let (_, desc) = handle_key(&mut s, "o");
        assert_eq!(desc.as_deref(), Some("fft_size = 8192"), "pinned at the ceiling");
        while s.fft_size > 512 {
            handle_key(&mut s, "n");
        }
        let (_, desc) = handle_key(&mut s, "n");
        assert_eq!(desc.as_deref(), Some("fft_size = 512"), "pinned at the floor");
        assert!(!handle_key(&mut s, "n").0, "not a quit key");
    }

    /// `c`/`v` log-factor keys: scaled, clamped, and reported per-key.
    #[test]
    fn log_factor_keys_scale_and_clamp() {
        let mut s = ViewSettings::default();
        handle_key(&mut s, "c");
        assert!((s.log_factor - 1.725).abs() < 1e-3, "got {}", s.log_factor);
        while s.log_factor > 0.0 {
            handle_key(&mut s, "v");
        }
        handle_key(&mut s, "v");
        assert_eq!(s.log_factor, 0.0, "0 = fully linear, pinned at the floor");
        handle_key(&mut s, "c");
        assert!((s.log_factor - 0.05).abs() < 1e-9, "steps off exact zero");
        while s.log_factor < 8.0 {
            handle_key(&mut s, "c");
        }
        handle_key(&mut s, "c");
        assert_eq!(s.log_factor, 8.0, "pinned at the ceiling");
    }

    #[test]
    fn camera_keys() {
        // Default vantage ≈ M4 fixed eye
        let mut c = Camera::default_vantage(800, 600);
        assert!((c.elevation - 0.55).abs() < 1e-6);
        handle_key_plain(&mut c, "X");
        assert!((c.elevation - ELEV_MAX).abs() < 1e-6);
        handle_key_plain(&mut c, "Y");
        assert!((c.azimuth - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
        handle_key_plain(&mut c, "Z");
        assert!(c.elevation.abs() < 1e-6 && c.azimuth.abs() < 1e-6);

        // `F` restores the default vantage
        let mut d = Camera::default_vantage(800, 600);
        d.orbit(3.0, 1.0);
        d.dolly(3.0);
        d.target = [5.0, 5.0, 5.0];
        handle_key_plain(&mut d, "F");
        let e = Camera::default_vantage(800, 600);
        assert!((d.azimuth - e.azimuth).abs() < 1e-6);
        assert!((d.distance - e.distance).abs() < 1e-6);
        assert!((d.target[0] - e.target[0]).abs() < 1e-6);
        assert!(!handle_key_plain(&mut c, "p"));
    }

    #[test]
    fn descriptions_report_changed_settings() {
        // Toggle reports the NEW state.
        let mut s = ViewSettings::default();
        assert_eq!(handle_key(&mut s, "m").1.as_deref(), Some("muted = on"));
        assert_eq!(handle_key(&mut s, "m").1.as_deref(), Some("muted = off"));
        assert_eq!(handle_key(&mut s, "1").1.as_deref(), Some("waveform = off"));
        // `a` auto-scale reports the NEW state like every toggle.
        let mut sa = ViewSettings::default();
        assert_eq!(handle_key(&mut sa, "a").1.as_deref(), Some("auto_scale = on"));
        assert!(sa.auto_scale);
        assert_eq!(handle_key(&mut sa, "a").1.as_deref(), Some("auto_scale = off"));
        assert!(!sa.auto_scale);
        assert_eq!(handle_key(&mut s, "d").1.as_deref(), Some("magnitude = dB"));
        // Scale key reports the multiplied value.
        let mut f = ViewSettings::default();
        let (quit, desc) = handle_key(&mut f, "=");
        assert!(!quit);
        assert_eq!(desc.as_deref(), Some("freq_scale = 1.010"));
        // Delay cycle reports the new sample count (512 → 256).
        assert_eq!(handle_key(&mut s, "y").1.as_deref(), Some("liss_delay = 256"));
        // Keys that change nothing: quit + unmapped.
        assert_eq!(handle_key(&mut s, "q").1, None);
        assert_eq!(handle_key(&mut s, "p").1, None);
        assert_eq!(handle_key(&mut s, "Ω").1, None);
        // Camera keys describe what moved.
        let mut c = Camera::default_vantage(800, 600);
        assert_eq!(handle_camera_key(&mut c, "F").1.as_deref(), Some("camera reset"));
        assert_eq!(handle_camera_key(&mut c, "X").1.as_deref(), Some("camera: top view"));
        assert_eq!(handle_camera_key(&mut c, "p").1, None);
        let desc = handle_camera_key(&mut c, "[").1.expect("tilt logs");
        assert!(desc.starts_with("camera tilt = "), "{desc}");
        assert!(handle_camera_key(&mut c, "]").1.is_some());
    }

    #[test]
    fn keybind_block_lists_every_key() {
        let block = keybind_block();
        // One row per entry, keys right-aligned before the action.
        for needle in ["drag", "1", "p", "q / Esc", "camera reset", "mute", "PageUp PageDown", "c v", "o n", "FFT size"] {
            assert!(block.contains(needle), "keybind block missing {needle:?}:\n{block}");
        }
        assert!(block.ends_with('\n'), "rows are newline-terminated");
    }

    fn handle_key_plain(c: &mut Camera, k: &str) -> bool {
        handle_camera_key(c, k).0
    }

    #[test]
    fn snap_enum_matches_keys() {
        // X→top, Y→side, Z→front (documented mapping).
        let mut c = Camera::default_vantage(800, 600);
        c.snap(SV::Top);
        assert!((c.elevation - ELEV_MAX).abs() < 1e-6);
    }
}
