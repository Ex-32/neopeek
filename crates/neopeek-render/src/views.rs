//! View settings and the waveform/Lissajous line-strip builders
//! (PLAN.md §4.1 bottom half; constants from `docs/research-sndpeek.md`).
//!
//! Pure CPU geometry — unit-tested headlessly. Builders append into a
//! caller-provided buffer that the renderer reuses across frames (no
//! per-frame allocation on the hot path).

use neopeek_dsp::{Frame, WINDOW_FRAMES};

// Default vantage constants come from `crate::camera`; view constants here.
pub const WAVE_Y: f32 = 0.7;
pub const WAVE_SPAN: f32 = 1.8; // x ∈ −1.8…1.8
pub const LISS_X: f32 = 1.2;
pub const LISS_SCALE: f32 = 0.45;
/// Pseudo-Lissajous delay (mono source): half the window.
pub const LISS_DELAY: usize = WINDOW_FRAMES / 2;

/// Mutable view state; the binary's input layer mutates it, the renderer
/// reads it every frame. All knobs from the M4 key list live here.
#[derive(Debug, Clone, Copy)]
pub struct ViewSettings {
    /// View toggles (`1`/`2`/`3`/`w`). All default on.
    pub waveform_on: bool,
    pub lissajous_on: bool,
    pub waterfall_on: bool,
    /// `d`: dB magnitude mode instead of linear.
    pub db: bool,
    /// `r`: rainbow depth colors instead of the green depth-dim look.
    pub rainbow: bool,
    /// `f`/`z`: stop ingesting frames, keep rendering.
    pub frozen: bool,
    /// `u`/`i`: slice z-spacing (default 0.12), `j`/`k`: z-offset (±0.1).
    pub spacing: f32,
    pub z_offset: f32,
    /// Frequency-axis position precompute (see `crate::grid`), default 1.5.
    pub log_factor: f32,
    /// `-`/`=`: spectrum frequency scale (default 1.0).
    pub freq_scale: f32,
    /// `_`/`+`: waveform vertical scale (default 1.0 — sndpeek `time_scale`).
    pub wave_scale: f32,
    /// `l`/`L`: Lissajous scale (default 0.45).
    pub liss_scale: f32,
}

impl Default for ViewSettings {
    fn default() -> Self {
        Self {
            waveform_on: true,
            lissajous_on: true,
            waterfall_on: true,
            db: false,
            rainbow: false,
            frozen: false,
            spacing: 0.12,
            z_offset: 0.0,
            log_factor: 1.5,
            freq_scale: 1.0,
            wave_scale: 1.0,
            liss_scale: LISS_SCALE,
        }
    }
}

impl ViewSettings {
    /// `-`/`=`: spectrum freq scale ×0.99 / ×1.01, clamped.
    pub fn scale_spectrum(&mut self, up: bool) {
        self.freq_scale = (self.freq_scale * if up { 1.01 } else { 0.99 })
            .clamp(0.01, 100.0);
    }

    /// `_`/`+`: waveform scale ×0.99 / ×1.01, clamped.
    pub fn scale_waveform(&mut self, up: bool) {
        self.wave_scale = (self.wave_scale * if up { 1.01 } else { 0.99 })
            .clamp(0.01, 100.0);
    }

    /// `u`/`i`: slice spacing ×1.02 / ×0.98 (per PLAN.md §4.3), clamped.
    pub fn scale_spacing(&mut self, wider: bool) {
        self.spacing = (self.spacing * if wider { 1.02 } else { 0.98 })
            .clamp(0.002, 1.0);
    }

    /// `j`/`k`: waterfall z-offset ±0.1 (sndpeek `g_dz`), clamped.
    pub fn move_z_offset(&mut self, toward_horizon: bool) {
        self.z_offset += if toward_horizon { -0.1 } else { 0.1 };
        self.z_offset = self.z_offset.clamp(-20.0, 20.0);
    }

