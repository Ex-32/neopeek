//! View settings and the waveform/Lissajous line-strip builders
//! (PLAN.md §4.1 bottom half; constants from `docs/research-sndpeek.md`).
//!
//! Pure CPU geometry — unit-tested headlessly. Builders append into a
//! caller-provided buffer that the renderer reuses across frames (no
//! per-frame allocation on the hot path).

use neopeek_dsp::{Frame, WINDOW_FRAMES};
use crate::grid::{DEFAULT_DEPTH, MAX_DEPTH, MIN_DEPTH};

/// Default vantage constants come from `crate::camera`; view constants here.
pub const WAVE_Y: f32 = 0.7;

/// Default waterfall uniform `gain` (sndpeek used 1.0). At 1.0 the linear
/// mapping flings a typical monitor frame (max |X| ≈ 0.3 →
/// y = 1.0·1.8·sqrt(25·0.3) ≈ 4.9) several times past the top of the view.
/// 0.35 maps the same content to y ≈ 1.73, which under the default vantage
/// (3.5 units out, 45° fov) projects to ≈ 0.86 of the front-slice frustum
/// half-height — firmly on screen, just under the top edge (pinned by the
/// `default_gain_typical_frame_fits` test via the real camera projection).
/// `-`/`=` still zoom *relative* to this default.
pub const DEFAULT_WATERFALL_GAIN: f32 = 0.35;
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
    /// `S`: filled-surface waterfall mode (theme `surface_ramp` colors).
    pub surface: bool,
    /// `r`: rainbow depth colors instead of the green depth-dim look.
    pub rainbow: bool,
    /// `f`/`z`: stop ingesting frames, keep rendering.
    pub frozen: bool,
    /// `m`: also stop ingesting frames (listener-only build: mute = pause).
    pub muted: bool,
    /// `H`: HUD (text + meters) visible at all.
    pub hud_on: bool,
    /// `h`: the keybind-hint line inside the bottom HUD panel.
    pub show_hints: bool,
    /// `u`/`i`: slice z-spacing (default 0.12), `j`/`k`: z-offset (±0.1).
    pub spacing: f32,
    pub z_offset: f32,
    /// Frequency-axis position precompute (see `crate::grid`), default 1.5.
    pub log_factor: f32,
    /// `PageUp`/`PageDown` + `--depth`: waterfall history layers
    /// (`grid::clamp_depth` bounds it; default `grid::DEFAULT_DEPTH`).
    /// Changing it rebuilds the history texture + grid buffers.
    pub depth: usize,
    /// `-`/`=`: spectrum frequency scale (default 1.0).
    pub freq_scale: f32,
    /// `_`/`+`: waveform vertical scale (default 1.0 — sndpeek `time_scale`).
    pub wave_scale: f32,
    /// `l`/`L`: Lissajous scale (default 0.45).
    pub liss_scale: f32,
    /// `<`/`>`: strip width (waveform/Lissajous quads, world units).
    /// Default ≈ the old 1-px hairline at the default vantage.
    pub strip_width: f32,
    /// `y`: pseudo-Lissajous delay in samples (mono-ish sources only);
    /// cycles 512 → 256 → 128 → 64 → 512. `Y` stays the snap-side view.
    pub liss_delay: usize,
}

