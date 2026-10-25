//! neopeek-dsp: analysis pipeline from interleaved PCM to FFT frames.
//!
//! M3 scope (PLAN.md §2.3, §4.1; constants verified against sndpeek-1.41 in
//! `docs/research-sndpeek.md`):
//! - per-frame stereo→mono average (L and R kept for meters/Lissajous upstream)
//! - Hanning window over the 1024-sample analysis window
//! - zero-padded 2048-point forward real FFT (rustfft) → 1024 magnitude bins
//! - feature extraction: spectral centroid, flux, rolloff at
//!   25%/50%/75%/80%
//!   (4-frame moving average smoothing, sndpeek `LP = 4` parity)
//!
//! All math is reimplemented fresh from definitions — nothing is ported
//! (clean-room rule). Bin magnitudes are stored raw; the dB/linear display
//! mapping and frequency-axis positioning belong to render/theme.

use rustfft::{FftPlanner, num_complex::Complex};

/// Audio samples per analysis window/hop (sndpeek `SND_BUFFER_SIZE`).
pub const WINDOW_FRAMES: usize = 1024;
/// Largest bin count any supported FFT size can produce
/// (`FFT_SIZES.last()/2 = 8192/2`).
pub const MAX_BINS: usize = 4096;
/// Adjustable FFT lengths (power-of-two ladder). The analysis window
/// stays [`WINDOW_FRAMES`], so larger sizes zero-pad it into more
/// (interpolated) bins — finer display granularity, **not** more true
/// resolution; sizes below the window analyze the newest `size` samples
/// of the window (see [`Engine::process_window`]). Kept to a fixed
/// ladder so FFT plans stay cheap and the bin math (log positions,
/// texture width, Hz mapping) has sane anchors.
pub const FFT_SIZES: [usize; 5] = [512, 1024, 2048, 4096, 8192];
/// Default FFT length (sndpeek parity: 1024-sample window zero-padded
/// to 2048).
pub const DEFAULT_FFT_SIZE: usize = 2048;
/// Magnitude bins of the default FFT (`DEFAULT_FFT_SIZE / 2`) — the
/// geometry the display defaults were tuned at; runtime sizes vary.
pub const DEFAULT_BINS: usize = DEFAULT_FFT_SIZE / 2;

/// Whether `fft_size` is one of the supported [`FFT_SIZES`].
pub fn is_fft_size(fft_size: usize) -> bool {
    FFT_SIZES.contains(&fft_size)
}

/// Maps a requested FFT size onto the ladder: valid sizes pass through,
/// anything else falls back to [`DEFAULT_FFT_SIZE`] (with a warning —
/// the CLI rejects invalid sizes, so this is a safety net, not a path).
fn normalize_fft_size(fft_size: usize) -> usize {
    if is_fft_size(fft_size) {
        fft_size
    } else {
        tracing::warn!(
            requested = fft_size,
            ladder = ?FFT_SIZES,
            "dsp: unsupported FFT size, falling back to the default"
        );
        DEFAULT_FFT_SIZE
    }
}
/// Channel count of interleaved input.
pub const CHANNELS: usize = 2;
/// Frames per feature moving average (sndpeek `LP`).
pub const LP_TAPS: usize = 4;

