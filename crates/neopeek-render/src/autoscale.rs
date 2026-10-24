//! Content-adaptive waterfall gain (`a` toggle): the renderer feeds this
//! pure state machine the max raw bin magnitude of every drained FFT
//! slice, and it produces the `WaterUniforms.gain` so the tallest recent
//! content sits at [`AUTOSCALE_TARGET_Y`] — the world-space height at the
//! fresh slice plane that projects to 0.8 of the screen's half-height at
//! the default vantage (pinned by the camera projection test below; the
//! fixed [`crate::views::DEFAULT_WATERFALL_GAIN`] lands a typical
//! frame at ≈ the same fraction).
//!
//! Smoothing is **frame-count based**, deliberately not `dt`-based: FFT
//! slices arrive on a fixed cadence (= 1024 frames / capture rate,
//! ≈ 21.3 ms at 48 kHz), so drained slices are the natural timebase and
//! the state machine stays a plain `Copy`-less struct — no clock, no
//! dt-injection, fully headless-testable.
//! - Attack: reach a louder content level in ~[`ATTACK_SLICES`] (2
//!   slices ≈ 43 ms) so loud material fills the target quickly.
//! - Release: fall back slowly over ~[`RELEASE_SLICES`] (64 slices ≈
//!   1.4 s) so the surface does not pump on usual level jitter.
//!
//! Zero-alloc: no buffers, the renderer just calls `observe` per drained
//! slice and reads `gain` for the uniform write.

use crate::views::DEFAULT_WATERFALL_GAIN;

/// Adaptive target height in world units at the fresh slice plane
/// (z = 0): the default vantage projects this y to 0.8 of the NDC
/// half-height, i.e. the tallest recent content fills ~8/10 of the view.
/// Pinned by the `target_projects_to_0_8_of_half_height` test below.
pub const AUTOSCALE_TARGET_Y: f32 = 1.684;

/// Gain clamp so silence-tiny content cannot blow the uniform up
/// (`gain ≥ GAIN_MIN`) and a huge transient cannot squish it below
/// visibility (`gain ≤ GAIN_MAX`).
pub const GAIN_MIN: f32 = 0.05;
pub const GAIN_MAX: f32 = 20.0;

/// Fast attack: one observed slice jumps the smoothed level this far
/// toward a newly louder value; ≈ 2 slices (~43 ms) to close.
const ATTACK_SLICES: f32 = 2.0;
/// Slow release: the number of slice cadences it takes the smoothed
/// level to close most of a downward step — ≈ 1.4 s at 48 kHz.
const RELEASE_SLICES: f32 = 64.0;

/// The adaptive-gain state (see crate docs).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutoScale {
    target_y: f32,
    /// Exponentially-smoothed recent max raw bin magnitude.
    smoothed_max: f32,
    /// The produced uniform gain; held while silent.
    gain: f32,
}

impl Default for AutoScale {
    fn default() -> Self {
        Self::new()
    }
}

impl AutoScale {
    pub fn new() -> Self {
        Self {
            target_y: AUTOSCALE_TARGET_Y,
            smoothed_max: 0.0,
            // Floats on [`DEFAULT_WATERFALL_GAIN`] until the first sound,
            // so turning auto_scale on over silence shows the fixed
            // default height, not a flat line.
            gain: DEFAULT_WATERFALL_GAIN,
        }
    }

    /// Folds one drained slice: `frame_max` is the max raw bin magnitude
    /// of that slice (`Frame.bins` max). Silence (`frame_max == 0`)
    /// intentionally holds the last gain — never divide by zero — and a
    /// wake from silence snaps the smoothed level straight to the
    /// incoming value (no release-lag low-pass on the first sound).
    pub fn observe(&mut self, frame_max: f32) {
        // Negative magnitudes are impossible, but a defensive max keeps
        // any strange input at least silent-shaped (no state change).
        if !frame_max.is_finite() || frame_max <= 0.0 {
            return;
        }
        if self.smoothed_max <= 0.0 {
            self.smoothed_max = frame_max;
        } else {
            // Rising content attacks fast; falling content releases slow.
            let alpha =
                1.0 / if frame_max > self.smoothed_max { ATTACK_SLICES } else { RELEASE_SLICES };
            self.smoothed_max += (frame_max - self.smoothed_max) * alpha;
        }
        self.gain = Self::gain_for(self.target_y, self.smoothed_max);
    }

    /// Hard reset (toggle off, renderer re-arm): forgets the smoothed
    /// level and gain. The shell re-runs this whenever auto_scale is off,
    /// so the *next* toggle-on re-arms from silence; meanwhile the shell
    /// writes [`DEFAULT_WATERFALL_GAIN`] directly.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Current adaptive uniform gain.
    pub fn gain(&self) -> f32 {
        self.gain
    }

