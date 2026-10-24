//! HUD (PLAN.md §4.4): meter math, feature-panel formatting, overlay
//! state, and `HudData` assembly — all headless and unit-tested.
//!
//! The wgpu/glyphon drawing code is isolated at the bottom of this module
//! (plan §9 risk note: a glyphon swap costs one module) and has no runtime
//! test here (no GPU/display in the build env).

use std::time::{SystemTime, UNIX_EPOCH};

use neopeek_dsp::{Frame, FFT_SIZE};

use crate::views::ViewSettings;

/// Meter dB window.
pub const METER_FLOOR_DB: f32 = -60.0;
pub const METER_CEIL_DB: f32 = 0.0;
/// Peak-hold duration; the theme's `meters.peak_hold_ms` is the
/// authoritative mirror (kept as the Ember default for tests).
pub const PEAK_HOLD_MS: u64 = 800;

/// Meter quad layout constants, physical pixels.
pub const HUD_MARGIN: f32 = 10.0;
pub const BAR_W: f32 = 150.0;
pub const BAR_H: f32 = 10.0;
pub const BAR_GAP: f32 = 18.0;

/// Wall-clock milliseconds (peak-hold expiry clock).
pub fn now_millis() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// Linear amplitude → dBFS, floored at the meter's bottom.
pub fn rms_to_db(rms: f32) -> f32 {
    (20.0 * rms.max(1e-6).log10()).clamp(METER_FLOOR_DB, METER_CEIL_DB)
}

/// dB value → bar fill fraction 0…1.
pub fn db_to_fill(db: f32) -> f32 {
    ((db - METER_FLOOR_DB) / (METER_CEIL_DB - METER_FLOOR_DB)).clamp(0.0, 1.0)
}

/// Convenience: linear amplitude → fill fraction.
pub fn rms_to_fill(rms: f32) -> f32 {
    db_to_fill(rms_to_db(rms))
}

/// Peak-hold with expiry (one state per channel).
#[derive(Debug, Clone, Copy)]
pub struct MeterState {
    held: [f32; 2],
    risen_ms: [u64; 2],
}

impl Default for MeterState {
    fn default() -> Self {
        Self {
            held: [0.0; 2],
            risen_ms: [0; 2],
        }
    }
}

impl MeterState {
    pub fn update(&mut self, peaks: [f32; 2], now_ms: u64, hold_ms: u64) {
        for (c, &peak) in peaks.iter().enumerate() {
            let expired = now_ms.saturating_sub(self.risen_ms[c]) >= hold_ms;
            if peak >= self.held[c] || expired {
                self.held[c] = peak;
                self.risen_ms[c] = now_ms;
            }
        }
    }

    /// Current held peaks.
    pub fn held(&self) -> [f32; 2] {
        self.held
    }
}

/// Bins → Hz at the negotiated rate (centroid/rolloff use bin units).
pub fn bin_to_hz(bins: f32, rate: u32) -> f64 {
    bins as f64 * (rate as f64) / (FFT_SIZE as f64)
}

/// The five panel features in fixed order (centroid, flux, RMS, 50%, 80%).
type FeatureVals = [f32; 5];

/// Raw per-frame feature values for the panel.
pub fn frame_features(frame: &Frame) -> FeatureVals {
    [
        frame.centroid,
        frame.flux,
        (frame.rms[0] + frame.rms[1]) / 2.0,
        frame.rolloff50,
        frame.rolloff80,
    ]
}

/// 4-frame running mean over the panel features (sndpeek-faithful
/// smoothing; PLAN.md §4.4). Cheap and allocation-free.
#[derive(Debug, Clone, Copy)]
pub struct FeatureSmooth {
    hist: [FeatureVals; 4],
    filled: usize,
    next: usize,
}

impl Default for FeatureSmooth {
    fn default() -> Self {
        Self {
            hist: [[0.0; 5]; 4],
            filled: 0,
            next: 0,
        }
    }
}