/// One waterfall slice plus live audio meters/features.
///
/// Fixed-size POD so it fits in a lock-free ring with no allocation. The
/// FFT size is runtime-adjustable, so the bin array is sized for the
/// largest supported transform (`MAX_BINS`) and the *valid* count rides
/// along in [`Frame::bin_count`]: only `bins[0..bin_count]` carries
/// spectrum, the tail is always zero-filled by the producer. Consumers
/// must bound their loops by `bin_count` (a zero tail cannot corrupt a
/// max/sum, but index-based feature math must not see the pad).
#[derive(Debug, Clone, Copy)]
pub struct Frame {
    pub seq: u64,
    /// `|X_k|` for k = 0..`bin_count` of the windowed, zero-padded FFT.
    pub bins: [f32; MAX_BINS],
    /// Valid magnitude-bin count (`fft_size / 2` of the producing engine).
    pub bin_count: u32,
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
    /// Lowest bin where cumulative magnitude crosses 25% / 75% of total —
    /// the *quartile* bars of the cursor overlay (sndpeek's overlay drew
    /// 50%/80% markers; here the centroid line is flanked by the 25%/75%
    /// points instead).
    pub rolloff25: f32,
    pub rolloff75: f32,
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
            bins: [0.0; MAX_BINS],
            bin_count: 0,
            mono: [0.0; WINDOW_FRAMES],
            left: [0.0; WINDOW_FRAMES],
            right: [0.0; WINDOW_FRAMES],
            mono_like: true,
            rms: [0.0; CHANNELS],
            peak: [0.0; CHANNELS],
            centroid: 0.0,
            flux: 0.0,
            rolloff25: 0.0,
            rolloff75: 0.0,
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
    /// Plan cache: rebuilding a plan for a ladder size reuses this
    /// planner's factorization cache (cheap on repeated `o`/`n` toggles).
    planner: FftPlanner<f32>,
    hanning: Vec<f32>,
    fft: std::sync::Arc<dyn rustfft::Fft<f32>>,
    scratch: Vec<Complex<f32>>,
    input: Vec<Complex<f32>>,
    /// Active FFT length (one of [`FFT_SIZES`]); `input.len() == fft_size`.
    fft_size: usize,
    /// Fixed-size mono downmix; converted to `[f32; WINDOW_FRAMES]` copies
    /// into `Frame` with no heap traffic on the ~21 ms hot path.
    mono: [f32; WINDOW_FRAMES],
    left: [f32; WINDOW_FRAMES],
    right: [f32; WINDOW_FRAMES],
    max_l_r_diff: f32,
    max_l_r_abs: f32,
    prev_bins: [f32; MAX_BINS],
    seq: u64,
    smooth_centroid: MovingAverage,
    smooth_flux: MovingAverage,
    smooth_rolloff25: MovingAverage,
    smooth_rolloff75: MovingAverage,
    smooth_rolloff50: MovingAverage,
    smooth_rolloff80: MovingAverage,
}

impl Engine {
    pub fn new(fft_size: usize) -> Self {
        let fft_size = normalize_fft_size(fft_size);
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(fft_size);
        let scratch = vec![Complex::new(0.0, 0.0); fft.get_inplace_scratch_len()];
        Self {
            planner,
            hanning: hanning(fft_size.min(WINDOW_FRAMES)),
            fft,
            scratch,
            input: vec![Complex::new(0.0, 0.0); fft_size],
            fft_size,
            mono: [0.0; WINDOW_FRAMES],
            left: [0.0; WINDOW_FRAMES],
            right: [0.0; WINDOW_FRAMES],
            max_l_r_diff: 0.0,
            max_l_r_abs: 0.0,
            prev_bins: [0.0; MAX_BINS],
            seq: 0,
            smooth_centroid: MovingAverage::default(),
            smooth_flux: MovingAverage::default(),
            smooth_rolloff25: MovingAverage::default(),
            smooth_rolloff75: MovingAverage::default(),
            smooth_rolloff50: MovingAverage::default(),
            smooth_rolloff80: MovingAverage::default(),
        }
    }

    /// The active FFT length (one of [`FFT_SIZES`]).
    pub fn fft_size(&self) -> usize {
        self.fft_size
    }

    /// Valid magnitude-bin count of the frames this engine produces
    /// (`fft_size / 2`).
    pub fn bins(&self) -> usize {
        self.fft_size / 2
    }

    /// Switches the FFT to a new ladder size: rebuilds the plan (through
    /// the cached planner), resizes the input/scratch buffers, and
    /// rebuilds the window for sizes below [`WINDOW_FRAMES`]. A size
    /// change is a *new spectrum geometry* — the flux continuity
    /// (`prev_bins`) and the 4-tap feature smoothing restart so features
    /// are computed strictly within the new bin space; `seq` keeps
    /// counting. A no-op when the size already matches.
    pub fn set_fft_size(&mut self, fft_size: usize) {
        let fft_size = normalize_fft_size(fft_size);
        if fft_size == self.fft_size {
            return;
        }
        let fft = self.planner.plan_fft_forward(fft_size);
        self.scratch = vec![Complex::new(0.0, 0.0); fft.get_inplace_scratch_len()];
        self.input = vec![Complex::new(0.0, 0.0); fft_size];
        self.fft = fft;
        self.fft_size = fft_size;
        self.hanning = hanning(fft_size.min(WINDOW_FRAMES));
        self.reset_features();
    }

