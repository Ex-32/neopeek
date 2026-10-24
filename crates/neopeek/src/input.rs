//! Key mapping (PLAN.md §4.3, M4 subset): input handling stays in the
//! binary; it owns the [`ViewSettings`]. Mouse camera interaction = M5.

use neopeek_render::views::ViewSettings;

/// Applies one logical key press. Returns `true` when the app should exit.
pub fn handle_key(settings: &mut ViewSettings, key: &str) -> bool {
    debug_assert!(settings.log_factor > 0.0);
    match key {
        // View toggles
        "1" => settings.toggle_waveform(),
        "2" => settings.toggle_lissajous(),
        "3" | "w" => settings.toggle_waterfall(),
        // Magnitude / color modes
        "d" => settings.toggle_db(),
        "r" => settings.toggle_rainbow(),
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
        // Quit
        "q" => return true,
        _ => {}
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // '_' needs shift on most layouts: unshifted is '-'; the logical
        // key is the character, shift → "+"; so '_' tests both directly.
        handle_key(&mut s, "_");
        assert!(s.wave_scale < w0);
        handle_key(&mut s, "+");
        assert!((s.wave_scale - w0).abs() < 1e-3, "back to {w0}? got {}", s.wave_scale);

        assert!(!s.frozen);
        handle_key(&mut s, "f");
        assert!(s.frozen);
        handle_key(&mut s, "z");
        assert!(!s.frozen);
    }

    #[test]
    fn quit_keys() {
        let mut s = ViewSettings::default();
        assert!(handle_key(&mut s, "q"));
        assert!(!handle_key(&mut s, "p")); // unmapped → stays
    }
}