    /// Raw mapping gain for a level: `target_y / (1.8·√(25·m))`, i.e. the
    /// shader's y = gain·freq_scale·1.8·√(25·|X|) puts content of `m` at
    /// `target_y` (before the user's `-`/`=` `freq_scale` zoom, which
    /// still multiplies on top). Clamped to the sane range.
    fn gain_for(target_y: f32, level: f32) -> f32 {
        let raw = target_y / (1.8 * (25.0 * level).sqrt());
        raw.clamp(GAIN_MIN, GAIN_MAX)
    }
}

/// Gain computed for a converged level `m` (the asymptote the attack
/// test below converges toward).
#[cfg(test)]
fn converged_gain(m: f32) -> f32 {
    AutoScale::gain_for(AUTOSCALE_TARGET_Y, m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::Camera;

    fn at(mut s: AutoScale, level: f32, n: usize) -> AutoScale {
        for _ in 0..n {
            s.observe(level);
        }
        s
    }

    #[test]
    fn attack_rises_quickly_toward_target() {
        // First sound snaps the level (no lag from the silent start).
        let s = at(AutoScale::new(), 2.0, 1);
        assert!(
            (s.smoothed_max - 2.0).abs() < 1e-9,
            "first observed slice must snap, got {}",
            s.smoothed_max
        );
        // A louder step closes ~75 % in 2 slices (per-slice approach ½).
        let louder = at(AutoScale::new(), 2.0, 4);
        let target = converged_gain(2.0);
        assert!(
            louder.gain() >= 0.95 * target && louder.gain() <= target,
            "4 attack slices must be within 5 % of the target gain: {} vs {target}",
            louder.gain()
        );
    }

    #[test]
    fn release_falls_much_more_slowly_than_attack() {
        // Converge loud, then drop 4×. After 4 slices the gain must have
        // moved only a small fraction of the way toward the quieter
        // target (the same 4 slices close ~94 % of an *attack* step).
        let s = at(at(AutoScale::new(), 2.0, 8), 0.5, 4);
        let loud_gain = converged_gain(2.0);
        let quiet_target = converged_gain(0.5);
        let progress = (s.gain() - loud_gain) / (quiet_target - loud_gain);
        assert!(
            (0.0..0.15).contains(&progress),
            "4 release slices must travel < 15 % of the drop, got {progress}"
        );
    }

    #[test]
    fn silence_holds_last_gain_never_divides_by_zero() {
        let mut s = at(AutoScale::new(), 0.25, 8);
        let g = s.gain();
        assert!(
            g.is_finite() && g > 0.0,
            "busy state must be finite/positive"
        );
        for _ in 0..200 {
            s.observe(0.0);
        }
        assert_eq!(s.gain(), g, "silence keeps the last gain");
        // Silence from the start (toggle-on over a muted feed): the gain
        // stays the fixed default, never 0/NaN.
        let fresh = at(AutoScale::new(), 0.0, 100);
        assert!((fresh.gain() - DEFAULT_WATERFALL_GAIN).abs() < 1e-9);
    }

    #[test]
    fn gain_clamped_to_sane_range() {
        // Tiny content → raw gain ≈ 5917 → clamped to GAIN_MAX.
        let tiny = at(AutoScale::new(), 1e-9, 1);
        assert_eq!(tiny.gain(), GAIN_MAX);
        // Huge content → raw gain ≈ 1.9e-4 → clamped to GAIN_MIN.
        let huge = at(AutoScale::new(), 1e9, 1);
        assert_eq!(huge.gain(), GAIN_MIN);
        // Normal content sits between.
        let mid = at(AutoScale::new(), 0.3, 1);
        assert!(GAIN_MIN < mid.gain() && mid.gain() < GAIN_MAX, "{}", mid.gain());
    }

    #[test]
    fn toggle_off_resets_and_rearms_cleanly() {
        let mut s = at(AutoScale::new(), 3.0, 8);
        assert_ne!(s.smoothed_max, 0.0);
        s.reset();
        assert_eq!(s.smoothed_max, 0.0);
        assert!(
            (s.gain() - DEFAULT_WATERFALL_GAIN).abs() < 1e-9,
            "re-armed state floats on the fixed default"
        );
        // First slice after a re-arm snaps (no release lag from the old
        // loud history).
        let s = at(s, 0.5, 1);
        assert!(
            (s.smoothed_max - 0.5).abs() < 1e-9,
            "post-reset first slice snaps, got {}",
            s.smoothed_max
        );
        assert!((s.gain() - converged_gain(0.5)).abs() < 1e-6);
    }

    /// The target height is defined *visually*: at the default vantage the
    /// fresh slice plane (z = 0) content at `AUTOSCALE_TARGET_Y` must
    /// project to 0.8 of the screen half-height — not overshoot it.
    #[test]
    fn target_projects_to_0_8_of_half_height() {
        let cam = Camera::default_vantage(800, 600);
        let vp = cam.view_proj();
        let ndc = crate::camera::project(vp, [0.0, AUTOSCALE_TARGET_Y, 0.0])[1];
        assert!(
            (ndc - 0.8).abs() < 0.02,
            "target y must project to 0.8 of half-height, got {ndc}"
        );
    }
}