    /// Clears the feature continuity state (`prev_bins` + moving
    /// averages) without touching the sequence counter.
    fn reset_features(&mut self) {
        self.prev_bins.fill(0.0);
        self.smooth_centroid.reset();
        self.smooth_flux.reset();
        self.smooth_rolloff25.reset();
        self.smooth_rolloff75.reset();
        self.smooth_rolloff50.reset();
        self.smooth_rolloff80.reset();
    }

    /// Processes exactly one window of interleaved stereo PCM.
    ///
    /// Input is sanitized first: any non-finite sample (NaN, ±inf) becomes
    /// 0.0. A PulseAudio/PipeWire MONITOR capture can legitimately contain
    /// non-finite f32s (misbehaving capture sources, garbage in the host
    /// float path), and one inf sample poisons the spectral features
    /// permanently: inf bins make `centroid` and `flux` inf/inf = NaN, and
    /// the 4-tap moving averages then hold NaN forever (they never recover).
    /// The waterfall is lost too — its `sqrt(25·mag)` y-mapping sends any
    /// non-finite bin's slice to a degenerate line-strip. Sanitizing at the
    /// single entry point keeps every downstream consumer (features, meters,
    /// waveform, Lissajous, `prev_bins` continuity) finite by construction.
    pub fn process_window(&mut self, interleaved: &[f32]) -> Option<Frame> {
        let want = WINDOW_FRAMES * CHANNELS;
        if interleaved.len() != want {
            tracing::warn!(got = interleaved.len(), want, "dsp: wrong window size");
            return None;
        }
        // One sanitize pass over the window; everything below reads only
        // sanitized values. Fixed-capacity stack buffer → allocation-free.
        let mut stored = [0.0f32; WINDOW_FRAMES * CHANNELS];
        for (dst, &src) in stored.iter_mut().zip(interleaved.iter()) {
            *dst = if src.is_finite() { src } else { 0.0 };
        }
        for k in 0..WINDOW_FRAMES {
            let l = stored[k * CHANNELS];
            let r = stored[k * CHANNELS + 1];
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
        let (rms, peak) = channel_levels(&stored);

        // Windowed input: for fft_size ≥ WINDOW_FRAMES the full 1024-
        // sample window is loaded and zero-padded (sndpeek's display
        // path); for fft_size = 512 the window would not fit, so the
        // FFT analyzes the newest 512 samples of the window (the tail —
        // the most recent audio) instead. The stored 1024-sample mono /
        // L / R history is unchanged either way (waveform, Lissajous).
        let analyze = self.fft_size.min(WINDOW_FRAMES);
        let skip = WINDOW_FRAMES - analyze;
        for (n, slot) in self.input[..analyze].iter_mut().enumerate() {
            let mono = self.mono[skip + n] * self.hanning[n];
            *slot = Complex::new(mono, 0.0); // real signal → zero imaginary
        }

        // Zero the pad region (window → fft_size; empty at fft_size =
        // 512): the FFT runs in place, so without this each frame would
        // start with the previous frame's spectrum in the pad bins. That
        // residue compounds across windows until the complex powers
        // overflow to inf — another way to freeze centroid/flux at NaN
        // even on a clean capture.
        self.input[analyze..].fill(Complex::ZERO);

        // rustfft transforms in place with a single scratch buffer.
        self.fft
            .process_with_scratch(&mut self.input, &mut self.scratch);

        // Magnitude bins for k = 0..fft_size/2 (real input ⇒ conjugate-
        // symmetric spectrum; the upper half carries no display data).
        // The mags tail beyond `bin_count` stays zero: `Frame` is POD
        // sized for MAX_BINS, and only `bins[0..bin_count]` is valid.
        let bin_count = self.input.len() / 2;
        let mut mags = [0f32; MAX_BINS];
        for (k, slot) in mags[..bin_count].iter_mut().enumerate() {
            let m =
                (self.input[k].re * self.input[k].re + self.input[k].im * self.input[k].im).sqrt();
            // Belt-and-braces on top of the input sanitize: keeps us
            // finite even if rustfft ever emits a denormal/rounding
            // surprise. A poisoned magnitude here would again drive
            // centroid/flux to inf/inf = NaN and freeze the smoothing
            // history forever.
            *slot = if m.is_finite() { m } else { 0.0 };
        }
        let valid = &mags[..bin_count];
        let raw_flux = flux(valid, &self.prev_bins[..bin_count]);
        let c = centroid(valid);
        let r50 = rolloff(valid, 0.5);
        let r80 = rolloff(valid, 0.8);
        let r25 = rolloff(valid, 0.25);
        let r75 = rolloff(valid, 0.75);
        self.prev_bins = mags;

        self.seq += 1;
        Some(Frame {
            seq: self.seq,
            bins: mags,
            bin_count: bin_count as u32,
            mono: self.mono,
            left: self.left,
            right: self.right,
            mono_like: self.max_l_r_diff <= (self.max_l_r_abs * 1e-3).max(1e-6),
            rms,
            peak,
            centroid: self.smooth_centroid.push(c),
            flux: self.smooth_flux.push(raw_flux),
            rolloff25: self.smooth_rolloff25.push(r25),
            rolloff75: self.smooth_rolloff75.push(r75),
            rolloff50: self.smooth_rolloff50.push(r50),
            rolloff80: self.smooth_rolloff80.push(r80),
        })
    }

    pub fn sequence(&self) -> u64 {
        self.seq
    }

    pub fn reset_history(&mut self) {
        self.reset_features();
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
        let f = 40.0 * RATE as f32 / DEFAULT_FFT_SIZE as f32;
        let mut engine = Engine::new(DEFAULT_FFT_SIZE);
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
        let f = 100.0 * RATE as f32 / DEFAULT_FFT_SIZE as f32;
        let mut engine = Engine::new(DEFAULT_FFT_SIZE);
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
        // Two-tone spectrum (bin 10 weight 1.0, bin 90 weight 3.0,
        // total 4): the cumulative magnitude sits flat between the tones,
        // so 25% (= 1.0) is crossed at bin 10 and 75% (= 3.0) at bin 90 —
        // the quartile crossings bracket the centroid (weighted mean = 70).
        let mut two_tone = vec![0.0f32; 128];
        two_tone[10] = 1.0;
        two_tone[90] = 3.0;
        let total = two_tone.iter().sum::<f32>();
        assert_eq!(rolloff(&two_tone, 0.25), 10.0);
        assert_eq!(rolloff(&two_tone, 0.75), 90.0);
        assert!((total - 4.0).abs() < 1e-6);
    }

    /// The engine's quartile rolloffs follow a known synthetic spectrum
    /// through the same 4-frame smoothing as centroid (constant input ⇒
    /// the smoothed value equals the raw metric; non-constant input ⇒
    /// they move together, tap for tap).
    #[test]
    fn quartile_rolloffs_follow_two_tone_and_smooth_like_centroid() {
        let mut engine = Engine::new(DEFAULT_FFT_SIZE);
        let mut frame = None;
        for _ in 0..4 {
            frame = engine.process_window(&sine_interleaved(
                90.0 * RATE as f32 / DEFAULT_FFT_SIZE as f32,
                WINDOW_FRAMES,
            ));
        }
        let frame = frame.unwrap();
        // One pure tone: every cumulative crossing lands at the same
        // peak bin — quartiles and centroid agree for a line spectrum.
        let peak = argmax(&frame.bins[..frame.bin_count as usize]) as f32;
        assert!(
            (frame.rolloff25 - peak).abs() <= 1.0
                && (frame.rolloff75 - peak).abs() <= 1.0,
            "quartiles {} / {} vs peak {}",
            frame.rolloff25,
            frame.rolloff75,
            peak
        );
        // Smoothing parity: on constant frames the smoothed engine value
        // equals what a raw MovingAverage over the same metric produces
        // — exactly how the smoothed centroid arrives too.
        let raw_valid = &frame.bins[..frame.bin_count as usize];
        assert_eq!(frame.rolloff25, rolloff(raw_valid, 0.25));
        assert_eq!(frame.rolloff75, rolloff(raw_valid, 0.75));
        assert_eq!(frame.centroid, centroid(raw_valid));
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

    /// User-reported symptom: a monitor stream containing one non-finite
    /// sample. Before the fix the FFT carried inf into the bins, so
    /// centroid/flux froze at NaN (inf/inf through their normalizations)
    /// and the waterfall's sqrt(25·mag) y-mapping clipped whole
    /// line-strips. The engine must sanitize instead.
    fn assert_finite_frame(frame: &Frame) {
        assert!(frame.bins.iter().all(|m| m.is_finite()), "non-finite bin");
        assert!(frame.mono.iter().all(|m| m.is_finite()));
        assert!(frame.left.iter().all(|m| m.is_finite()));
        assert!(frame.right.iter().all(|m| m.is_finite()));
        assert!(frame.rms.iter().all(|m| m.is_finite()));
        assert!(frame.peak.iter().all(|m| m.is_finite()));
        assert!(frame.centroid.is_finite());
        assert!(frame.flux.is_finite());
        assert!(frame.rolloff25.is_finite());
        assert!(frame.rolloff75.is_finite());
        assert!(frame.rolloff50.is_finite());
        assert!(frame.rolloff80.is_finite());
    }

    fn poisoned_window(bad: f32, at: usize) -> Vec<f32> {
        let mut window = sine_interleaved(40.0 * RATE as f32 / DEFAULT_FFT_SIZE as f32, WINDOW_FRAMES);
        window[at] = bad;
        window
    }

    #[test]
    fn one_inf_sample_yields_finite_frame_and_small_rms() {
        let mut engine = Engine::new(DEFAULT_FFT_SIZE);
        // Warm the smoothing history with clean frames first.
        for _ in 0..4 {
            let f = engine
                .process_window(&sine_interleaved(
                    40.0 * RATE as f32 / DEFAULT_FFT_SIZE as f32,
                    WINDOW_FRAMES,
                ))
                .expect("clean window");
            assert_finite_frame(&f);
        }
        for at in [0usize, 1, WINDOW_FRAMES * CHANNELS - 1] {
            let f = engine
                .process_window(&poisoned_window(f32::INFINITY, at))
                .expect("poisoned window");
            assert_finite_frame(&f);
            // inf → 0.0 sample: RMS is small and finite, never pegged inf.
            assert!(
                f.rms.iter().all(|r| r.is_finite() && *r < 1.0 && *r > 0.0),
                "rms {:?} should be finite and small",
                f.rms
            );
            assert!(f.peak.iter().all(|p| p.is_finite() && *p <= 1.0));
        }
    }

    #[test]
    fn one_nan_sample_yields_finite_frame() {
        let mut engine = Engine::new(DEFAULT_FFT_SIZE);
        for at in [0usize, 1, WINDOW_FRAMES * CHANNELS - 1] {
            let f = engine
                .process_window(&poisoned_window(f32::NAN, at))
                .expect("poisoned window");
            assert_finite_frame(&f);
        }
    }

    #[test]
    fn repeated_poisoned_windows_do_not_poison_the_history() {
        let mut engine = Engine::new(DEFAULT_FFT_SIZE);
        // Alternate clean and hostile windows; frames stay finite and the
        // 4-tap moving averages keep tracking (no NaN ever enters).
        for i in 0..12 {
            let bad = if i % 2 == 0 { f32::INFINITY } else { f32::NAN };
            let f = if i % 4 == 0 {
                engine
                    .process_window(&poisoned_window(bad, 5))
                    .expect("poisoned window")
            } else {
                engine
                    .process_window(&sine_interleaved(
                        40.0 * RATE as f32 / DEFAULT_FFT_SIZE as f32,
                        WINDOW_FRAMES,
                    ))
                    .expect("clean window")
            };
            assert_finite_frame(&f);
        }
        // After the storm, a clean frame lands back in its FFT bin: the
        // engine recovered, nothing is stuck frozen.
        let f = engine
            .process_window(&sine_interleaved(
                40.0 * RATE as f32 / DEFAULT_FFT_SIZE as f32,
                WINDOW_FRAMES,
            ))
            .expect("clean window");
        assert_finite_frame(&f);
        assert_eq!(argmax(&f.bins), 40);
    }

    #[test]
    fn engine_sequence_counts() {
        let mut engine = Engine::new(DEFAULT_FFT_SIZE);
        let f = 40.0 * RATE as f32 / DEFAULT_FFT_SIZE as f32;
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
        // MAX_BINS-magnitude array + mono/left/right (3×1024) + u64 seq +
        // u32 bin_count + 2×2 channel f32 (rms, peak) + 6 scalar f32
        // (centroid, flux, rolloff25/50/75/80) + 1 bool (mono_like).
        let expected = std::mem::size_of::<u64>()
            + MAX_BINS * std::mem::size_of::<f32>()
            + 3 * WINDOW_FRAMES * std::mem::size_of::<f32>()
            + std::mem::size_of::<u32>()
            + std::mem::size_of::<u8>() // mono_like
            + 4 * std::mem::size_of::<f32>()
            + 6 * std::mem::size_of::<f32>();
        let aligned = expected.div_ceil(8) * 8; // u64 alignment of field 1
        assert_eq!(std::mem::size_of::<Frame>(), aligned.max(8));
        // Ring budget: 256 frames ≈ 7 MB with the MAX_BINS-sized array.
        assert_eq!(
            std::mem::size_of::<Frame>(),
            28_728,
            "unexpected Frame growth"
        );
    }

    /// The producer fills only the valid head; the pad tail is zeroed so
    /// downstream max/sum scans never see stale magnitudes.
    #[test]
    fn frame_bins_tail_is_zeroed() {
        let mut engine = Engine::new(DEFAULT_FFT_SIZE);
        let frame = engine
            .process_window(&sine_interleaved(1000.0, WINDOW_FRAMES))
            .expect("window processed");
        assert_eq!(frame.bin_count, (DEFAULT_FFT_SIZE / 2) as u32);
        assert!(frame.bins[..frame.bin_count as usize].iter().any(|m| *m > 0.0));
        assert!(
            frame.bins[frame.bin_count as usize..].iter().all(|m| *m == 0.0),
            "pad tail must be zero-filled"
        );
    }

    #[test]
    fn engine_keeps_l_and_r_and_flags_mono_like() {
        let mut engine = Engine::new(DEFAULT_FFT_SIZE);
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

    /// The same sine must land in the same bin index at every ladder
    /// size — the bin frequency is `k·rate/fft_size`, so the sine is
    /// tuned per size. Bin-relative feature math (centroid etc.) is
    /// index-based, which is exactly why it transfers unchanged.
    #[test]
    fn sine_sits_in_its_bin_at_every_ladder_size() {
        for size in FFT_SIZES {
            let f = 40.0 * RATE as f32 / size as f32;
            let mut engine = Engine::new(size);
            assert_eq!(engine.fft_size(), size);
            assert_eq!(engine.bins(), size / 2);
            let frame = engine
                .process_window(&sine_interleaved(f, WINDOW_FRAMES))
                .expect("window processed");
            assert_eq!(frame.bin_count, (size / 2) as u32);
            assert_eq!(argmax(&frame.bins[..frame.bin_count as usize]), 40);
            let p = frame.bins[40];
            assert!(frame.bins[20] < 0.05 * p);
            assert!(frame.bins[100.min(frame.bin_count as usize - 1)] < 0.05 * p);
        }
    }

    /// The pad region must be cleared every frame: two identical
    /// windows in a row must produce identical spectra (without the
    /// clear, the second frame would start from the first frame's
    /// transformed pad residue).
    #[test]
    fn zero_pad_region_cleared_each_frame() {
        for size in [512usize, 4096] {
            let f = 40.0 * RATE as f32 / size as f32;
            let mut engine = Engine::new(size);
            let window = sine_interleaved(f, WINDOW_FRAMES);
            let first = engine.process_window(&window).unwrap();
            let second = engine.process_window(&window).unwrap();
            let n = first.bin_count as usize;
            assert_eq!(n, size / 2);
            for k in 0..n {
                assert_eq!(
                    first.bins[k], second.bins[k],
                    "bin {k} differs between identical windows at fft {size}"
                );
            }
        }
    }

    /// A size change is a new geometry: the feature continuity restarts
    /// (first frame after the switch carries zero flux) and the produced
    /// bin count follows the new size; features stay finite and
    /// bin-relative afterwards.
    #[test]
    fn set_fft_size_resets_features_and_follows_geometry() {
        let mut engine = Engine::new(DEFAULT_FFT_SIZE);
        let warm = sine_interleaved(40.0 * RATE as f32 / 2048.0, WINDOW_FRAMES);
        for _ in 0..4 {
            engine.process_window(&warm).unwrap();
        }
        engine.set_fft_size(8192);
        assert_eq!(engine.fft_size(), 8192);
        let f = 40.0 * RATE as f32 / 8192.0;
        let frame = engine
            .process_window(&sine_interleaved(f, WINDOW_FRAMES))
            .unwrap();
        assert_eq!(frame.bin_count, 4096);
        assert_eq!(frame.flux, 0.0, "prev_bins must be reset on a size change");
        assert_eq!(argmax(&frame.bins[..4096]), 40);
        assert!(frame.centroid.is_finite());
        assert!(frame.rolloff50.is_finite() && frame.rolloff80.is_finite());
        assert!(frame.rolloff25.is_finite() && frame.rolloff75.is_finite());
        // Back down the ladder: geometry follows again.
        engine.set_fft_size(512);
        let frame = engine
            .process_window(&sine_interleaved(40.0 * RATE as f32 / 512.0, WINDOW_FRAMES))
            .unwrap();
        assert_eq!(frame.bin_count, 256);
        assert_eq!(argmax(&frame.bins[..256]), 40);
        // Same size twice is a no-op (no feature reset).
        engine.set_fft_size(512);
        let frame = engine
            .process_window(&sine_interleaved(40.0 * RATE as f32 / 512.0, WINDOW_FRAMES))
            .unwrap();
        assert!(frame.flux >= 0.0);
    }

    /// Unsupported sizes never reach the plan: they normalize onto the
    /// default (CLI validation is the front line; this is the net).
    #[test]
    fn unsupported_sizes_fall_back_to_default() {
        assert_eq!(Engine::new(3000).fft_size(), DEFAULT_FFT_SIZE);
        assert_eq!(Engine::new(0).fft_size(), DEFAULT_FFT_SIZE);
        let mut engine = Engine::new(DEFAULT_FFT_SIZE);
        engine.set_fft_size(1 << 20);
        assert_eq!(engine.fft_size(), DEFAULT_FFT_SIZE);
        assert!(is_fft_size(DEFAULT_FFT_SIZE));
        for size in FFT_SIZES {
            assert!(is_fft_size(size));
        }
        assert!(!is_fft_size(1000));
    }

    /// At fft 512 the window (1024) overflows the transform: the engine
    /// must analyze the newest 512 samples of the window. A sine burst
    /// confined to the window's *first* half must then vanish from the
    /// spectrum (it is not among the analyzed samples), while one
    /// confined to the tail stays put.
    #[test]
    fn fft_below_window_size_analyzes_the_newest_tail() {
        let rate = RATE as f32;
        let mut engine = Engine::new(512);
        // Head-only burst: 512 samples of a sine in bin 40 (at fft 512),
        // then 512 silent samples → the analyzed tail is silent.
        let mut head_burst = vec![0.0f32; WINDOW_FRAMES * CHANNELS];
        for n in 0..WINDOW_FRAMES / 2 {
            let s = (std::f32::consts::TAU * 40.0 * rate / 512.0 * n as f32 / rate).sin();
            head_burst[n * CHANNELS] = s;
            head_burst[n * CHANNELS + 1] = s;
        }
        let frame = engine.process_window(&head_burst).unwrap();
        let energy: f32 = frame.bins[..256].iter().sum();
        assert!(energy < 1e-3, "head-only burst must not reach the 512-point FFT, got {energy}");

        // Tail-only burst: silence then the same sine → it must land in
        // bin 40.
        let mut tail_burst = vec![0.0f32; WINDOW_FRAMES * CHANNELS];
        for n in WINDOW_FRAMES / 2..WINDOW_FRAMES {
            let s = (std::f32::consts::TAU * 40.0 * rate / 512.0 * n as f32 / rate).sin();
            tail_burst[n * CHANNELS] = s;
            tail_burst[n * CHANNELS + 1] = s;
        }
        let frame = engine.process_window(&tail_burst).unwrap();
        assert_eq!(argmax(&frame.bins[..256]), 40);
        // The full mono window (both halves) is still stored untouched.
        assert_eq!(frame.mono[0], 0.0);
        assert!(frame.mono[WINDOW_FRAMES - 1].abs() > 0.0);
    }
}
