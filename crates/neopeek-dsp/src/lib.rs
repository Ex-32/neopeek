//! neopeek-dsp: analysis pipeline from interleaved PCM to FFT frames.
//!
//! M3 scope (PLAN.md §2.3, §4.1; constants verified against sndpeek-1.41 in
//! `docs/research-sndpeek.md`):
//! - per-frame stereo→mono average (L and R kept for meters/Lissajous upstream)
//! - Hanning window over the 1024-sample analysis window
//! - zero-padded 2048-point forward real FFT (rustfft) → 1024 magnitude bins
//! - feature extraction: spectral centroid, flux, rolloff at 50%/80%
//!   (4-frame moving average smoothing, sndpeek `LP = 4` parity)
//!
//! All math is reimplemented fresh from definitions — nothing is ported
//! (clean-room rule). Bin magnitudes are stored raw; the dB/linear display
//! mapping and frequency-axis positioning belong to render/theme.

use rustfft::{FftPlanner, num_complex::Complex};

/// Audio samples per analysis window/hop (sndpeek `SND_BUFFER_SIZE`).
pub const WINDOW_FRAMES: usize = 1024;
/// FFT length: the window zero-padded out from 1024 to 2048 (display path).
pub const FFT_SIZE: usize = 2048;
/// Magnitude bins per slice (`FFT_SIZE/2`).
pub const BINS: usize = FFT_SIZE / 2;
/// Channel count of interleaved input.
pub const CHANNELS: usize = 2;
/// Frames per feature moving average (sndpeek `LP`).
pub const LP_TAPS: usize = 4;

/// One waterfall slice plus live audio meters/features.
///
/// Fixed-size POD so it fits in a lock-free ring with no allocation.
#[derive(Debug, Clone, Copy)]
pub struct Frame {
    pub seq: u64,
    /// `|X_k|` for k = 0..BINS of the windowed, zero-padded FFT.
    pub bins: [f32; BINS],
    /// Last window of mono (averaged) samples, for waveform/Lissajous.
    pub mono: [f32; WINDOW_FRAMES],
    /// Last window of the left channel (true Lissajous).
    pub left: [f32; WINDOW_FRAMES],
    /// Last window of the right channel (true Lissajous).
    pub right: [f32; WINDOW_FRAMES],
    /// `true` when L and R were near-identical this window (a mono-ish
    /// source, e.g. a mono monitor): the Lissajous figure must then be
    /// drawn as the pseudo-Lissajous (mono vs its 512-sample delayed copy)
    /// because L̄ vs R̄ degenerates into a straight diagonal line.
    pub mono_like: bool,
    /// Per-channel window RMS (linear amplitude).
    pub rms: [f32; CHANNELS],
    /// Per-channel window peak (linear amplitude).
    pub peak: [f32; CHANNELS],
    /// Spectral centroid in bins (mean-weighted bin index).
    pub centroid: f32,
    /// RMS-normalized spectral flux (see [`flux`]).
    pub flux: f32,
    /// Lowest bin where cumulative magnitude crosses 50% / 80% of total.
    pub rolloff50: f32,
    pub rolloff80: f32,
}

/// Zeroed window (all-silent frame); manual because `Default` is not
/// implemented in core for large arrays.
impl Default for Frame {
    /// Zeroed, all-silent frame (`mono_like = true` — no stereo evidence).
    fn default() -> Self {
        Self {
            seq: 0,
            bins: [0.0; BINS],
            mono: [0.0; WINDOW_FRAMES],
            left: [0.0; WINDOW_FRAMES],
            right: [0.0; WINDOW_FRAMES],
            mono_like: true,
            rms: [0.0; CHANNELS],
            peak: [0.0; CHANNELS],
            centroid: 0.0,
            flux: 0.0,
            rolloff50: 0.0,
            rolloff80: 0.0,
        }
    }
}

/// Hanning window, `w[n] = 0.5 * (1 - cos(2π n / N))` (periodic variant).
pub fn hanning(n: usize) -> Vec<f32> {
    let n_f = n as f32;
    (0..n)
        .map(|i| {
            let x = std::f32::consts::TAU * i as f32 / n_f;
            0.5 * (1.0 - x.cos())
        })
        .collect()
}