    /// `l`/`L`: Lissajous scale ×0.98 / ×1.02, clamped.
    pub fn scale_lissajous(&mut self, up: bool) {
        self.liss_scale = (self.liss_scale * if up { 1.02 } else { 0.98 })
            .clamp(0.0, 4.0);
    }

    pub fn toggle_waveform(&mut self) {
        self.waveform_on = !self.waveform_on;
    }
    pub fn toggle_lissajous(&mut self) {
        self.lissajous_on = !self.lissajous_on;
    }
    pub fn toggle_waterfall(&mut self) {
        self.waterfall_on = !self.waterfall_on;
    }
    pub fn toggle_db(&mut self) {
        self.db = !self.db;
    }
    pub fn toggle_rainbow(&mut self) {
        self.rainbow = !self.rainbow;
    }
    /// `f`/`z`: freeze toggle.
    pub fn toggle_freeze(&mut self) {
        self.frozen = !self.frozen;
    }
}

/// Line vertices are world-space 3D positions (12 bytes); runs of them
/// fill one draw as a `LineStrip`/indexed strip.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LineVertex {
    pub pos: [f32; 3],
}

/// Append the mono waveform strip: x ∈ −1.8…1.8 across the window at
/// y = 0.7, vertically scaled `wave_scale · 2` (sndpeek display mapping).
pub fn append_waveform(frame: &Frame, s: &ViewSettings, out: &mut Vec<LineVertex>) {
    for i in 0..WINDOW_FRAMES {
        let x = -WAVE_SPAN + 2.0 * WAVE_SPAN * (i as f32 / (WINDOW_FRAMES - 1) as f32);
        let y = WAVE_Y + s.wave_scale * 2.0 * frame.mono[i];
        out.push(LineVertex { pos: [x, y, 0.0] });
    }
}

