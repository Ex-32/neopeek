//! View settings and the waveform/Lissajous line-strip builders
//! (PLAN.md §4.1 bottom half; constants from `docs/research-sndpeek.md`).
//!
//! Pure CPU geometry — unit-tested headlessly. Builders append into a
//! caller-provided buffer that the renderer reuses across frames (no
//! per-frame allocation on the hot path).

use std::time::Duration;

use neopeek_dsp::{Frame, DEFAULT_FFT_SIZE, FFT_SIZES, WINDOW_FRAMES};
use crate::grid::{DEFAULT_DEPTH, MAX_DEPTH, MIN_DEPTH};

/// Default vantage constants come from `crate::camera`; view constants here.
pub const WAVE_Y: f32 = 0.7;

/// Top of the centroid cursor's vertical extent (world y). Just above the
/// default-gain typical waterfall peak (≈ 1.73 at gain 0.35); `=` zoom and
/// auto-scale can push content past it — the cursor marks the axis, it is
/// not a ceiling for the spectrum.
pub const CURSOR_TOP_Y: f32 = 2.0;

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
/// Lissajous placement (coplanar with the waveform, `z = 0`): right of
/// the scene center, below the waveform strip's silent line
/// (`WAVE_Y = 0.7`). At full amplitude the figure is a ±0.45 square
/// around the center (x ≈ 0.25…1.15, y ≈ −0.23…0.68) — unlike the old
/// `x = 1.2` edge-on plane it faces the default camera, it clears the
/// waveform strip for every input, and at LOS/typical amplitudes (the
/// figure points at the square center) it clears the waterfall's silent
/// floor line (y = 0) too; only near-full-scale stereo corners graze
/// the floor.
///
/// Why not farther right: on the waveform's plane (z = 0) the default
/// vantage's frustum is the near edge — visible y spans ≈ −0.24…1.81
/// and the x reach tightens with height; "below the wave strip and
/// fully on-screen at 800×600" pins the center window to
/// y ≈ 0.21…0.24, x ≤ ≈0.75. (The waveform's own ±1.8 ends already
/// project past the 4:3 frustum.) Pinned by tests through the real
/// camera projection — both 4:3 and 16:9, full amplitude.
pub const LISS_CENTER_X: f32 = 0.7;
pub const LISS_CENTER_Y: f32 = 0.225;
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
    /// `4`: feature-cursor overlay (sndpeek feature-overlay parity) —
    /// the vertical spectral-centroid line on the waterfall and the
    /// horizontal RMS level line on the waveform strip. Default **on**:
    /// sndpeek drew the overlay on every frame unless `4` disabled it
    /// (research doc: the overlay is one of the four things drawn on
    /// every frame). Color comes from the theme (`cursor` field).
    pub cursors_on: bool,
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
    /// `o`/`n` + `--fft`: FFT length on [`neopeek_dsp::FFT_SIZES`]
    /// (default [`neopeek_dsp::DEFAULT_FFT_SIZE`]). Changing it rebuilds
    /// the renderer grid and restarts the DSP feature smoothing (new
    /// spectrum geometry; the window stays 1024).
    pub fft_size: usize,
    /// `-`/`=`: spectrum frequency scale (default 1.0).
    pub freq_scale: f32,
    /// `a`: content-adaptive waterfall gain (default off). While on, the
    /// renderer computes [`crate::autoscale::AutoScale::gain`] per frame
    /// from the drained slices so the tallest recent content sits at
    /// ~0.8 of the view height (fast attack, slow release); `-`/`=` still
    /// zoom relative to the adaptive level. Off → the uniform gain snaps
    /// to [`crate::views::DEFAULT_WATERFALL_GAIN`] immediately.
    pub auto_scale: bool,
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
    /// Idle pause: the renderer stops redrawing once nothing has been
    /// audible for this long (silent monitor-stream frames count as
    /// idle; input and resumed audio wake it). CLI-only knob
    /// (`--idle-pause <SECONDS>`, no runtime key); 0 = disabled. Default
    /// [`crate::idle::DEFAULT_IDLE_PAUSE_SECS`]. Owned here so the `p`
    /// dump reports the live value. Consumed by the binary's
    /// `IdleGate` wiring (see `crate::idle`).
    pub idle_pause: Duration,
}

