//! Idle-pause gate (pure, headless): decides when the render loop may
//! stop drawing because nothing audible is happening (PLAN-style perf
//! feature: "pause rendering when there's been no active audio for a few
//! seconds" — saves drawing a whole bunch of nothing at 60 fps).
//!
//! Why not just "no frames arriving": monitor streams often deliver
//! all-zero chunks continuously, so frame-arrival alone never fires on
//! many hosts. The gate therefore treats **digital silence** as idleness
//! too: a drained slice is silent when the max of its raw FFT bins is
//! `0.0` exactly (PA monitor silence is exact zeros; the DSP sanitizes
//! anything non-finite to `0.0`). The renderer's drain loop already
//! computes that per-slice max for `AutoScale::observe`, so feeding the
//! gate is free.
//!
//! Inputs (fed by the binary's wiring, see `crates/neopeek/src/app.rs`):
//! - [`IdleGate::note_drain`] after every frame drain (render or the
//!   idle poll-drain): `(now, frames_drained, frame_max)`.
//! - [`IdleGate::note_input`] when an input event changed settings or
//!   camera.
//! - [`IdleGate::decide`] once per `about_to_wait` wake.
//!
//! Semantics:
//! - *Activity* = a drained **non-silent** frame, or a state-changing
//!   input.
//! - *Idle* = `now − last_activity > threshold`, where `last_activity`
//!   is the later of (last non-silent frame, last input change);
//!   `None` (nothing has ever been active) counts as idle.
//!   `threshold == 0` disables the gate entirely (always [`IdleAction::Redraw`]).
//! - Silent frames flowing do **not** reset `last_activity`, but they do
//!   refresh `last_seen_frame` and set the `saw_frames_since_idle`
//!   latch: the stream is alive, so the caller keeps the 1 s poll going
//!   and a new sound is noticed at the very next poll.
//! - `input_dirty` (or the `note_input` latch) yields exactly one forced
//!   [`IdleAction::Redraw`], then the gate re-enters the normal
//!   evaluation. Note the activity window the input opens keeps the loop
//!   on the ordinary tick-paced path for up to `threshold` afterwards —
//!   bounded (default 5 s), per the same idle formula.
//! - `frames_pending == true` → [`IdleAction::Redraw`]: in the active
//!   path that is the low-latency "drain fresh frames now" trigger.
//!   While idle the caller drains silently (a future
//!   `Renderer::poll_audio`) *before* deciding, so pending-ness after
//!   that means the caller skipped the poll-drain — the gate then
//!   degrades safely to one render per poll (never a hot spin, never
//!   misses loud audio) instead of a full sleep.
//!
//! Resume-latency budget: sound onset → next idle poll ≤ `poll` (1 s;
//! average 0.5 s) → the poll-drain notices the loud frame and
//!   `decide` returns [`IdleAction::Redraw`] → the caller's existing
//!   pacing requests a redraw, aligned by the compositor frame callback
//!   (≤ 1 display frame). Worst case ≈ 1 s + 1 frame; the up-to-`poll`
//!   worth of frames that arrived while sleeping are drained on resume
//!   (the ring is drained fully at each poll, so at most ~1 s of slices
//!   can be dropped by the DSP's newest-drop policy while it is full).
//!
//! While idle the sleep is simply one poll (`poll`, 1 s): the literal
//! `min(1 s, threshold − elapsed)` from the design notes degenerates
//! (when idle is already established, `threshold − elapsed` is
//! negative), so the poll cadence is the constant; the countdown to the
//! idle transition itself is paced by the caller's normal redraw tick,
//! because rendering continues until the idle threshold has elapsed.
//!
//! Frozen/muted/autospin are deliberately **not** gate inputs: the wiring
//! keeps rendering unconditionally in those states — frozen = the user
//! explicitly froze the scene to look at it; muted = ingestion paused but
//! the user may still orbit; autospin animates the camera every frame.
//! Only genuine audio idleness pauses the renderer.

use std::time::{Duration, Instant};

/// Idle poll cadence: how often the loop wakes while idle to check
/// whether audio resumed (or frames restarted). This bounds the
/// resume-latency budget below.
pub const IDLE_POLL: Duration = Duration::from_secs(1);

/// Default idle threshold seconds (`--idle-pause`, 0 = disabled).
pub const DEFAULT_IDLE_PAUSE_SECS: u64 = 5;