/// Append the Lissajous strip on the plane `x = 1.2`:
/// - true stereo: figure-X = L, figure-Y = R (figure-Y maps to world y,
///   figure-X maps to world z, toward the camera for positive L);
/// - mono-ish source (`Frame::mono_like`): pseudo-Lissajous, mono vs its
///   512-sample delayed copy.
pub fn append_lissajous(frame: &Frame, s: &ViewSettings, out: &mut Vec<LineVertex>) {
    for i in 0..WINDOW_FRAMES {
        let (u, v) = if frame.mono_like {
            (
                frame.mono[i],
                frame.mono[(i + LISS_DELAY) % WINDOW_FRAMES],
            )
        } else {
            (frame.left[i], frame.right[i])
        };
        out.push(LineVertex {
            pos: [LISS_X, s.liss_scale * v, s.liss_scale * u],
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_plan() {
        let s = ViewSettings::default();
        assert!((s.spacing - 0.12).abs() < 1e-9);
        assert!((s.log_factor - 1.5).abs() < 1e-9);
        assert!((s.liss_scale - 0.45).abs() < 1e-9);
        assert!((s.z_offset - 0.0).abs() < 1e-9);
        assert!(s.waveform_on && s.lissajous_on && s.waterfall_on);
        assert!(!s.db && !s.rainbow && !s.frozen);
    }

    #[test]
    fn waveform_strip_spans_x_at_wave_y() {
        let mut f = Frame::default();
        f.mono[0] = 1.0;
        f.mono[WINDOW_FRAMES - 1] = -0.5;
        let s = ViewSettings::default();
        let mut out = Vec::new();
        append_waveform(&f, &s, &mut out);
        assert_eq!(out.len(), WINDOW_FRAMES);
        approx(out[0].pos[0], -WAVE_SPAN);
        approx(out[WINDOW_FRAMES - 1].pos[0], WAVE_SPAN);
        approx(out[0].pos[1], WAVE_Y + 2.0); // 1.0 · gain·scale·2
        approx(out[WINDOW_FRAMES - 1].pos[1], WAVE_Y - 1.0);
        approx(out[0].pos[2], 0.0);
        // scale setting flows through
        let mut s2 = ViewSettings::default();
        for _ in 0..10 {
            s2.scale_waveform(true);
        }
        out.clear();
        append_waveform(&f, &s2, &mut out);
        assert!(out[0].pos[1] > WAVE_Y + 2.0, "scale up grows y");
    }

    #[test]
    fn lissajous_true_stereo_vs_pseudo() {
        let mut f = Frame::default();
        f.left[0] = 1.0;
        f.right[0] = -0.25;
        f.mono[0] = 0.375;
        f.mono[LISS_DELAY] = 0.9;
        // true-stereo frame
        f.mono_like = false;
        let s = ViewSettings::default();
        let mut out = Vec::new();
        append_lissajous(&f, &s, &mut out);
        assert_eq!(out.len(), WINDOW_FRAMES);
        approx(out[0].pos[0], LISS_X); // plane constant
        approx(out[0].pos[1], LISS_SCALE * f.right[0]); // figure-Y = R
        approx(out[0].pos[2], LISS_SCALE * f.left[0]); // figure-X = L
        // pseudo: mono vs delayed copy (same y for all slices in delta 0)
        f.mono_like = true;
        out.clear();
        append_lissajous(&f, &s, &mut out);
        // pseudo: figure-Y = mono[(i+delay)], figure-X = mono[i]
        approx(out[0].pos[1], LISS_SCALE * f.mono[LISS_DELAY]);
        approx(out[0].pos[2], LISS_SCALE * f.mono[0]);
    }

    #[test]
    fn scale_keys_step_and_clamp() {
        let mut s = ViewSettings::default();
        for _ in 0..2000 {
            s.scale_spectrum(true);
        }
        assert_eq!(s.freq_scale, 100.0, "clamp high");
        for _ in 0..2500 {
            s.scale_spectrum(false);
        }
        assert_eq!(s.freq_scale, 0.01, "clamp low");

        let mut w = ViewSettings::default();
        for _ in 0..2000 {
            w.scale_waveform(true);
        }
        assert_eq!(w.wave_scale, 100.0);

        let mut l = ViewSettings::default();
        for _ in 0..400 {
            l.scale_lissajous(true);
        }
        assert_eq!(l.liss_scale, 4.0);
        for _ in 0..5000 {
            l.scale_lissajous(false);
        }
        // Multiplication asymptotically approaches the 0.0 floor; it never
        // wraps past it or goes negative.
        assert!(l.liss_scale < 1e-30, "down-scaled lissajous floor {}", l.liss_scale);
        assert!(l.liss_scale >= 0.0);
        // …a single step from the smallest positive value stays ≥ 0
        // (the ×0.98 step can never round past the lower clamp).
        let mut l2 = ViewSettings {
            liss_scale: f32::MIN_POSITIVE,
            ..ViewSettings::default()
        };
        l2.scale_lissajous(false);
        assert!(l2.liss_scale >= 0.0);

        let mut sp = ViewSettings::default();
        for _ in 0..400 {
            sp.scale_spacing(true);
        }
        assert_eq!(sp.spacing, 1.0);
    }

    #[test]
    fn z_offset_steps_and_clamps() {
        let mut s = ViewSettings::default();
        s.move_z_offset(true);
        approx(s.z_offset, -0.1);
        for _ in 0..100_000 {
            s.move_z_offset(true);
        }
        assert_eq!(s.z_offset, -20.0, "clamp");
        s.move_z_offset(false);
        approx(s.z_offset, -19.9);
    }

    #[test]
    fn toggles_flip() {
        let mut s = ViewSettings::default();
        s.toggle_waveform();
        assert!(!s.waveform_on);
        s.toggle_freeze();
        assert!(s.frozen);
        s.toggle_rainbow();
        assert!(s.rainbow);
        s.toggle_db();
        assert!(s.db);
        s.toggle_waterfall();
        assert!(!s.waterfall_on);
        s.toggle_lissajous();
        assert!(!s.lissajous_on);
    }

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-5, "{a} ≠ {b}");
    }
}