impl Default for ViewSettings {
    fn default() -> Self {
        Self {
            waveform_on: true,
            lissajous_on: true,
            waterfall_on: true,
            cursors_on: true,
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
            fft_size: neopeek_dsp::DEFAULT_FFT_SIZE,
            freq_scale: 1.0,
            auto_scale: false,
            wave_scale: 1.0,
            liss_scale: LISS_SCALE,
            strip_width: 0.004,
            liss_delay: LISS_DELAY,
            idle_pause: Duration::from_secs(crate::idle::DEFAULT_IDLE_PAUSE_SECS),
        }
    }
}

impl ViewSettings {
    /// `-`/`=`: spectrum freq scale ×0.99 / ×1.01, clamped.
    pub fn scale_spectrum(&mut self, up: bool) {
        self.freq_scale = (self.freq_scale * if up { 1.01 } else { 0.99 })
            .clamp(0.01, 100.0);
    }

    /// `a`: on = the renderer drives the waterfall gain from the audio
    /// content ([`crate::autoscale::AutoScale`]); off = the fixed
    /// [`crate::views::DEFAULT_WATERFALL_GAIN`], restored immediately.
    pub fn toggle_auto_scale(&mut self) {
        self.auto_scale = !self.auto_scale;
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

    /// `o`/`n`: step one rung along the FFT ladder ([`FFT_SIZES`]), not
    /// a ×2 multiply — the display geometry (Hz per bin, texture columns)
    /// jumps visibly per rung. Clamped at both ends rather than wrapping:
    /// a silent wrap 8192 → 512 would flip the axis geometry twice over
    /// while the key keeps repeating. A no-op when pinned at a ladder end
    /// *or* when the value is not on the ladder (defensive; `--fft`
    /// validation is the front line).
    pub fn adjust_fft_size(&mut self, up: bool) {
        let Some(idx) = FFT_SIZES.iter().position(|&s| s == self.fft_size) else {
            return;
        };
        let next = if up { idx + 1 } else { idx.saturating_sub(1) };
        self.fft_size = FFT_SIZES[next.min(FFT_SIZES.len() - 1)];
    }

    /// Sets the FFT size from a user request (CLI flag): the value was
    /// validated against the ladder at parse time, so it is taken
    /// verbatim; a defensive off-ladder request falls back to the default.
    pub fn set_fft_size_exact(&mut self, size: usize) {
        self.fft_size = if FFT_SIZES.contains(&size) {
            size
        } else {
            DEFAULT_FFT_SIZE
        };
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
    /// `4`: feature-cursor overlay toggle.
    pub fn toggle_cursors(&mut self) {
        self.cursors_on = !self.cursors_on;
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

/// Append the Lissajous figure coplanar with the waveform — the same
/// x-y plane (`z = 0`; the figure faces the default camera, previously it
/// stood on the edge-on plane `x = 1.2` and read as flat):
/// - figure-X world x = [`LISS_CENTER_X`] + `liss_scale · L`,
/// - figure-Y world y = [`LISS_CENTER_Y`] + `liss_scale · R`,
/// - true stereo: figure-X = L, figure-Y = R;
/// - mono-ish source (`Frame::mono_like`): pseudo-Lissajous, mono vs its
///   `liss_delay`-sample delayed copy (`y` cycles the delay; `Y` is the
///   snap-side camera view, not this).
///
/// Closed figure → wrapped tangents; expanded by `strip_width` (the
/// normal now lies in the x-y plane, so the strip stays in-plane).
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
        (
            LISS_CENTER_X + s.liss_scale * u, // figure-X → world x
            LISS_CENTER_Y + s.liss_scale * v, // figure-Y → world y
        )
    };
    for i in 0..WINDOW_FRAMES {
        let (xu, yv) = point(i);
        let (xp, yp) = point((i + WINDOW_FRAMES - 1) % WINDOW_FRAMES);
        let (xn, yn) = point((i + 1) % WINDOW_FRAMES);
        let (tx, ty) = (xn - xp, yn - yp);
        let len = (tx * tx + ty * ty).sqrt().max(1e-9);
        let (nx, ny) = (-ty / len, tx / len);
        out.push(LineVertex { pos: [xu + nx * hw, yv + ny * hw, 0.0] });
        out.push(LineVertex { pos: [xu - nx * hw, yv - ny * hw, 0.0] });
    }
}

/// One straight strip segment `a` → `b` (2 points → 4 verts), expanded
/// perpendicular to the tangent by [`ViewSettings::strip_width`] exactly
/// like the curved strips.
fn append_segment(a: [f32; 3], b: [f32; 3], s: &ViewSettings, out: &mut Vec<LineVertex>) {
    let hw = s.strip_width * 0.5;
    let (tx, ty) = (b[0] - a[0], b[1] - a[1]);
    let len = (tx * tx + ty * ty).sqrt().max(1e-9);
    let (nx, ny) = (-ty / len, tx / len);
    out.push(LineVertex { pos: [a[0] + nx * hw, a[1] + ny * hw, a[2]] });
    out.push(LineVertex { pos: [a[0] - nx * hw, a[1] - ny * hw, a[2]] });
    out.push(LineVertex { pos: [b[0] + nx * hw, b[1] + ny * hw, b[2]] });
    out.push(LineVertex { pos: [b[0] - nx * hw, b[1] - ny * hw, b[2]] });
}

/// Append the spectral-centroid cursor (sndpeek's feature overlay, key
/// `4`): a vertical line at the centroid bin's x, mapped through the same
/// log-spaced axis as the waterfall grid
/// ([`crate::grid::normalized_position_at`] — one bin position, no
/// per-frame axis allocation), so the line always stands over the bin it
/// marks whatever the runtime FFT size (`o`/`n`) or log factor (`c`/`v`).
///
/// Extent/placement: y spans `0…[`CURSOR_TOP_Y`]` (the spectrum's floor
/// up to just past the default-gain typical peak), matching sndpeek's
/// full-height red line. It stands on the newest *history* slice plane —
/// one [`ViewSettings::spacing`] step behind the arriving plane, but
/// never in front of `z = −spacing` even when `j`/`k` push the grid
/// toward the camera — because the Lissajous figure is coplanar with the
/// arriving plane (`z = z_offset`, x ≈ 0.25…1.15): the offset keeps the
/// cursor out of the figure's plane entirely, and it rides the
/// waterfall's `z_offset`/`spacing` keys with the rest of the grid.
///
/// The centroid arrives in bins ([`Frame::centroid`], 4-frame smoothed);
/// fractional values round to the nearest bin, out-of-range values clamp
/// into the axis. Skipped when the frame carries no spectrum
/// (`bin_count < 2` — the axis mapping divides by `bins − 1`).
pub fn append_centroid_cursor(frame: &Frame, s: &ViewSettings, out: &mut Vec<LineVertex>) {
    if !s.cursors_on {
        return;
    }
    let bins = frame.bin_count as usize;
    if bins < 2 {
        return;
    }
    let bin = (frame.centroid.round() as i64).clamp(0, bins as i64 - 1) as usize;
    let x = crate::grid::x_from_normalized(crate::grid::normalized_position_at(
        s.log_factor, bins, bin,
    ));
    // z_offset rides along, but never past the Lissajous plane (z = 0).
    let z = (s.z_offset - s.spacing).min(-s.spacing);
    append_segment([x, 0.0, z], [x, CURSOR_TOP_Y, z], s, out);
}

/// Append the RMS level cursor (sndpeek's feature overlay, key `4`): a
/// horizontal hairline across the waveform strip at the window's RMS
/// level, `y = WAVE_Y + wave_scale · 2 · rms` — the same vertical mapping
/// the waveform samples use, so the line tracks the strip's `_`/`+`
/// scale keys and always sits inside the waveform's own amplitude range.
///
/// Channel combination: the **average** of the L/R window RMS — the same
/// value the HUD feature panel and `--print` report
/// (`(rms[0] + rms[1]) / 2`), and sndpeek's own stereo→mono averaging in
/// the callback. Clamped to 0…1 (full-scale audio) so the line tops out
/// at the waveform's maximum; it rides at or above the strip's silent
/// line (y = 0.7 > the Lissajous region's top ≈ 0.68) for every level.
pub fn append_rms_cursor(frame: &Frame, s: &ViewSettings, out: &mut Vec<LineVertex>) {
    if !s.cursors_on {
        return;
    }
    let rms = (frame.rms[0] + frame.rms[1]) * 0.5;
    let y = WAVE_Y + s.wave_scale * 2.0 * rms.clamp(0.0, 1.0);
    append_segment([-WAVE_SPAN, y, 0.0], [WAVE_SPAN, y, 0.0], s, out);
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
        assert_eq!(s.fft_size, DEFAULT_FFT_SIZE);
        assert!((s.liss_scale - 0.45).abs() < 1e-9);
        assert!((s.z_offset - 0.0).abs() < 1e-9);
        assert!(s.waveform_on && s.lissajous_on && s.waterfall_on);
        assert!(s.cursors_on, "sndpeek drew the feature overlay by default");
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

    /// `o`/`n` step one rung along the ladder and clamp at both ends;
    /// no multiplication, no wrap.
    #[test]
    fn fft_size_steps_the_ladder_and_clamps() {
        let mut s = ViewSettings::default();
        assert_eq!(s.fft_size, DEFAULT_FFT_SIZE);
        s.adjust_fft_size(true);
        assert_eq!(s.fft_size, 4096);
        s.adjust_fft_size(false);
        assert_eq!(s.fft_size, DEFAULT_FFT_SIZE);
        s.adjust_fft_size(false);
        assert_eq!(s.fft_size, 1024);
        while s.fft_size > 512 {
            s.adjust_fft_size(false);
        }
        assert_eq!(s.fft_size, 512, "512 is the floor");
        s.adjust_fft_size(false);
        assert_eq!(s.fft_size, 512, "pinned at the floor");
        while s.fft_size < 8192 {
            s.adjust_fft_size(true);
        }
        assert_eq!(s.fft_size, 8192, "8192 is the ceiling");
        s.adjust_fft_size(true);
        assert_eq!(s.fft_size, 8192, "pinned at the ceiling");
    }

    /// `--fft`: the CLI value was validated at parse time and is taken
    /// verbatim; a defensive off-ladder value falls back to the default.
    #[test]
    fn set_fft_size_exact_takes_ladder_values() {
        for size in FFT_SIZES {
            let mut s = ViewSettings::default();
            s.set_fft_size_exact(size);
            assert_eq!(s.fft_size, size);
        }
        let mut s = ViewSettings::default();
        s.set_fft_size_exact(3000);
        assert_eq!(s.fft_size, DEFAULT_FFT_SIZE);
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
        // Quad pair is centered on the figure point: (x,y) mean.
        approx((out[0].pos[0] + out[1].pos[0]) / 2.0, LISS_CENTER_X + LISS_SCALE * f.left[0]); // figure-X = L → world x
        approx((out[0].pos[1] + out[1].pos[1]) / 2.0, LISS_CENTER_Y + LISS_SCALE * f.right[0]); // figure-Y = R → world y
        // pseudo: mono vs delayed copy (same y for all slices in delta 0)
        f.mono_like = true;
        out.clear();
        append_lissajous(&f, &s, &mut out);
        // pseudo: figure-Y = mono[(i+delay)], figure-X = mono[i]
        approx((out[0].pos[0] + out[1].pos[0]) / 2.0, LISS_CENTER_X + LISS_SCALE * f.mono[0]);
        approx((out[0].pos[1] + out[1].pos[1]) / 2.0, LISS_CENTER_Y + LISS_SCALE * f.mono[LISS_DELAY]);
        // A different delay flows through (`y` cycle state).
        let mut s2 = ViewSettings::default();
        s2.cycle_liss_delay(); // delay = 256
        out.clear();
        append_lissajous(&f, &s2, &mut out);
        approx((out[0].pos[0] + out[1].pos[0]) / 2.0, LISS_CENTER_X + LISS_SCALE * f.mono[0]);
        approx(
            (out[0].pos[1] + out[1].pos[1]) / 2.0,
            LISS_CENTER_Y + LISS_SCALE * f.mono[256],
        );
    }

    /// The figure is coplanar with the waveform: every vertex sits at
    /// z = 0 and inside the reserved region (center ± scale, plus the
    /// half strip width), face-on to the default camera.
    #[test]
    fn lissajous_is_coplanar_and_inside_reserved_region() {
        // Full-amplitude figure: all four corners reachable.
        let mut f = Frame { mono_like: false, ..Frame::default() };
        for (i, &(l, r)) in [(1.0f32, 1.0), (1.0, -1.0), (-1.0, -1.0), (-1.0, 1.0)]
            .iter()
            .enumerate()
        {
            f.left[i] = l;
            f.right[i] = r;
        }
        let s = ViewSettings::default();
        let mut out = Vec::new();
        append_lissajous(&f, &s, &mut out);
        // z ≡ 0 exactly ('coplanar with the audio wave').
        for v in &out {
            approx(v.pos[2], 0.0);
        }
        // Bounded by the reserved square: center ± (scale + half width).
        let reach = LISS_SCALE + s.strip_width * 0.5 + 1e-4;
        let (xmin, xmax) = (LISS_CENTER_X - reach, LISS_CENTER_X + reach);
        let (ymin, ymax) = (LISS_CENTER_Y - reach, LISS_CENTER_Y + reach);
        for v in &out {
            assert!(v.pos[0] >= xmin && v.pos[0] <= xmax, "x {} out of {:?}", v.pos[0], v.pos);
            assert!(v.pos[1] >= ymin && v.pos[1] <= ymax, "y {} out of {:?}", v.pos[1], v.pos);
        }
        // Clear of the waveform strip: the whole region sits below the
        // strip's silent centerline by more than the strip can reach.
        assert!(
            ymax < WAVE_Y - s.strip_width,
            "liss region top {} must stay under the waveform strip at y = {WAVE_Y}",
            ymax
        );
        // …and the quad expansion stays in-plane perpendicular: each pair
        // is strip_width apart, both points at z = 0. (Fully flat figure
        // points — zero neighbor change, the pre-existing 1e-9 fallback —
        // degenerate to a single point; the corner indices below have
        // real tangents, so the width contract is checked there.)
        for pair in out.chunks(2).take(4) {
            let d = (pair[0].pos[0] - pair[1].pos[0],
                     pair[0].pos[1] - pair[1].pos[1]);
            approx((d.0 * d.0 + d.1 * d.1).sqrt(), s.strip_width);
        }
        assert_eq!(out[0].pos[2], 0.0);

        // Every figure point must project on-screen: the placement was
        // chosen so the FULL-amplitude figure fits the default 800×600
        // (4:3) vantage and wide windows alike — pinned at both, plus
        // at 640×480 (a narrower 4:3).
        for size in [(800u32, 600u32), (1920, 1080), (640, 480)] {
            let cam = Camera::default_vantage(size.0, size.1);
            let vp = cam.view_proj();
            for v in &out {
                let ndc = crate::camera::project(vp, v.pos);
                assert!(ndc[0].abs() < 1.0 && ndc[1].abs() < 1.0 && ndc[2] < 1.0,
                    "{}×{} clips vertex {:?}", size.0, size.1, v.pos);
            }
        }
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

    /// Centroid cursor: x maps through the grid's log-spaced axis for the
    /// frame's bin count; y spans 0…CURSOR_TOP_Y; it stands one spacing
    /// behind the arriving plane and never enters the Lissajous plane.
    #[test]
    fn centroid_cursor_sits_at_the_mapped_bin_x() {
        let mut f = Frame { bin_count: 1024, ..Frame::default() }; // the default 2048-point FFT
        f.centroid = 0.0;
        let s = ViewSettings::default();
        let mut out = Vec::new();
        append_centroid_cursor(&f, &s, &mut out);
        assert_eq!(out.len(), 4, "one straight segment = 2 points = 4 verts");
        // Bin 0 → the axis' left edge; vertical extent 0…CURSOR_TOP_Y.
        approx((out[0].pos[0] + out[1].pos[0]) / 2.0, -1.8);
        approx((out[0].pos[1] + out[1].pos[1]) / 2.0, 0.0);
        approx((out[2].pos[1] + out[3].pos[1]) / 2.0, CURSOR_TOP_Y);
        // One spacing behind the arriving plane (z = 0 at the default
        // z_offset) — off the Lissajous plane entirely.
        approx(out[0].pos[2], -0.12);

        // A mid-spectrum centroid maps exactly like the grid axis.
        f.centroid = 331.0;
        out.clear();
        append_centroid_cursor(&f, &s, &mut out);
        let want = crate::grid::x_from_normalized(
            crate::grid::normalized_positions(s.log_factor, 1024)[331],
        );
        approx((out[0].pos[0] + out[1].pos[0]) / 2.0, want);

        // Fractional (4-frame smoothed) centroids round to the nearest bin.
        f.centroid = 331.49;
        out.clear();
        append_centroid_cursor(&f, &s, &mut out);
        approx((out[0].pos[0] + out[1].pos[0]) / 2.0, want);

        // Runtime FFT size: the mapping follows the frame's bin count
        // (here 512 bins = the 1024-point FFT rung).
        f.centroid = 255.0;
        f.bin_count = 512;
        out.clear();
        append_centroid_cursor(&f, &s, &mut out);
        let want = crate::grid::x_from_normalized(
            crate::grid::normalized_positions(s.log_factor, 512)[255],
        );
        approx((out[0].pos[0] + out[1].pos[0]) / 2.0, want);

        // Out-of-range centroids clamp into the axis instead of wild x.
        f.centroid = 9_999.0;
        out.clear();
        append_centroid_cursor(&f, &s, &mut out);
        approx((out[0].pos[0] + out[1].pos[0]) / 2.0, 1.8);

        // z_offset rides along, but never past the Lissajous plane (z = 0
        // at any spacing/z_offset combination the keys can produce).
        let mut fwd = ViewSettings::default();
        fwd.move_z_offset(false); // +0.1 toward the camera
        out.clear();
        append_centroid_cursor(&f, &fwd, &mut out);
        approx(out[0].pos[2], -0.12);
        let mut back = ViewSettings::default();
        back.move_z_offset(true); // −0.1
        out.clear();
        append_centroid_cursor(&f, &back, &mut out);
        approx(out[0].pos[2], -0.22);

        // No spectrum this frame (bin_count < 2) → no cursor at all.
        let silent = Frame::default();
        out.clear();
        append_centroid_cursor(&silent, &s, &mut out);
        assert!(out.is_empty());
    }

    /// RMS cursor: a horizontal line across the strip's x extent at the
    /// waveform mapping of the averaged L/R RMS.
    #[test]
    fn rms_cursor_rides_the_waveform_mapping() {
        let mut f = Frame { rms: [0.5, 0.25], ..Frame::default() }; // avg 0.375
        let s = ViewSettings::default();
        let mut out = Vec::new();
        append_rms_cursor(&f, &s, &mut out);
        assert_eq!(out.len(), 4);
        // Spans the waveform strip's x extent, on the waveform's plane.
        approx((out[0].pos[0] + out[1].pos[0]) / 2.0, -WAVE_SPAN);
        approx((out[2].pos[0] + out[3].pos[0]) / 2.0, WAVE_SPAN);
        approx(out[0].pos[2], 0.0);
        // y = WAVE_Y + wave_scale·2·avg(rms).
        approx((out[0].pos[1] + out[1].pos[1]) / 2.0, WAVE_Y + 2.0 * 0.375);
        // Above the Lissajous region's top even at full-scale audio.
        f.rms = [1.0, 1.0];
        out.clear();
        append_rms_cursor(&f, &s, &mut out);
        approx((out[0].pos[1] + out[1].pos[1]) / 2.0, WAVE_Y + 2.0);
        // wave_scale flows through.
        let mut s2 = ViewSettings::default();
        s2.scale_waveform(true);
        out.clear();
        append_rms_cursor(&f, &s2, &mut out);
        approx((out[0].pos[1] + out[1].pos[1]) / 2.0, WAVE_Y + s2.wave_scale * 2.0);
    }

    /// Disabled (`4` off) → no cursor vertices at all.
    #[test]
    fn cursors_disabled_produce_no_vertices() {
        let f = Frame { bin_count: 1024, centroid: 100.0, rms: [0.3, 0.2], ..Frame::default() };
        let mut s = ViewSettings::default();
        let mut out = Vec::new();
        append_centroid_cursor(&f, &s, &mut out);
        append_rms_cursor(&f, &s, &mut out);
        assert_eq!(out.len(), 8, "two straight segments");
        s.toggle_cursors();
        assert!(!s.cursors_on);
        out.clear();
        append_centroid_cursor(&f, &s, &mut out);
        append_rms_cursor(&f, &s, &mut out);
        assert!(out.is_empty(), "disabled → no vertices");
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
        // `4`: feature cursors default on, flip off and back.
        s.toggle_cursors();
        assert!(!s.cursors_on);
        s.toggle_cursors();
        assert!(s.cursors_on);
        // `a`: content-adaptive gain — default off, toggles back.
        assert!(!s.auto_scale);
        s.toggle_auto_scale();
        assert!(s.auto_scale);
        s.toggle_auto_scale();
        assert!(!s.auto_scale);
    }

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-5, "{a} ≠ {b}");
    }
}