impl FeatureSmooth {
    pub fn push(&mut self, vals: FeatureVals) {
        self.hist[self.next] = vals;
        self.next = (self.next + 1) % 4;
        self.filled = (self.filled + 1).min(4);
    }

    /// Running mean of the held frames (0 until the first frame arrives).
    pub fn mean(&self) -> FeatureVals {
        if self.filled == 0 {
            return [0.0; 5];
        }
        let mut out = [0.0; 5];
        for h in &self.hist[..self.filled] {
            for (o, v) in out.iter_mut().zip(h) {
                *o += v;
            }
        }
        out.map(|v| v / self.filled as f32)
    }
}

/// The five feature-panel lines from smoothed values.
pub fn fmt_features(vals: FeatureVals, rate: u32) -> [String; 5] {
    [
        format!("centroid = {:.1} Hz", bin_to_hz(vals[0], rate)),
        format!("flux = {:.3}", vals[1]),
        format!("RMS = {:.4}", vals[2]),
        format!("50% rolloff = {:.1} Hz", bin_to_hz(vals[3], rate)),
        format!("80% rolloff = {:.1} Hz", bin_to_hz(vals[4], rate)),
    ]
}

/// Center overlay strings from the two overlay flags (frozen first,
/// muted second; both shown when both active).
pub fn center_overlay(frozen: bool, muted: bool) -> Vec<String> {
    let mut lines = Vec::with_capacity(2);
    if frozen {
        lines.push("frozen… (press f to resume)".to_owned());
    }
    if muted {
        lines.push("muted… (press m to unmute)".to_owned());
    }
    lines
}

/// Text-panel layout offsets, logical pixels (scaled per panel below).
/// 170 ≈ the widest perf-column line at the default 14 px font;
/// 120/30 center the overlay; bottom offsets derive from the line
/// height (`font_px · 1.43`, see [`hud_text_anchors`]).
const PERF_COL_W: f32 = 170.0;
const OVERLAY_HALF_W: f32 = 120.0;
const OVERLAY_TOP: f32 = 30.0;
const BOTTOM_PAD: f32 = 4.0;
/// Gap between the device line and the hints line(s).
const BOTTOM_LINE_GAP: f32 = 2.0;
/// Keybind hints wrap to three lines so every bind fits the surface
/// width — the two-line version dropped the newest binds (c/v log
/// factor, PageUp/PageDown depth).
pub const HINT_LINES: usize = 3;

/// Top-left anchor positions of the five HUD text panels, in **physical**
/// pixels, computed from the current surface size, DPI scale and font
/// metrics. Order: `[0]` feature panel, `[1]` perf column (top-right),
/// `[2]` device stats (bottom), `[3]` overlay (center), `[4]` keybind
/// hints (bottom).
///
/// All offsets are logical pixels and are multiplied by `scale` here;
/// the surface `width`/`height` are already physical, so they combine
/// with the *scaled* offsets — never the other way around. (The previous
/// inline math did `(height - 46) * scale`, double-scaling the physical
/// height: on a scale ≠ 1 monitor every width/height-anchored panel slid
/// off the surface — bottom line, top-right column, hints — while the
/// fixed-offset feature panel stayed visible. That is the reported
/// "HUD text vanishes after moving to another monitor".)
///
/// Glyphon contract (0.12): `TextArea.left/top` are physical pixel
/// positions and the buffer's logical glyph coordinates are multiplied
/// by `TextArea.scale` (`cosmic_text::LayoutGlyph::physical`), so an
/// anchor is `physical_dim − logical_offset · scale`.
pub fn hud_text_anchors(
    width: u32,
    height: u32,
    scale: f32,
    font_px: f32,
    hint_lines: usize,
) -> [(f32, f32); 5] {
    let (w, h) = (width as f32, height as f32);
    // Line height matches the buffers' `Metrics` (hud_render).
    let line_h = (font_px * 1.43).max(1.0);
    // Bottom stack: hints line(s) closest to the edge, the device line
    // above them. `hint_lines = 1` reproduces the old offsets (46/24).
    let hints_h = hint_lines as f32 * line_h;
    let device_top = h - (BOTTOM_PAD + hints_h + BOTTOM_LINE_GAP + line_h) * scale;
    let hints_top = h - (BOTTOM_PAD + hints_h) * scale;
    let feature_top = HUD_MARGIN + 2.0 * BAR_GAP + 6.0;
    [
        (HUD_MARGIN * scale, feature_top * scale),
        // Tiny windows × large scales can push the right-anchored column
        // past the left edge; clamp so the text stays visible. The device
        // line gets the same treatment in height: three hint lines (46…
        // 86 logical px of bottom stack) can otherwise pull it above the
        // top edge in the same degenerate combos.
        ((w - PERF_COL_W * scale).max(HUD_MARGIN * scale), HUD_MARGIN * scale),
        (HUD_MARGIN * scale, device_top.max(HUD_MARGIN * scale)),
        (
            (w / 2.0 - OVERLAY_HALF_W * scale).max(0.0),
            (h / 2.0 - OVERLAY_TOP * scale).max(0.0),
        ),
        (HUD_MARGIN * scale, hints_top),
    ]
}