impl Default for ViewSettings {
    fn default() -> Self {
        Self {
            waveform_on: true,
            lissajous_on: true,
            waterfall_on: true,
            db: false,
            surface: false,
            rainbow: false,
            frozen: false,
            muted: false,
            hud_on: true,
            show_hints: true,
            spacing: 0.12,
            z_offset: 0.0,
            log_factor: 1.5,
            depth: DEFAULT_DEPTH,
            freq_scale: 1.0,
            wave_scale: 1.0,
            liss_scale: LISS_SCALE,
            strip_width: 0.004,
            liss_delay: LISS_DELAY,
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

    /// `c`/`v`: waterfall freq-axis log factor ×1.15 / ×0.87 (sndpeek
    /// `g_log_factor` scaling, snappier steps so single presses are
    /// perceptible on the axis), clamped to 0.0 (fully linear)…8.0. Below
    /// 0.05 flattening snaps to exactly 0 (the linear axis — `v` pinned on
    /// zeros can no longer be lifted, so `c` from 0 steps to 0.05). The
    /// grid picks this up via the per-frame `Waterfall::set_log_factor`
    /// rebuild in `shell::render`.
    pub fn adjust_log_factor(&mut self, more_log: bool) {
        if !more_log && self.log_factor < 0.05 {
            self.log_factor = 0.0; // snap: exact linear axis
            return;
        }
        let next = self.log_factor * if more_log { 1.15 } else { 0.87 };
        self.log_factor = if more_log && self.log_factor == 0.0 {
            0.05 // multiplicative scaling can't lift an exact zero
        } else {
            next.clamp(0.0, 8.0)
        };
    }

    /// `PageUp`/`PageDown`: waterfall history layers ×1.25 / ×0.8,
    /// clamped to `grid::MIN_DEPTH`…`MAX_DEPTH` (no pow-2 requirement).
    pub fn adjust_depth(&mut self, more: bool) {
        let next = if more {
            (self.depth as f32 * 1.25).round() as usize
        } else {
            (self.depth as f32 * 0.8).round() as usize
        };
        self.depth = next.clamp(MIN_DEPTH, MAX_DEPTH);
    }

    /// Sets the history depth from a user request (CLI flag): same clamp
    /// the runtime keys apply, so `--depth 9999` means 256 layers.
    pub fn set_depth_clamped(&mut self, depth: usize) {
        self.depth = depth.clamp(MIN_DEPTH, MAX_DEPTH);
    }

    /// `<`/`>`: waveform/Lissajous strip width ×1.25 / ×0.8, clamped.
    pub fn scale_strip_width(&mut self, up: bool) {
        self.strip_width = (self.strip_width * if up { 1.25 } else { 0.8 })
            .clamp(0.001, 0.1);
    }

    /// `y`: pseudo-Lissajous delay halves (512→256→128→64→512).
    pub fn cycle_liss_delay(&mut self) {
        self.liss_delay = match self.liss_delay {
            d if d <= 64 => WINDOW_FRAMES / 2,
            d => d / 2,
        };
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
    /// `S`: filled-surface mode toggle.
    pub fn toggle_surface(&mut self) {
        self.surface = !self.surface;
    }
    pub fn toggle_rainbow(&mut self) {
        self.rainbow = !self.rainbow;
    }
    /// `f`/`z`: freeze toggle.
    pub fn toggle_freeze(&mut self) {
        self.frozen = !self.frozen;
    }
    /// `m`: mute (pause ingestion) toggle.
    pub fn toggle_mute(&mut self) {
        self.muted = !self.muted;
    }
    /// `H`: whole HUD toggle.
    pub fn toggle_hud(&mut self) {
        self.hud_on = !self.hud_on;
    }
    /// `h`: keybind-hint line toggle.
    pub fn toggle_hints(&mut self) {
        self.show_hints = !self.show_hints;
    }
}

/// Line vertices are world-space 3D positions (12 bytes); a strip is
/// drawn as a triangle strip with 2 verts per point (M8: width-expanded
/// quads replacing the 1-px GL lines).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LineVertex {
    pub pos: [f32; 3],
}

/// Append the mono waveform strip: x ∈ −1.8…1.8 across the window at
/// y = 0.7, vertically scaled `wave_scale · 2` (sndpeek display mapping).
/// Expanded perpendicular to the local tangent by `strip_width`.
pub fn append_waveform(frame: &Frame, s: &ViewSettings, out: &mut Vec<LineVertex>) {
    let hw = s.strip_width * 0.5;
    // Local tangents need neighbors; x is monotonic so tx never vanishes.
    let point = |i: usize| {
        (
            -WAVE_SPAN + 2.0 * WAVE_SPAN * (i as f32 / (WINDOW_FRAMES - 1) as f32),
            WAVE_Y + s.wave_scale * 2.0 * frame.mono[i],
        )
    };
    for i in 0..WINDOW_FRAMES {
        let (x, y) = point(i);
        let (xp, yp) = point(i.saturating_sub(1));
        let (xn, yn) = point((i + 1).min(WINDOW_FRAMES - 1));
        let (tx, ty) = (xn - xp, yn - yp);
        let len = (tx * tx + ty * ty).sqrt().max(1e-9);
        let (nx, ny) = (-ty / len, tx / len);
        out.push(LineVertex { pos: [x + nx * hw, y + ny * hw, 0.0] });
        out.push(LineVertex { pos: [x - nx * hw, y - ny * hw, 0.0] });
    }
}

/// Append the Lissajous strip on the plane `x = 1.2`:
/// - true stereo: figure-X = L, figure-Y = R (figure-Y maps to world y,
///   figure-X maps to world z, toward the camera for positive L);
/// - mono-ish source (`Frame::mono_like`): pseudo-Lissajous, mono vs its
///   `liss_delay`-sample delayed copy (`y` cycles the delay; `Y` is the
///   snap-side camera view, not this).
///
/// Closed figure → wrapped tangents; expanded by `strip_width`.
pub fn append_lissajous(frame: &Frame, s: &ViewSettings, out: &mut Vec<LineVertex>) {
    let hw = s.strip_width * 0.5;
    let point = |i: usize| {
        let (u, v) = if frame.mono_like {
            (
                frame.mono[i],
                frame.mono[(i + s.liss_delay) % WINDOW_FRAMES],
            )
        } else {
            (frame.left[i], frame.right[i])
        };
        (s.liss_scale * u, s.liss_scale * v) // (z, y) in-world
    };
    for i in 0..WINDOW_FRAMES {
        let (zu, yv) = point(i);
        let (zp, yp) = point((i + WINDOW_FRAMES - 1) % WINDOW_FRAMES);
        let (zn, yn) = point((i + 1) % WINDOW_FRAMES);
        let (tz, ty) = (zn - zp, yn - yp);
        let len = (tz * tz + ty * ty).sqrt().max(1e-9);
        let (nz, ny) = (-ty / len, tz / len);
        out.push(LineVertex { pos: [LISS_X, yv + ny * hw, zu + nz * hw] });
        out.push(LineVertex { pos: [LISS_X, yv - ny * hw, zu - nz * hw] });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::Camera;

    #[test]
    fn default_gain_typical_frame_fits() {
        // Sanity math for `DEFAULT_WATERFALL_GAIN`: the shader maps
        // y = gain·freq_scale·1.8·sqrt(25·|X|). A typical monitor frame
        // (max |X| ≈ 0.3) must land at a moderate fraction of the scene
        // height through the *real* default camera projection (sndpeek's
        // gain 1.0 flung the same content to y ≈ 4.9 — far off screen).
        let y = DEFAULT_WATERFALL_GAIN * 1.0 * 1.8 * (25.0 * 0.3f32).sqrt();
        assert!(
            (y - 1.726).abs() < 0.01,
            "y = {y}, expected ≈1.73 (0.35·1.8·sqrt(7.5))"
        );
        // Through the default vantage (target [0, 0.25, −1.3], distance
        // 3.5, fov 45°): the top of the fresh-slice plane (z = 0) is the
        // world y where the frustum cuts it. Content at 0.35 gain must sit
        // under that top but well above center — moderate, not overshoot.
        let cam = Camera::default_vantage(800, 600);
        let vp = cam.view_proj();
        let ndc_top = crate::camera::project(vp, [0.0, 2.0, 0.0])[1];
        let ndc_our_peak = crate::camera::project(vp, [0.0, y, 0.0])[1];
        assert!(
            ndc_our_peak < ndc_top,
            "typical frame must stay under the fresh-slice frustum top: {ndc_our_peak} < {ndc_top}?"
        );
        // …and above the old too-tall default by a wide headroom: gain 1.0
        // overshoots the same frustum top.
        let ndc_old = crate::camera::project(vp, [0.0, 1.8 * 1.0 * (25.0 * 0.3f32).sqrt(), 0.0])[1];
        assert!(ndc_old > ndc_top, "gain 1.0 must overshoot: {ndc_old} > {ndc_top}?");
        // Roughly four-fifths of the half-height (documented ≈0.86):
        assert!(
            (ndc_our_peak - 0.86).abs() < 0.05,
            "default gain should land ≈0.86 of the half-height, got {ndc_our_peak}"
        );
    }

    #[test]
    fn defaults_match_plan() {
        let s = ViewSettings::default();
        assert!((s.spacing - 0.12).abs() < 1e-9);
        assert!((s.log_factor - 1.5).abs() < 1e-9);
        assert_eq!(s.depth, crate::grid::DEFAULT_DEPTH);
        assert!((s.liss_scale - 0.45).abs() < 1e-9);
        assert!((s.z_offset - 0.0).abs() < 1e-9);
        assert!(s.waveform_on && s.lissajous_on && s.waterfall_on);
        assert!(!s.db && !s.rainbow && !s.frozen);
    }

    /// `PageUp`/`PageDown` depth stepping ×1.25/×0.8 with the 8…256 clamp.
    #[test]
    fn adjust_depth_steps_and_clamps() {
        let mut s = ViewSettings::default();
        s.adjust_depth(true);
        assert_eq!(s.depth, 60); // 48 × 1.25
        while s.depth < crate::grid::MAX_DEPTH {
            s.adjust_depth(true);
        }
        assert_eq!(s.depth, crate::grid::MAX_DEPTH, "256 is the ceiling");
        s.adjust_depth(true); // pinned at max
        assert_eq!(s.depth, 256);

        while s.depth > crate::grid::MIN_DEPTH {
            s.adjust_depth(false);
        }
        assert_eq!(s.depth, crate::grid::MIN_DEPTH, "8 is the floor");
        s.adjust_depth(false); // pinned at min
        assert_eq!(s.depth, 8);
        // No pow-2 requirement; odd values survive a step fine.
        let mut s = ViewSettings::default();
        s.set_depth_clamped(91);
        s.adjust_depth(true);
        assert_eq!(s.depth, (91.0f32 * 1.25).round() as usize);
    }

    /// `--depth`: clamped the same way the runtime keys clamp.
    #[test]
    fn set_depth_clamped_uses_grid_range() {
        let mut s = ViewSettings::default();
        s.set_depth_clamped(0);
        assert_eq!(s.depth, crate::grid::MIN_DEPTH);
        s.set_depth_clamped(24);
        assert_eq!(s.depth, 24);
        s.set_depth_clamped(9_999);
        assert_eq!(s.depth, crate::grid::MAX_DEPTH);
    }

    /// `c`/`v` log-factor adjustments scale, clamp, and snap.
    #[test]
    fn adjust_log_factor_clamps() {
        let mut s = ViewSettings::default();
        s.adjust_log_factor(true);
        assert!((s.log_factor - 1.725).abs() < 1e-3);
        while s.log_factor > 0.0 {
            s.adjust_log_factor(false);
        }
        assert_eq!(s.log_factor, 0.0, "0 = fully linear, pinned at the floor");
        s.adjust_log_factor(true);
        assert!((s.log_factor - 0.05).abs() < 1e-9, "steps off exact zero");
        while s.log_factor < 8.0 {
            s.adjust_log_factor(true);
        }
        assert_eq!(s.log_factor, 8.0, "pinned at the ceiling");
    }

    #[test]
    fn waveform_strip_spans_x_at_wave_y() {
        let mut f = Frame::default();
        f.mono[0] = 1.0;
        f.mono[WINDOW_FRAMES - 1] = -0.5;
        let s = ViewSettings::default();
        let mut out = Vec::new();
        append_waveform(&f, &s, &mut out);
        // 2 verts per point (quad strip expansion).
        assert_eq!(out.len(), 2 * WINDOW_FRAMES);
        // First point: +normal then −normal around y = WAVE_Y + 2.
        approx((out[0].pos[0] + out[1].pos[0]) / 2.0, -WAVE_SPAN);
        approx(out[0].pos[2], 0.0);
        approx((out[0].pos[1] + out[1].pos[1]) / 2.0, WAVE_Y + 2.0);
        // The pair is strip_width apart, perpendicular to the tangent
        // (here near-vertical tangent → near-horizontal spread).
        let dx = out[0].pos[0] - out[1].pos[0];
        let dy = out[0].pos[1] - out[1].pos[1];
        approx((dx * dx + dy * dy).sqrt(), s.strip_width);
        // Last point: centered at y = WAVE_Y − 1.
        let n = 2 * (WINDOW_FRAMES - 1);
        approx((out[n].pos[1] + out[n + 1].pos[1]) / 2.0, WAVE_Y - 1.0);
        approx((out[n].pos[0] + out[n + 1].pos[0]) / 2.0, WAVE_SPAN);
        // Flat line (all zeros): perpendicular expansion is vertical.
        let f2 = Frame::default();
        out.clear();
        append_waveform(&f2, &s, &mut out);
        for v in &out {
            approx(v.pos[1], WAVE_Y + if v.pos[1] >= WAVE_Y { s.strip_width / 2.0 } else { -s.strip_width / 2.0 });
        }
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
    fn strip_width_key_steps_and_clamps() {
        let mut s = ViewSettings::default();
        for _ in 0..100 {
            s.scale_strip_width(true);
        }
        assert_eq!(s.strip_width, 0.1);
        for _ in 0..200 {
            s.scale_strip_width(false);
        }
        assert_eq!(s.strip_width, 0.001);
    }

    #[test]
    fn liss_delay_cycles_halves_and_wraps() {
        let mut s = ViewSettings::default();
        assert_eq!(s.liss_delay, WINDOW_FRAMES / 2);
        s.cycle_liss_delay();
        assert_eq!(s.liss_delay, 256);
        s.cycle_liss_delay();
        assert_eq!(s.liss_delay, 128);
        s.cycle_liss_delay();
        assert_eq!(s.liss_delay, 64);
        s.cycle_liss_delay();
        assert_eq!(s.liss_delay, WINDOW_FRAMES / 2, "wraps back to 512");
        // From a weird value it just halves down to 64.
        s.liss_delay = 300;
        s.cycle_liss_delay();
        assert_eq!(s.liss_delay, 150);
        s.cycle_liss_delay();
        assert_eq!(s.liss_delay, 75);
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
        assert_eq!(out.len(), 2 * WINDOW_FRAMES);
        // Quad pair is centered on the figure point: (y,z) mean.
        approx(out[0].pos[0], LISS_X); // plane constant
        approx((out[0].pos[1] + out[1].pos[1]) / 2.0, LISS_SCALE * f.right[0]); // figure-Y = R
        approx((out[0].pos[2] + out[1].pos[2]) / 2.0, LISS_SCALE * f.left[0]); // figure-X = L
        // pseudo: mono vs delayed copy (same y for all slices in delta 0)
        f.mono_like = true;
        out.clear();
        append_lissajous(&f, &s, &mut out);
        // pseudo: figure-Y = mono[(i+delay)], figure-X = mono[i]
        approx((out[0].pos[1] + out[1].pos[1]) / 2.0, LISS_SCALE * f.mono[LISS_DELAY]);
        approx((out[0].pos[2] + out[1].pos[2]) / 2.0, LISS_SCALE * f.mono[0]);
        // A different delay flows through (`y` cycle state).
        let mut s2 = ViewSettings::default();
        s2.cycle_liss_delay(); // delay = 256
        out.clear();
        append_lissajous(&f, &s2, &mut out);
        approx((out[0].pos[2] + out[1].pos[2]) / 2.0, LISS_SCALE * f.mono[0]);
        approx(
            (out[0].pos[1] + out[1].pos[1]) / 2.0,
            LISS_SCALE * f.mono[256],
        );
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