/// Per-channel RMS and peak over a window of interleaved samples.
pub fn channel_levels(interleaved: &[f32]) -> ([f32; CHANNELS], [f32; CHANNELS]) {
    let mut s2 = [0.0f64; CHANNELS];
    let mut peak = [0.0f32; CHANNELS];
    let mut n = [0usize; CHANNELS];
    for (i, &s) in interleaved.iter().enumerate() {
        let ch = i % CHANNELS;
        s2[ch] += (s as f64) * (s as f64);
        let abs = s.abs();
        if abs > peak[ch] {
            peak[ch] = abs;
        }
        n[ch] += 1;
    }
    let rms = [
        (s2[0] / (n[0] as f64).max(1.0)).sqrt() as f32,
        (s2[1] / (n[1] as f64).max(1.0)).sqrt() as f32,
    ];
    (rms, peak)
}

/// Spectral centroid in bins: first moment over zeroth (weighted mean of
/// the bin index). Returns `0.0` on silence.
pub fn centroid(magnitudes: &[f32]) -> f32 {
    let m0: f32 = magnitudes.iter().sum();
    if m0 <= 0.0 {
        return 0.0;
    }
    let m1: f32 = magnitudes
        .iter()
        .enumerate()
        .map(|(k, m)| k as f32 * m)
        .sum();
    m1 / m0
}

/// RMS-normalized spectral flux: the L2 norm of the change between the
/// current and previous magnitude vectors, each scaled to unit RMS. The
/// normalization keeps loudness from dominating the motion readout.
pub fn flux(current: &[f32], previous: &[f32]) -> f32 {
    let unit = |s: &[f32]| -> f32 {
        let sum: f32 = s.iter().map(|m| m * m).sum();
        (sum / s.len().max(1) as f32).sqrt()
    };
    let cur = unit(current);
    let prev = unit(previous);
    if cur <= 0.0 || prev <= 0.0 {
        return 0.0;
    }
    let mut acc: f32 = 0.0;
    for (a, b) in current.iter().zip(previous.iter()) {
        let d = a / cur - b / prev;
        acc += d * d;
    }
    acc.sqrt()
}

/// Rolloff: smallest bin index where the cumulative magnitude sum crosses
/// `perc * total` (0 < perc <= 1). Returns `0.0` for a zero spectrum.
pub fn rolloff(magnitudes: &[f32], perc: f32) -> f32 {
    let total: f32 = magnitudes.iter().sum();
    if total <= 0.0 {
        return 0.0;
    }
    let threshold = perc * total;
    let mut cum = 0.0f32;
    for (k, m) in magnitudes.iter().enumerate() {
        cum += m;
        if cum >= threshold {
            return k as f32;
        }
    }
    (magnitudes.len() - 1) as f32
}

/// 4-frame moving average of scalar features (sndpeek `LP = 4` parity).
#[derive(Debug, Clone, Copy, Default)]
pub struct MovingAverage {
    taps: [f32; LP_TAPS],
    head: usize,
    filled: usize,
}