/// One laid-out meter rect, physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeterRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Meter quad geometry, per channel `[background, rms fill, peak tick]`
/// (bar constants above; callers multiply by the DPI scale).
pub fn meter_rects(rms_fill: [f32; 2], peak_fill: [f32; 2]) -> [MeterRect; 6] {
    let mut out: [MeterRect; 6] = [MeterRect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 }; 6];
    for c in 0..2 {
        let y = HUD_MARGIN + (c as f32) * BAR_GAP;
        out[3 * c] = MeterRect { x: HUD_MARGIN, y, w: BAR_W, h: BAR_H };
        out[3 * c + 1] = MeterRect {
            x: HUD_MARGIN,
            y,
            w: BAR_W * rms_fill[c].clamp(0.0, 1.0),
            h: BAR_H,
        };
        // White tick 2 px wide, tall enough to overhang the bar 2 px each
        // way; never past the bar's right edge.
        let tick_x = (HUD_MARGIN + BAR_W * peak_fill[c].clamp(0.0, 1.0))
            .min(HUD_MARGIN + BAR_W - 2.0);
        out[3 * c + 2] = MeterRect { x: tick_x, y: y - 2.0, w: 2.0, h: BAR_H + 4.0 };
    }
    out
}

/// Per-frame HUD snapshot assembled by the binary.
#[derive(Debug, Clone)]
pub struct HudData {
    // bottom line
    pub source_name: String,
    pub backend: String,
    pub rate: u32,
    pub fft_size: u32,
    pub window_frames: u32,
    pub hop_ms: f32,
    pub depth: usize,
    // right column
    pub fps: f32,
    pub frame_ms: f32,
    pub adapter: String,
    // meters
    pub rms_fill: [f32; 2],
    pub peak_fill: [f32; 2],
    pub rms_db: [f32; 2],
    // feature panel (top-left, under meters)
    pub feature_lines: [String; 5],
    // overlays (empty when idle)
    pub overlay: Vec<String>,
    // keybind-hint line inside the bottom panel
    pub show_hints: bool,
}

/// Everything the binary hands to [`HudData::assemble`] each frame.
pub struct HudSources<'a> {
    pub source_name: &'a str,
    pub backend: &'a str,
    pub adapter: &'a str,
    pub rate: u32,
    pub window_frames: u32,
    pub depth: usize,
    pub fps: f32,
    pub frame_ms: f32,
    /// Peak-hold window from the theme's `meters.peak_hold_ms` (M7).
    pub peak_hold_ms: u64,
    pub settings: &'a ViewSettings,
    pub latest: Option<&'a Frame>,
}

