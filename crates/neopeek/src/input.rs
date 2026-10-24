//! Input mapping (PLAN.md §4.3): keys + mouse handling stay in the
//! binary; it owns the [`ViewSettings`] and the shell [`Camera`].

use neopeek_render::camera::{Camera, SnapView};
use neopeek_render::views::ViewSettings;

/// Applies one logical character key press. Returns `true` on quit.
pub fn handle_key(settings: &mut ViewSettings, key: &str) -> bool {
    match key {
        // View toggles
        "1" => settings.toggle_waveform(),
        "2" => settings.toggle_lissajous(),
        "3" | "w" => settings.toggle_waterfall(),
        // Magnitude / color modes
        "d" => settings.toggle_db(),
        "S" => settings.toggle_surface(),
        "r" => settings.toggle_rainbow(),
        // Width of the waveform/Lissajous quads (< / >)
        "<" => settings.scale_strip_width(false),
        ">" => settings.scale_strip_width(true),
        // Pseudo-Lissajous delay (y; Y is the snap-side view)
        "y" => settings.cycle_liss_delay(),
        // Waterfall layout
        "u" => settings.scale_spacing(true),
        "i" => settings.scale_spacing(false),
        "j" => settings.move_z_offset(false), // +z (toward camera)
        "k" => settings.move_z_offset(true),  // −z (toward horizon)
        // Scales
        "-" => settings.scale_spectrum(false),
        "=" => settings.scale_spectrum(true),
        "_" => settings.scale_waveform(false),
        "+" => settings.scale_waveform(true),
        "l" => settings.scale_lissajous(false),
        "L" => settings.scale_lissajous(true),
        // Freeze: keeps rendering, stops ingesting (f/z)
        "f" | "z" => settings.toggle_freeze(),
        // Mute: also stops ingesting (listener-only build)
        "m" => settings.toggle_mute(),
        // HUD
        "h" => settings.toggle_hints(),
        "H" => settings.toggle_hud(),
        // Quit
        "q" => return true,
        _ => {}
    }
    false
}

/// Camera keys (uppercase/bracket family). Returns `true` on quit.
pub fn handle_camera_key(camera: &mut Camera, key: &str) -> bool {
    match key {
        // `[`/`]` eye tilt 0.025 steps (sndpeek)
        "[" => camera.orbit(0.0, -TILT_STEP),
        "]" => camera.orbit(0.0, TILT_STEP),
        // F reset/fit; X/Y/Z snap top/side/front
        "F" => camera.reset(),
        "X" => camera.snap(SnapView::Top),
        "Y" => camera.snap(SnapView::Side),
        "Z" => camera.snap(SnapView::Front),
        _ => {}
    }
    false
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
        assert!(!handle_key(&mut s, "1"));
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
        assert!(handle_key(&mut s, "q"));
        assert!(!handle_key(&mut s, "p")); // unmapped → stays
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

    fn handle_key_plain(c: &mut Camera, k: &str) -> bool {
        handle_camera_key(c, k)
    }

    #[test]
    fn snap_enum_matches_keys() {
        // X→top, Y→side, Z→front (documented mapping).
        let mut c = Camera::default_vantage(800, 600);
        c.snap(SV::Top);
        assert!((c.elevation - ELEV_MAX).abs() < 1e-6);
    }
}