impl MovingAverage {
    /// Feeds one new sample; returns the smoothed value. Early frames use
    /// the partial-tap average so the HUD ramps up honestly.
    pub fn push(&mut self, value: f32) -> f32 {
        self.taps[self.head] = value;
        self.head = (self.head + 1) % LP_TAPS;
        self.filled = (self.filled + 1).min(LP_TAPS);
        let count = self.filled;
        let mut sum = 0.0f32;
        for i in 0..count {
            sum += self.taps[(self.head + LP_TAPS - count + i) % LP_TAPS];
        }
        sum / count as f32
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Full analysis engine: owns the FFT plan, window, and smoothing state.
pub struct Engine {
    hanning: Vec<f32>,
    fft: std::sync::Arc<dyn rustfft::Fft<f32>>,
    scratch: Vec<Complex<f32>>,
    input: Vec<Complex<f32>>,
    /// Fixed-size mono downmix; converted to `[f32; WINDOW_FRAMES]` copies
    /// into `Frame` with no heap traffic on the ~21 ms hot path.
    mono: [f32; WINDOW_FRAMES],
    left: [f32; WINDOW_FRAMES],
    right: [f32; WINDOW_FRAMES],
    max_l_r_diff: f32,
    max_l_r_abs: f32,
    prev_bins: [f32; BINS],
    seq: u64,
    smooth_centroid: MovingAverage,
    smooth_flux: MovingAverage,
    smooth_rolloff50: MovingAverage,
    smooth_rolloff80: MovingAverage,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    pub fn new() -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let scratch = vec![Complex::new(0.0, 0.0); fft.get_inplace_scratch_len()];
        Self {
            hanning: hanning(WINDOW_FRAMES),
            fft,
            scratch,
            input: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            mono: [0.0; WINDOW_FRAMES],
            left: [0.0; WINDOW_FRAMES],
            right: [0.0; WINDOW_FRAMES],
            max_l_r_diff: 0.0,
            max_l_r_abs: 0.0,
            prev_bins: [0.0; BINS],
            seq: 0,
            smooth_centroid: MovingAverage::default(),
            smooth_flux: MovingAverage::default(),
            smooth_rolloff50: MovingAverage::default(),
            smooth_rolloff80: MovingAverage::default(),
        }
    }

    /// Processes exactly one window of interleaved stereo PCM.
    pub fn process_window(&mut self, interleaved: &[f32]) -> Option<Frame> {
        let want = WINDOW_FRAMES * CHANNELS;
        if interleaved.len() != want {
            tracing::warn!(got = interleaved.len(), want, "dsp: wrong window size");
            return None;
        }
        for k in 0..WINDOW_FRAMES {
            let l = interleaved[k * CHANNELS];
            let r = interleaved[k * CHANNELS + 1];
            self.mono[k] = (l + r) * 0.5;
            self.left[k] = l;
            self.right[k] = r;
            // Cheap mono-ness detector: tracks how much more the two
            // channels differ than either ends up being. l/R identical
            // (mono monitor) → 0 relative difference.
            let diff = (l - r).abs();
            if diff > self.max_l_r_diff {
                self.max_l_r_diff = diff;
            }
            let abs = l.abs().max(r.abs());
            if abs > self.max_l_r_abs {
                self.max_l_r_abs = abs;
            }
        }
        let (rms, peak) = channel_levels(interleaved);

        for (n, slot) in self.input[..WINDOW_FRAMES].iter_mut().enumerate() {
            let mono = self.mono[n] * self.hanning[n];
            *slot = Complex::new(mono, 0.0); // real signal → zero imaginary
        }

        // rustfft transforms in place with a single scratch buffer.
        self.fft.process_with_scratch(&mut self.input, &mut self.scratch);

        let mut mags = [0f32; BINS];
        for (k, slot) in mags.iter_mut().enumerate() {
            *slot = (self.input[k].re * self.input[k].re + self.input[k].im * self.input[k].im).sqrt();
        }
        let raw_flux = flux(&mags, &self.prev_bins);
        let c = centroid(&mags);
        let r50 = rolloff(&mags, 0.5);
        let r80 = rolloff(&mags, 0.8);
        self.prev_bins = mags;

        self.seq += 1;
        Some(Frame {
            seq: self.seq,
            bins: mags,
            mono: self.mono,
            left: self.left,
            right: self.right,
            mono_like: self.max_l_r_diff <= (self.max_l_r_abs * 1e-3).max(1e-6),
            rms,
            peak,
            centroid: self.smooth_centroid.push(c),
            flux: self.smooth_flux.push(raw_flux),
            rolloff50: self.smooth_rolloff50.push(r50),
            rolloff80: self.smooth_rolloff80.push(r80),
        })
    }

    pub fn sequence(&self) -> u64 {
        self.seq
    }

    pub fn reset_history(&mut self) {
        self.prev_bins.fill(0.0);
        self.smooth_centroid.reset();
        self.smooth_flux.reset();
        self.smooth_rolloff50.reset();
        self.smooth_rolloff80.reset();
        self.seq = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: usize = 48_000;

    fn sine_interleaved(freq: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|n| {
                let s = (std::f32::consts::TAU * freq * n as f32 / RATE as f32).sin();
                [s, s]
            })
            .collect()
    }

    fn argmax(slice: &[f32]) -> usize {
        slice
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(k, _)| k)
            .unwrap()
    }

    #[test]
    fn hanning_shape() {
        let w = hanning(8);
        assert!((w[0]).abs() < 1e-3);
        // periodic variant: symmetric pair w[1] ≈ w[N-1], both small
        assert!((w[1] - w[8 - 1]).abs() < 1e-6);
        assert!(w[1] < 0.2);
        let max = w.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        assert!((max - 1.0).abs() < 1e-6);
        assert_eq!(w.len(), 8);
    }

    #[test]
    fn sine_sits_in_its_bin() {
        // f such that k = 40: f = 40·48000/2048 = 937.5 Hz
        let f = 40.0 * RATE as f32 / FFT_SIZE as f32;
        let mut engine = Engine::new();
        let frame = engine
            .process_window(&sine_interleaved(f, WINDOW_FRAMES))
            .expect("window processed");
        let peak = argmax(&frame.bins);
        assert_eq!(peak, 40);
        let p = frame.bins[40];
        assert!(frame.bins[20] < 0.05 * p);
        assert!(frame.bins[100] < 0.05 * p);
    }