impl HudData {
    pub fn assemble(
        src: HudSources<'_>,
        meter_state: &mut MeterState,
        smooth: &mut FeatureSmooth,
    ) -> Self {
        let rate = src.rate;
        // Advance the peak-hold even while frozen/muted so the tick
        // visibly expires on the wall clock.
        meter_state.update(src.latest.map_or([0.0; 2], |f| f.peak), now_millis(), src.peak_hold_ms);
        if let Some(f) = src.latest {
            smooth.push(frame_features(f));
        }
        Self {
            source_name: src.source_name.to_owned(),
            backend: src.backend.to_owned(),
            adapter: src.adapter.to_owned(),
            rate,
            fft_size: FFT_SIZE as u32,
            window_frames: src.window_frames,
            hop_ms: src.window_frames as f32 / rate as f32 * 1000.0,
            depth: src.depth,
            fps: src.fps,
            frame_ms: src.frame_ms,
            rms_fill: src.latest.map_or([0.0; 2], |f| {
                [rms_to_fill(f.rms[0]), rms_to_fill(f.rms[1])]
            }),
            peak_fill: {
                let held = meter_state.held();
                src.latest.map_or([0.0; 2], |f| [
                    rms_to_fill(held[0].max(f.peak[0])),
                    rms_to_fill(held[1].max(f.peak[1])),
                ])
            },
            rms_db: src.latest.map_or([METER_FLOOR_DB; 2], |f| {
                [rms_to_db(f.rms[0]), rms_to_db(f.rms[1])]
            }),
            feature_lines: if src.latest.is_some() {
                fmt_features(smooth.mean(), rate)
            } else {
                std::array::from_fn(|_| String::new())
            },
            overlay: center_overlay(src.settings.frozen, src.settings.muted),
            show_hints: src.settings.show_hints,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::ViewSettings;

    /// Default HUD font (Ember metrics; hud_render starts at this too).
    const FONT_PX: f32 = 14.0;

    /// Regression for "HUD text vanishes after moving to another
    /// monitor": every panel anchor stays on the surface for a spread of
    /// DPI scales and window sizes. (The old math multiplied the
    /// already-physical `height`/`width` by `scale` again, pushing the
    /// device line at (height−46)·scale and the perf column at
    /// (width−170)·scale off the surface whenever scale ≠ 1.)
    #[test]
    fn anchors_stay_on_surface_across_scales_and_sizes() {
        for &(w, h) in &[(800, 600), (1600, 1200), (1280, 720), (300, 200)] {
            for scale in [0.75, 1.0, 1.25, 1.5, 2.0, 3.0] {
                for hint_lines in [0usize, 1, 2, 3] {
                    let anchors = hud_text_anchors(w, h, scale, FONT_PX, hint_lines);
                    for (i, &(x, y)) in anchors.iter().enumerate() {
                        assert!(
                            x >= 0.0 && x < w as f32 && y >= 0.0 && y < h as f32,
                            "panel {i} anchor ({x}, {y}) off-surface at {w}×{h}, scale {scale}, hint_lines {hint_lines}"
                        );
                    }
                    // The hints block must stack above the device line,
                    // leaving the device stats on-surface too (empty
                    // hint_lines = 0 anchors the block at the edge — no
                    // relation to the device line there).
                    // The device line must stack above the hints top — but
                    // only while the geometry is unclamped: in degenerate
                    // tiny-window × big-scale × many-hint-line combos the
                    // bottom stack alone can exceed the height and both
                    // anchors clamp to the top margin (see hud_text_anchors).
                    let line_h = FONT_PX * 1.43;
                    let unclamped_device_top =
                        h as f32 - (BOTTOM_PAD + (hint_lines + 1) as f32 * line_h + BOTTOM_LINE_GAP) * scale;
                    if hint_lines > 0 && unclamped_device_top >= HUD_MARGIN * scale {
                        assert!(
                            anchors[2].1 <= anchors[4].1,
                            "device line {} must stack above the hints top {} at {w}×{h}, scale {scale}, hint_lines {hint_lines}",
                            anchors[2].1,
                            anchors[4].1,
                        );
                    }
                }
            }
        }
    }

    /// Bottom panel x/y contract at scale 1 reproduces the tuned
    /// geometry: device line 46 px up, hints line 24 px up.
    #[test]
    fn anchors_match_tuned_defaults_at_scale_one() {
        let a = hud_text_anchors(800, 600, 1.0, FONT_PX, 1);
        approx(a[2].0, 10.0, 1e-3);
        approx(a[2].1, 554.0, 1e-1); // device line
        approx(a[4].1, 576.0, 1e-1); // hints line
        approx(a[0].1, 52.0, 1e-4); // feature panel
        approx(a[1].0, 630.0, 1e-4); // perf column
        approx(a[3].0, 280.0, 1e-4); // overlay
    }

    /// Scale change (the monitor-move event): positions track the new
    /// scale in the correct direction — the device line stays near the
    /// bottom edge as scale grows, instead of shooting past it.
    #[test]
    fn anchors_track_scale_change_direction() {
        let a1 = hud_text_anchors(800, 600, 1.0, FONT_PX, 1)[2].1;
        let a2 = hud_text_anchors(800, 600, 2.0, FONT_PX, 1)[2].1;
        // 1×: 600−46 = 554; 2×: 600−92 = 508 — closer to the edge, on it.
        approx(a1, 554.0, 1e-1);
        approx(a2, 508.0, 1e-1);
        assert!(a2 < a1 && a2 > 0.0);
        // The buggy formula gave (600−46)·2 = 1108 ≥ height 600.
    }

    /// With hints hidden (`hint_lines = 0`) the device line must anchor to
    /// the actual bottom margin — this is how `hud_text_anchors` is fed 0
    /// when `HudData.show_hints` is false, moving the stats line down
    /// instead of leaving it floating where the hidden hints block was.
    /// The bottom margin is the line height plus padding (the device line
    /// stacks directly above the bottom edge): h − (4 + 2 + line_h)·scale.
    #[test]
    fn device_line_anchors_to_bottom_when_hints_hidden() {
        let line_h = FONT_PX * 1.43;
        let expected = |h: f32, scale: f32| h - (BOTTOM_PAD + BOTTOM_LINE_GAP + line_h) * scale;
        for &(w, h) in &[(800u32, 600u32), (1600, 1200), (1280, 720), (300, 200)] {
            for scale in [1.0f32, 1.25, 1.5, 2.0] {
                let a = hud_text_anchors(w, h, scale, FONT_PX, 0);
                let want = expected(h as f32, scale);
                // The bottom margin must land above the clamp floor
                // (HUD_MARGIN·scale) for these non-degenerate sizes, and
                // the device line sits there, not clamped away from it.
                assert!(want > HUD_MARGIN * scale);
                approx(a[2].1, want, 1e-2);
            }
        }
        // Regression: hints shown (3 lines) stacks higher; hidden (0)
        // drops the device line strictly lower (closer to the bottom).
        let shown = hud_text_anchors(800, 600, 1.0, FONT_PX, HINT_LINES)[2].1;
        let hidden = hud_text_anchors(800, 600, 1.0, FONT_PX, 0)[2].1;
        assert!(hidden > shown, "hidden device line {} should sit lower (larger y) than with hints {}", hidden, shown);
    }

    #[test]
    fn db_math() {
        assert_eq!(rms_to_db(1.0), 0.0);
        assert_eq!(rms_to_db(0.1), -20.0, "0.1 amp = −20 dB");
        approx(rms_to_db(1e-7), METER_FLOOR_DB, 0.01);
        assert_eq!(rms_to_db(0.0), METER_FLOOR_DB);

        assert_eq!(db_to_fill(0.0), 1.0);
        assert_eq!(db_to_fill(METER_FLOOR_DB), 0.0);
        approx(db_to_fill(-30.0), 0.5, 0.001);
    }

    #[test]
    fn rms_fill_chain() {
        // 0.001 linear → −60 dB → 0.0 fill
        approx(rms_to_fill(0.001), 0.0, 0.001);
        // 0.1 → −20 dB → 40/60 fill
        approx(rms_to_fill(0.1), 2.0 / 3.0, 0.001);
        // 1.0 → 1.0
        approx(rms_to_fill(1.0), 1.0, 0.001);
    }

    #[test]
    fn peak_hold_logic() {
        let mut m = MeterState::default();
        m.update([0.5, 0.0], 100, PEAK_HOLD_MS);
        assert_eq!(m.held(), [0.5, 0.0]);
        // Higher peak on the right rises immediately.
        m.update([0.2, 0.9], 200, PEAK_HOLD_MS);
        assert_eq!(m.held(), [0.5, 0.9]);
        // Within the hold window, a lower peak does NOT pull the tick down.
        m.update([0.1, 0.1], 500, PEAK_HOLD_MS);
        assert_eq!(m.held(), [0.5, 0.9]);
        // Ferals beyond the hold window re-rise to the (lower) current value.
        m.update([0.1, 0.2], 100 + 900, PEAK_HOLD_MS);
        assert_eq!(m.held(), [0.1, 0.2]);
    }

    #[test]
    fn hz_conversion() {
        // Bin 100 at 48 kHz / FFT 2048 = 2343.75 Hz
        approx_f64(bin_to_hz(100.0, 48_000), 100.0 * 48_000.0 / 2048.0);
        approx_f64(bin_to_hz(0.0, 48_000), 0.0);
    }

    #[test]
    fn feature_panel_lines() {
        let f = Frame {
            centroid: 100.0,
            flux: 2.0,
            rms: [0.05, 0.05],
            rolloff50: 10.0,
            rolloff80: 20.0,
            ..Frame::default()
        };
        let lines = fmt_features(frame_features(&f), 48_000);
        assert_eq!(lines.len(), 5);
        assert!(lines[0].starts_with("centroid = 2343.8 Hz"), "{}", lines[0]);
        assert!(lines[1].starts_with("flux = 2.000"));
        assert!(lines[2].starts_with("RMS = 0.0500"));
        assert!(lines[3].starts_with("50% rolloff = 234.4 Hz"));
        assert!(lines[4].starts_with("80% rolloff = 468.8 Hz"));
    }

    #[test]
    fn overlay_machine() {
        assert!(center_overlay(false, false).is_empty());
        assert_eq!(
            center_overlay(true, false),
            vec!["frozen… (press f to resume)".to_owned()]
        );
        assert_eq!(center_overlay(true, true).len(), 2);
        assert_eq!(
            center_overlay(false, true),
            vec!["muted… (press m to unmute)".to_owned()]
        );
    }

    #[test]
    fn feature_smoothing_is_a_4_frame_mean() {
        let mut s = FeatureSmooth::default();
        assert_eq!(s.mean(), [0.0; 5]);
        s.push([1.0; 5]);
        assert_eq!(s.mean(), [1.0; 5]);
        s.push([3.0; 5]);
        s.push([3.0; 5]);
        // 1+3+3 over 3 filled frames.
        approx(s.mean()[0], 7.0 / 3.0, 1e-6);
        // Ring full: 1,3,3,3.
        s.push([3.0; 5]);
        approx(s.mean()[0], 2.5, 1e-6);
        // Fifth push wraps: drops the oldest (1.0), keeps 3,3,3,3.
        s.push([3.0; 5]);
        assert_eq!(s.mean(), [3.0; 5]);
    }

    #[test]
    fn meter_bar_geometry() {
        let rects = meter_rects([0.5, 0.0], [1.0, 1.0]);
        // Background: full width at the margin.
        let bg = rects[0];
        assert_eq!(bg.x, HUD_MARGIN);
        assert_eq!(bg.w, BAR_W);
        assert_eq!(bg.h, BAR_H);
        // Second channel one BAR_GAP down.
        assert_eq!(rects[3].y, HUD_MARGIN + BAR_GAP);
        // Fill half width, same height/top as its background.
        let fill = rects[1];
        assert_eq!(fill.y, bg.y);
        assert_eq!(fill.h, bg.h);
        approx(fill.w, BAR_W * 0.5, 1e-4);
        // Silent channel: no fill.
        assert_eq!(rects[4].w, 0.0);
        // Peak ticks overhang the bar by 2 px top and bottom.
        let tick = rects[2];
        approx(tick.x, HUD_MARGIN + BAR_W - 2.0, 1e-4);
        approx(tick.h, BAR_H + 4.0, 1e-4);
        approx(tick.y, bg.y - 2.0, 1e-4);
        // A full-scale tick is clamped inside the bar's right edge.
        let rects_full = meter_rects([1.0, 1.0], [1.0, 1.0]);
        assert!(rects_full[2].x + rects_full[2].w <= HUD_MARGIN + BAR_W + f32::EPSILON);
    }

    #[test]
    fn hud_assembly() {
        let f = Frame {
            rms: [0.5, 0.25],
            peak: [0.9, 0.4],
            ..Frame::default()
        };
        let mut ms = MeterState::default();
        let mut sm = FeatureSmooth::default();
        let settings = ViewSettings::default();
        let hud = HudData::assemble(
            HudSources {
                source_name: "alsa_output…monitor",
                backend: "pulse",
                adapter: "Vulkan",
                rate: 48_000,
                window_frames: 1024,
                depth: 48,
                fps: 60.0,
                frame_ms: 16.6,
                peak_hold_ms: PEAK_HOLD_MS,
                settings: &settings,
                latest: Some(&f),
            },
            &mut ms,
            &mut sm,
        );
        assert_eq!(hud.source_name, "alsa_output…monitor");
        assert_eq!(hud.backend, "pulse");
        approx(hud.hop_ms, 21.333, 0.01);
        approx(hud.rms_fill[0], rms_to_fill(0.5), 0.001);
        assert!(hud.feature_lines[0].contains("Hz"));
        assert!(hud.overlay.is_empty());
        assert!(hud.show_hints);
    }

    #[test]
    fn hud_assembly_without_frames_is_flat() {
        let mut ms = MeterState::default();
        let mut sm = FeatureSmooth::default();
        let settings = ViewSettings::default();
        let hud = HudData::assemble(
            HudSources {
                source_name: "none",
                backend: "none",
                adapter: "n/a",
                rate: 48_000,
                window_frames: 1024,
                depth: 48,
                fps: 0.0,
                frame_ms: 0.0,
                peak_hold_ms: PEAK_HOLD_MS,
                settings: &settings,
                latest: None,
            },
            &mut ms,
            &mut sm,
        );
        assert_eq!(hud.rms_fill, [0.0; 2]);
        assert_eq!(hud.peak_fill, [0.0; 2]);
        assert!(hud.feature_lines.iter().all(|l| l.is_empty()));
    }

    fn approx(a: f32, b: f32, tol: f32) {
        assert!((a - b).abs() < tol, "{a} vs {b}");
    }
    fn approx_f64(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} vs {b}");
    }

    /// The Ember theme must mirror this crate's M6 hardcoded hold.
    #[test]
    fn theme_peak_hold_matches_constant() {
        let ember = neopeek_theme::builtin("Ember").expect("Ember parses");
        assert_eq!(ember.meters.peak_hold_ms, PEAK_HOLD_MS);
    }
}
