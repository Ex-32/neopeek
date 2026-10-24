//! Reusable `--print`/`p` settings dump (sndpeek parity): formats the
//! current [`ViewSettings`] + [`Camera`] state. The binary wires it to
//! both the `p` key and the future `--print` console flag.

use crate::camera::Camera;
use crate::views::ViewSettings;

/// Multi-line human-readable dump of the live settings.
pub fn settings_string(settings: &ViewSettings, camera: &Camera) -> String {
    let flag = |on: bool| if on { "on" } else { "off" };
    format!(
        "neopeek settings\n\
         waterfall: {}, surface: {}, waveform: {}, lissajous: {}\n\
         magnitude: {}, rainbow: {}, frozen: {}, muted: {}, auto_scale: {}\n\
         spacing: {:.4}, z_offset: {:+.3}, log_factor: {:.3}\n\
         depth: {} layers (history slices)\n\
         freq_scale: {:.3}, wave_scale: {:.3}, liss_scale: {:.3}\n\
         strip_width: {:.4}, liss_delay: {}\n\
         hud: {}, keybinds: {}\n\
         camera.target = ({:.3}, {:.3}, {:.3})\n\
         camera.azimuth = {:.3} rad, elevation = {:.3} rad, distance = {:.3}\n\
         camera.fov = {:.1}°, near = {:.3}, far = {:.1}, aspect = {:.3}",
        flag(settings.waterfall_on),
        flag(settings.surface),
        flag(settings.waveform_on),
        flag(settings.lissajous_on),
        if settings.db { "dB" } else { "linear" },
        flag(settings.rainbow),
        flag(settings.frozen),
        flag(settings.muted),
        flag(settings.auto_scale),
        settings.spacing,
        settings.z_offset,
        settings.log_factor,
        settings.depth,
        settings.freq_scale,
        settings.wave_scale,
        settings.liss_scale,
        settings.strip_width,
        settings.liss_delay,
        flag(settings.hud_on),
        flag(settings.show_hints),
        camera.target[0],
        camera.target[1],
        camera.target[2],
        camera.azimuth,
        camera.elevation,
        camera.distance,
        camera.fov_y.to_degrees(),
        camera.near,
        camera.far,
        camera.aspect,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dump_contains_all_m5_keys() {
        let s = ViewSettings::default();
        let c = Camera::default_vantage(800, 600);
        let txt = settings_string(&s, &c);
        for needle in [
            "waterfall: on",
            "surface: off",
            "magnitude: linear",
            "muted: off",
            "auto_scale: off",
            "log_factor: 1.500",
            "depth: 48 layers",
            "spacing: 0.1200",
            "liss_scale: 0.450",
            "strip_width: 0.0040",
            "liss_delay: 512",
            "hud: on",
            "camera.azimuth",
            "distance = 3.500",
            "fov = 45.0°",
        ] {
            assert!(txt.contains(needle), "dump missing {needle:?}:\n{txt}");
        }
    }

    #[test]
    fn dump_reflects_state_changes() {
        let mut s = ViewSettings::default();
        let mut c = Camera::default_vantage(800, 600);
        s.toggle_db();
        s.toggle_freeze();
        s.toggle_auto_scale();
        c.orbit(1.0, 0.0);
        let txt = settings_string(&s, &c);
        assert!(txt.contains("magnitude: dB"));
        assert!(txt.contains("frozen: on"));
        assert!(txt.contains("auto_scale: on"));
        assert!(txt.contains("azimuth = 1.000"));
    }
}