    #[test]
    fn sine_centroid_matches_bin() {
        let f = 100.0 * RATE as f32 / FFT_SIZE as f32;
        let mut engine = Engine::new();
        let window = sine_interleaved(f, WINDOW_FRAMES);
        let mut frame = None;
        for _ in 0..4 {
            frame = engine.process_window(&window);
        }
        let frame = frame.unwrap();
        assert!(
            frame.centroid > 90.0 && frame.centroid < 110.0,
            "centroid {} bins, want ~100",
            frame.centroid
        );
    }

    #[test]
    fn rolloff_math() {
        let mags: Vec<f32> = (0..1024).map(|k| if k < 10 { 1.0 } else { 0.0 }).collect();
        assert_eq!(rolloff(&mags, 0.5), 4.0);
        assert_eq!(rolloff(&mags, 0.8), 7.0);
        assert_eq!(rolloff(&mags, 1.0), 9.0);
        assert_eq!(rolloff(&[0.0; 4], 0.5), 0.0);
    }

    #[test]
    fn centroid_math() {
        let mut mags = [0f32; 8];
        mags[2] = 1.0;
        mags[4] = 3.0;
        assert_eq!(centroid(&mags), (2.0 + 12.0) / 4.0);
        assert_eq!(centroid(&[0.0; 8]), 0.0);
    }

    #[test]
    fn flux_math() {
        assert_eq!(flux(&[1.0, 2.0], &[1.0, 2.0]), 0.0);
        assert_eq!(flux(&[1.0, 0.0], &[0.0, 0.0]), 0.0);
        // unit-RMS swap between two bins = full motion (norm of (1.414,-1.414))
        let f = flux(&[1.0f32, 0.0], &[0.0f32, 1.0]);
        assert!((f - 2.0).abs() < 1e-6, "flux {f}");
    }

    #[test]
    fn moving_average_four_taps() {
        let mut m = MovingAverage::default();
        assert_eq!(m.push(4.0), 4.0);
        assert_eq!(m.push(8.0), 6.0);
        assert_eq!(m.push(12.0), 8.0);
        assert_eq!(m.push(16.0), 10.0);
        assert_eq!(m.push(0.0), 9.0);
        m.reset();
        assert_eq!(m.push(2.0), 2.0);
    }

    #[test]
    fn channel_levels_basic() {
        let inter = vec![1.0f32, 0.0];
        let (rms, peak) = channel_levels(&inter);
        assert_eq!(rms, [1.0, 0.0]);
        assert_eq!(peak, [1.0, 0.0]);
    }

    #[test]
    fn engine_sequence_counts() {
        let mut engine = Engine::new();
        let f = 40.0 * RATE as f32 / FFT_SIZE as f32;
        engine
            .process_window(&sine_interleaved(f, WINDOW_FRAMES))
            .unwrap();
        engine
            .process_window(&sine_interleaved(f, WINDOW_FRAMES))
            .unwrap();
        assert_eq!(engine.sequence(), 2);
    }

    #[test]
    fn frame_is_fixed_size_pod() {
        // Must stay a plain Copy POD to live in the lock-free ring.
        fn assert_copy<T: Copy>() {}
        assert_copy::<Frame>();
        // 4×1024-sample f32 arrays (bins, mono, left, right) + u64 seq +
        // 2×2 channel f32 (rms, peak) + 5 scalar f32 (centroid, flux,
        // rolloff50/80, mono_like-as-u32).
        let expected = std::mem::size_of::<u64>()
            + 4 * WINDOW_FRAMES * std::mem::size_of::<f32>()
            + 4 * std::mem::size_of::<f32>()
            + 5 * std::mem::size_of::<f32>();
        let aligned = expected.div_ceil(8) * 8; // u64 alignment of field 1
        assert_eq!(std::mem::size_of::<Frame>(), aligned.max(8));
    }

    #[test]
    fn engine_keeps_l_and_r_and_flags_mono_like() {
        let mut engine = Engine::new();
        // Identical channels (mono monitor style) → mono_like.
        let mono = sine_interleaved(220.0, WINDOW_FRAMES);
        let frame = engine.process_window(&mono).unwrap();
        assert!(frame.mono_like, "identical channels must be mono-like");
        assert_eq!(frame.left[13], frame.right[13]);

        // Hard-panned stereo: L only → not mono_like, L preserved, R silent.
        let mut inter = vec![0.0f32; WINDOW_FRAMES * CHANNELS];
        for k in 0..WINDOW_FRAMES {
            inter[k * CHANNELS] = 0.5;
        }
        let frame = engine.process_window(&inter).unwrap();
        assert!(!frame.mono_like, "L≠R must not be mono-like");
        assert_eq!(frame.left[100], 0.5);
        assert_eq!(frame.right[100], 0.0);
        assert_eq!(frame.mono[100], 0.25);
    }
}