/// What `about_to_wait` should do this wake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleAction {
    /// Not idle: use the caller's normal redraw pacing (`redraw_due` +
    /// tick deadline; the caller still avoids busy-spinning within a
    /// tick).
    Redraw,
    /// Idle: do **not** request a redraw; sleep until the given instant
    /// offset from `now` (one poll), then re-evaluate.
    Sleep(Duration),
}

/// The idle-pause state machine. Pure: no clocks, no threads — all
/// instants are supplied by the caller so it stays headless-testable.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IdleGate {
    /// Pause rendering once no activity for this long. `0` = disabled.
    threshold: Duration,
    /// Poll cadence while idle ([`IDLE_POLL`]).
    poll: Duration,
    /// The later of (last non-silent drained frame, last state-changing
    /// input). `None` until something has ever been active.
    last_activity: Option<Instant>,
    /// Any drained frame, silent or not (diagnostics + latch input).
    last_seen_frame: Option<Instant>,
    /// Frames arrived (drained) since the gate last considered itself
    /// idle — "the stream is still alive" latch.
    saw_frames_since_idle: bool,
    /// A `note_input` not yet consumed by `decide`: guarantees the
    /// state-change redraw fires even if the caller keeps reporting
    /// `input_dirty == false`.
    input_pending: bool,
}

impl IdleGate {
    /// Gate with the given idle threshold and the standard 1 s poll.
    /// `threshold == 0` disables idling (always [`IdleAction::Redraw`]).
    pub fn new(threshold: Duration) -> Self {
        Self::with_poll(threshold, IDLE_POLL)
    }

    /// Gate with an explicit poll cadence (tests use a short one).
    pub fn with_poll(threshold: Duration, poll: Duration) -> Self {
        Self {
            threshold,
            poll,
            last_activity: None,
            last_seen_frame: None,
            saw_frames_since_idle: false,
            input_pending: false,
        }
    }

    /// Configured idle threshold (`0` = disabled).
    pub fn threshold(&self) -> Duration {
        self.threshold
    }

    /// Whether the gate is currently idle at `now` (nothing active for
    /// longer than the threshold, or nothing was ever active). The
    /// caller checks this to decide whether an idle poll-drain is needed
    /// before `decide`.
    pub fn is_idle(&self, now: Instant) -> bool {
        match self.last_activity {
            None => true,
            Some(t) => now.saturating_duration_since(t) > self.threshold,
        }
    }

    /// Whether frames have arrived since the gate last considered itself
    /// idle (the "stream alive while silent" latch).
    pub fn saw_frames_since_idle(&self) -> bool {
        self.saw_frames_since_idle
    }

    /// The instant of the most recent drained frame (any kind), for
    /// diagnostics.
    pub fn last_seen_frame(&self) -> Option<Instant> {
        self.last_seen_frame
    }

    /// A drain happened (`Renderer::render` or the idle poll-drain).
    /// `frames == 0` (frozen/muted wake, empty ring) is a no-op — frames
    /// that never arrived prove nothing. Any drained frame refreshes
    /// `last_seen_frame`; a drained frame whose max bin magnitude is
    /// `> 0.0` is activity and resumes rendering immediately at the next
    /// `decide`. A silent drain while idle sets the
    /// `saw_frames_since_idle` latch without touching `last_activity`.
    pub fn note_drain(&mut self, now: Instant, frames: usize, frame_max: f32) {
        if frames == 0 {
            return;
        }
        self.last_seen_frame = Some(now);
        if frame_max > 0.0 {
            self.last_activity = Some(now);
            self.saw_frames_since_idle = false;
        } else if self.is_idle(now) {
            self.saw_frames_since_idle = true;
        }
    }

    /// A state-changing input event (settings/camera/theme/resize).
    /// Opens an activity window: the forced redraw is consumed once by
    /// `decide`, and the ordinary idle formula keeps the loop on the
    /// tick-paced path until `threshold` after this instant.
    pub fn note_input(&mut self, now: Instant) {
        self.last_activity = Some(now);
        self.input_pending = true;
        self.saw_frames_since_idle = false;
    }

    /// The wake decision. `frames_pending` = undrained DSP frames in the
    /// ring right now; `input_dirty` = an input event changed state
    /// since the last call (OR-ed with any unconsumed `note_input`).
    pub fn decide(&mut self, now: Instant, frames_pending: bool, input_dirty: bool) -> IdleAction {
        // Disabled: never idle, always the caller's normal pacing.
        if self.threshold.is_zero() {
            self.input_pending = false;
            return IdleAction::Redraw;
        }
        // State-changing input: exactly one forced redraw, then normal
        // evaluation (with the fresh activity window this opens).
        if input_dirty || self.input_pending {
            self.input_pending = false;
            self.last_activity = Some(now);
            return IdleAction::Redraw;
        }
        // Undrained frames: low-latency trigger in the active path; a
        // safe degrade (one render per poll) if the caller skipped the
        // idle poll-drain. Either way, render.
        if frames_pending || !self.is_idle(now) {
            return IdleAction::Redraw;
        }
        IdleAction::Sleep(self.poll)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Advance helper: real `Instant`s offset from one base, so the
    /// arithmetic stays monotone without any fake clock.
    fn t(base: Instant, offset: Duration) -> Instant {
        base + offset
    }

    const S1: Duration = Duration::from_secs(1);
    const THRESHOLD: Duration = Duration::from_secs(5);

    /// Idle fires only after the full threshold of inactivity.
    #[test]
    fn idle_after_threshold() {
        let base = Instant::now();
        let mut gate = IdleGate::new(THRESHOLD);
        gate.note_drain(t(base, S1), 3, 0.5); // loud frame at t = 1 s
        assert!(!gate.is_idle(t(base, 6 * S1)));
        assert_eq!(gate.decide(t(base, 6 * S1), false, false), IdleAction::Redraw);
        assert_eq!(
            gate.decide(t(base, 6 * S1 + Duration::from_millis(1)), false, false),
            IdleAction::Sleep(IDLE_POLL)
        );
    }

    /// Continuous silent frames do not reset the activity clock: the
    /// countdown started when the last non-silent frame drained.
    #[test]
    fn silent_frames_do_not_reset_activity() {
        let base = Instant::now();
        let mut gate = IdleGate::new(THRESHOLD);
        gate.note_drain(t(base, S1), 3, 0.5); // last loud frame at t = 1 s
        for s in 2..=6u64 {
            gate.note_drain(t(base, Duration::from_secs(s)), 47, 0.0);
            assert!(!gate.saw_frames_since_idle(), "still in countdown at {s} s");
        }
        // t = 6.001 s: 5.001 s since the last loud frame → idle, despite
        // a silent frame having drained at t = 6 s.
        assert!(gate.is_idle(t(base, 6 * S1 + Duration::from_millis(1))));
        assert_eq!(
            gate.decide(t(base, 6 * S1 + Duration::from_millis(1)), false, false),
            IdleAction::Sleep(IDLE_POLL)
        );
        // The silent stream is alive: the latch is set once idle.
        gate.note_drain(t(base, 7 * S1), 47, 0.0);
        assert!(gate.saw_frames_since_idle());
    }

    /// A non-silent frame seen at an idle poll resumes rendering at that
    /// same wake — no extra poll needed.
    #[test]
    fn non_silent_frame_resumes_immediately() {
        let base = Instant::now();
        let mut gate = IdleGate::new(THRESHOLD);
        gate.note_drain(t(base, S1), 3, 0.5);
        // …the countdown is still active at exactly the threshold
        // elapsed (idle needs strictly more)…
        assert_eq!(gate.decide(t(base, 6 * S1), false, false), IdleAction::Redraw);
        assert_eq!(gate.decide(t(base, 6 * S1 + Duration::from_millis(1)), false, false), IdleAction::Sleep(IDLE_POLL));
        // Idle polls drain silently while the stream delivers zeros…
        gate.note_drain(t(base, 7 * S1), 47, 0.0);
        assert_eq!(gate.decide(t(base, 7 * S1), false, false), IdleAction::Sleep(IDLE_POLL));
        // …then sound returns: the poll-drain sees it and resumes now.
        gate.note_drain(t(base, 8 * S1), 47, 0.25);
        assert_eq!(gate.decide(t(base, 8 * S1), false, false), IdleAction::Redraw);
        assert!(!gate.saw_frames_since_idle());
    }

    /// A state-changing input yields exactly one forced redraw and then
    /// re-enters the ordinary evaluation: the input's activity window
    /// (threshold wide) keeps the tick-paced path going, after which the
    /// gate sleeps again. Composed with `redraw_due` this means one
    /// immediate render (the tick has long elapsed) and no more until
    /// the window closes.
    #[test]
    fn input_dirty_redraws_once_then_normal_evaluation() {
        let base = Instant::now();
        let mut gate = IdleGate::new(THRESHOLD);
        gate.note_drain(t(base, S1), 3, 0.5);
        // Idled out.
        assert_eq!(gate.decide(t(base, 7 * S1), false, false), IdleAction::Sleep(IDLE_POLL));
        // Input while idle → one forced redraw at the next wake…
        gate.note_input(t(base, 8 * S1));
        assert_eq!(gate.decide(t(base, 8 * S1), false, false), IdleAction::Redraw);
        // …consumed: further wakes are the ordinary (still active) path,
        // so the caller's `redraw_due` allows at most tick-cadence
        // renders — here right after a render nothing is due.
        assert_eq!(gate.decide(t(base, 8 * S1 + Duration::from_millis(1)), false, false), IdleAction::Redraw);
        // Window closes `threshold` after the input.
        assert_eq!(
            gate.decide(t(base, 13 * S1 + Duration::from_millis(1)), false, false),
            IdleAction::Sleep(IDLE_POLL)
        );
        // The `input_dirty` argument alone (no note_input) behaves the same.
        let mut gate = IdleGate::new(THRESHOLD);
        assert_eq!(gate.decide(t(base, S1), false, true), IdleAction::Redraw);
    }

    /// Resume latency from sound onset to the redraw decision is within
    /// one poll (plus the caller's tick): here 100 ms polls, idle began
    /// at t = 6.001 s, sound onset at t = 7.15 s lands in the t = 7.2 s
    /// poll → decision at 7.2 s, 50 ms after onset.
    #[test]
    fn resume_latency_within_one_poll() {
        let base = Instant::now();
        let poll = Duration::from_millis(100);
        let mut gate = IdleGate::with_poll(THRESHOLD, poll);
        gate.note_drain(t(base, S1), 3, 0.5); // last loud frame at t = 1 s
        gate.note_drain(t(base, 6 * S1 + Duration::from_millis(1)), 5, 0.0); // silence flows, latch set
        assert!(gate.saw_frames_since_idle());
        // Idle polls: drain silently first (the wiring contract), then
        // decide.
        let mut wake = t(base, Duration::from_millis(6_300));
        while wake < t(base, 7 * S1) {
            gate.note_drain(wake, 5, 0.0);
            assert_eq!(gate.decide(wake, false, false), IdleAction::Sleep(poll));
            wake += poll;
        }
        // Sound onset just after a poll (worst case): the next poll
        // notices within `poll`.
        let sound_onset = t(base, Duration::from_millis(7_150));
        gate.note_drain(t(base, Duration::from_millis(7_200)), 47, 0.4);
        assert_eq!(gate.decide(t(base, Duration::from_millis(7_200)), false, false), IdleAction::Redraw);
        assert!(t(base, Duration::from_millis(7_200)).duration_since(sound_onset) <= poll);
    }

    /// `threshold == 0` disables the gate: never idle, always the normal
    /// pacing path (and input latches can't make it sleep).
    #[test]
    fn zero_threshold_disables() {
        let base = Instant::now();
        let mut gate = IdleGate::new(Duration::ZERO);
        // No activity ever recorded; even so, never Sleep.
        for s in 1..30u64 {
            assert_eq!(
                gate.decide(t(base, Duration::from_secs(s)), false, false),
                IdleAction::Redraw
            );
            gate.note_drain(t(base, Duration::from_secs(s)), 0, 0.0);
        }
        gate.note_input(t(base, S1));
        assert_eq!(gate.decide(t(base, 100 * S1), false, false), IdleAction::Redraw);
    }

    /// Degenerate instants: a `now` earlier than the recorded activity
    /// must not panic (saturate to "not idle"), and an activity instant
    /// from before any drain must not make `Sleep` negative.
    #[test]
    fn degenerate_instants() {
        let base = Instant::now();
        let mut gate = IdleGate::new(THRESHOLD);
        gate.note_drain(base, 3, 0.5);
        let before = base.checked_sub(10 * S1);
        if let Some(before) = before {
            // Clock went "backwards": saturates to 0 elapsed → active.
            assert!(!gate.is_idle(before));
            assert_eq!(gate.decide(before, false, false), IdleAction::Redraw);
        }
        // A far-future-looking poll with a stale-but-present activity
        // instant sleeps exactly one poll, never zero/negative.
        assert_eq!(
            gate.decide(t(base, 1_000 * S1), false, false),
            IdleAction::Sleep(IDLE_POLL)
        );
        // Zero-count drains are pure no-ops (prove nothing).
        let mut gate = IdleGate::new(THRESHOLD);
        assert!(gate.is_idle(base));
        gate.note_drain(base, 0, 0.9);
        assert!(gate.is_idle(base));
        assert_eq!(gate.last_seen_frame(), None);
    }
}
