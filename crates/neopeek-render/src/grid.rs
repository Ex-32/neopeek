//! CPU-side precompute of the waterfall grid (PLAN.md §4.1): one static
//! vertex per (slice, bin) — DEFAULT_DEPTH × 1024 — plus a per-slice
//! line-list index buffer. Rebuilding this allocates only when a key
//! *changes* the log factor (`c`/`v`) or the history depth (grid+index
//! buffers rebuilt); per-frame work is a GPU-only texture write.
//!
//! Pure functions here are unit-tested without a GPU.

use bytemuck::Pod;
use bytemuck::Zeroable;
use neopeek_dsp::BINS;

/// Default history slices; runtime-adjustable via `PageUp`/`PageDown` and
/// `--depth`, clamped by [`MIN_DEPTH`]…[`MAX_DEPTH`].
pub const DEFAULT_DEPTH: usize = 48;
/// History depth clamp: 8…256 layers.
pub const MIN_DEPTH: usize = 8;
pub const MAX_DEPTH: usize = 256;

/// Clamp a requested history depth to [`MIN_DEPTH`]…[`MAX_DEPTH`].
pub fn clamp_depth(depth: usize) -> usize {
    depth.clamp(MIN_DEPTH, MAX_DEPTH)
}

/// Horizontal waterfall/waveform span, x ∈ −X_SPAN…X_SPAN.
pub const X_SPAN: f32 = 1.8;

/// Depth-cue factor per slice: 1.0 = newest, →1/d adjust older;
/// `drift` animates the fade between slice arrivals (shader recomputes it).
pub fn depth_factor(depth: usize, slice: usize, drift: f32) -> f32 {
    (depth as f32 - slice as f32 + drift) / depth as f32
}

/// History-texture row of `slice` slices ago (0 = newest), given `head` =
/// index of the *next* row to be written. Newest written row is
/// `(head + n − 1) mod n`. Wrap handled in the shader the same way.
pub fn history_row(head: usize, slice: usize, n: usize) -> usize {
    (head + n - 1 - slice % n) % n
}

/// Normalized x positions in 0…1 for bins 0…BINS with a log factor.
///
/// Clean-room definition (behaviour-matched, not ported): factor 0 is the
/// linear axis (sndpeek's `g_log_factor = 1.0` default); larger factors
/// give more x-space to low-frequency bins, countering the "bins squashed
/// toward the right" effect of the plain linear axis (research lesson 5).
/// We default to **1.5**:
///
/// `n(t) = ln(1 + f·t) / ln(1 + f)`, `t = k / (BINS − 1)`.
pub fn normalized_positions(factor: f32) -> Vec<f32> {
    let f = factor.max(0.0);
    let denom = (1.0 + f).ln();
    (0..BINS)
        .map(|k| {
            let t = k as f32 / (BINS - 1) as f32;
            if denom <= f32::EPSILON {
                t // f = 0 → exactly linear
            } else {
                (1.0 + f * t).ln() / denom
            }
        })
        .collect()
}

/// x position (−X_SPAN…X_SPAN) for normalized coordinate `n ∈ 0…1`.
pub fn x_from_normalized(n: f32) -> f32 {
    -X_SPAN + 2.0 * X_SPAN * n
}

/// One waterfall grid vertex: precomputed x plus (bin, slice) indexes used
/// to fetch magnitudes from the history texture in the shader. 12 bytes;
/// DEPTH·1024 of these ≈ 0.6 MB for the default depth, rebuilt on a
/// depth change.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct GridVertex {
    pub x: f32,
    pub bin: u32,
    pub slice: u32,
}

/// Grid size depends only on `depth`: [`grid_vertices`] produces exactly
/// this many vertices for *every* factor. That invariant is what lets
/// `Waterfall::set_log_factor` rewrite the grid vertex buffer in place —
/// same byte length, so the queued `write_buffer` always fits — with
/// factor-dependent *content*.
pub fn grid_vertex_count(depth: usize) -> usize {
    depth * BINS
}

/// Build the full grid (positions fresh every call — callers cache it and
/// only rebuild on a log-factor or depth change).
pub fn grid_vertices(factor: f32, depth: usize) -> Vec<GridVertex> {
    let pos = normalized_positions(factor);
    let mut xs = vec![0.0f32; BINS];
    for (k, n) in pos.iter().enumerate() {
        xs[k] = x_from_normalized(*n);
    }
    let mut out = Vec::with_capacity(grid_vertex_count(depth));
    for slice in 0..depth {
        for (bin, &x) in xs.iter().enumerate() {
            out.push(GridVertex {
                x,
                bin: bin as u32,
                slice: slice as u32,
            });
        }
    }
    out
}

/// Line-list indices: per slice, consecutive bin pairs form a line strip
/// (2 indices per segment, sndpeek's `GL_LINE_STRIP` look).
pub const SLICE_SEGMENTS: usize = BINS - 1;

pub fn grid_indices(depth: usize) -> Vec<u32> {
    let mut out = Vec::with_capacity(depth * SLICE_SEGMENTS * 2);
    for slice in 0..depth {
        let base = (slice * BINS) as u32;
        for k in 0..SLICE_SEGMENTS as u32 {
            out.push(base + k);
            out.push(base + k + 1);
        }
    }
    out
}

/// Filled-surface mode (M8): the same grid drawn as triangles — one quad
/// (2 triangles, 6 indices) per (slice, bin segment) between adjacent
/// slices, wound so both faces render (front-face agnostic camera).
pub fn surface_indices(depth: usize) -> Vec<u32> {
    let mut out = Vec::with_capacity((depth - 1) * SLICE_SEGMENTS * 6);
    for slice in 0..depth - 1 {
        let a = (slice * BINS) as u32;
        let b = ((slice + 1) * BINS) as u32;
        for k in 0..SLICE_SEGMENTS as u32 {
            // Quad (a[k], a[k+1], b[k+1], b[k]) as two triangles.
            out.extend_from_slice(&[a + k, a + k + 1, b + k + 1]);
            out.extend_from_slice(&[a + k, b + k + 1, b + k]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32, tol: f32) {
        assert!((a - b).abs() < tol, "{a} vs {b}");
    }

    #[test]
    fn linear_when_factor_zero() {
        let p = normalized_positions(0.0);
        approx(p[0], 0.0, 1e-6);
        approx(p[BINS - 1], 1.0, 1e-6);
        approx(p[512], 512.0 / (BINS - 1) as f32, 1e-6);
    }

    #[test]
    fn log_factor_expands_low_end_monotonically() {
        let p = normalized_positions(1.5);
        // Ends pinned to the same span.
        approx(p[0], 0.0, 1e-6);
        approx(p[BINS - 1], 1.0, 1e-6);
        // Low half takes more than half the axis (the "squashed toward the
        // right" fix).
        assert!(p[BINS / 2 - 2] > 0.55, "mid {} want > 0.55", p[BINS / 2 - 2]);
        // Strictly increasing over bins.
        for w in p.windows(2) {
            assert!(w[1] > w[0]);
        }
    }

    #[test]
    fn grid_is_depth_by_bins_equally_spaced_slices() {
        let g = grid_vertices(1.5, DEFAULT_DEPTH);
        assert_eq!(g.len(), DEFAULT_DEPTH * BINS);
        // Slice s offsets its row: vertex (s, 0) has slice tag s and
        // shares x with vertex (0, 0)-column (log positions repeat).
        assert_eq!(g[0].slice, 0);
        assert_eq!(g[BINS].slice, 1);
        assert_eq!(g[BINS].bin, 0);
        assert_eq!(g[0].x, g[BINS].x);
        // x range spans −X_SPAN…X_SPAN with ends pinned.
        approx(g[0].x, -X_SPAN, 1e-3);
        approx(g[BINS - 1].x, X_SPAN, 1e-3);
    }

    /// Depth is a builder argument: vertex count and slice tags follow it.
    #[test]
    fn grid_vertices_follow_depth_arg() {
        let g = grid_vertices(0.0, 4);
        assert_eq!(g.len(), 4 * BINS);
        assert_eq!(g[3 * BINS].slice, 3);
        // Slices repeat the same x per row.
        assert_eq!(g[3 * BINS].x, g[0].x);
        assert_eq!(g[3 * BINS + BINS - 1].x, g[BINS - 1].x);
    }

    /// Vertex *content* changes with the factor across the c/v machine's
    /// whole reachable domain — including the smallest reachable step
    /// (one `c` from the floor snap). This pins the vertex buffer image
    /// the waterfall rebuild actually pushes; a value change that left
    /// `grid_vertices` identical would be the "INFO changed but the axis
    /// did not" failure mode.
    #[test]
    fn factor_changes_grid_x_content() {
        let mid = BINS / 2 - 1;
        let at = |f: f32, bin: usize| grid_vertices(f, 2)[bin].x;
        let lin = at(0.0, mid);
        let default = at(1.5, mid);
        let tiny = at(0.05, mid);
        assert!(tiny > lin && tiny < default, "one c-step off the floor sits between linear ({lin}) and default ({default}), got {tiny}");
        assert!(at(8.0, mid) > default + 1e-2, "ceiling factor spreads the mid-grid point, got {} vs {default}", at(8.0, mid));
        // The sndpeek property at grid level: the low end gains x-space
        // under larger factors (first segment widens).
        let steal = grid_vertices(0.0, 2);
        let spread = grid_vertices(1.5, 2);
        assert!(
            spread[1].x - spread[0].x > steal[1].x - steal[0].x,
            "low-end segment widens with the factor"
        );
    }

    /// Vertex count is factor-independent (depth is the only determinant)
    /// — the byte-length half of the in-place `write_buffer` invariant.
    #[test]
    fn grid_vertex_count_ignores_factor() {
        assert_eq!(grid_vertex_count(DEFAULT_DEPTH), DEFAULT_DEPTH * BINS);
        for depth in [MIN_DEPTH, 13, 48, 100, MAX_DEPTH] {
            assert_eq!(grid_vertices(0.0, depth).len(), grid_vertex_count(depth));
            assert_eq!(
                grid_vertices(8.0, depth).len(),
                grid_vertices(1.5, depth).len(),
                "upload byte length must not depend on the factor"
            );
        }
    }

    #[test]
    fn indices_form_per_slice_strips() {
        for depth in [2, DEFAULT_DEPTH, 13] {
            let idx = grid_indices(depth);
            assert_eq!(idx.len(), depth * (BINS - 1) * 2);
            // First slice: consecutive pairs (0,1),(1,2)…
            assert_eq!(idx[0], 0);
            assert_eq!(idx[1], 1);
            assert_eq!(idx[2], 1);
            assert_eq!(idx[3], 2);
            // Second slice starts at its own base, not sharing the first's last.
            let s1 = (BINS - 1) * 2;
            assert_eq!(idx[s1], BINS as u32);
            assert_eq!(idx[s1 + 1], BINS as u32 + 1);
        }
    }

    #[test]
    fn surface_indices_tile_the_grid() {
        for depth in [2, DEFAULT_DEPTH, 13] {
            let idx = surface_indices(depth);
            assert_eq!(idx.len(), (depth - 1) * (BINS - 1) * 6);
            // First quad: slice 0/1, bins 0/1 → triangle 1 = (0, 1, BINS+1),
            // triangle 2 = (0, BINS+1, BINS).
            assert_eq!(idx[0], 0);
            assert_eq!(idx[1], 1);
            assert_eq!(idx[2], BINS as u32 + 1);
            assert_eq!(idx[3], 0);
            assert_eq!(idx[4], BINS as u32 + 1);
            assert_eq!(idx[5], BINS as u32);
            // Every index within the grid; slices adjacent.
            let max = idx.iter().copied().max().unwrap();
            assert_eq!(max, (depth * BINS - 1) as u32);
        }
    }

    /// Clamp shared by the CLI flag and the runtime keys.
    #[test]
    fn clamp_depth_pins_range() {
        assert_eq!(clamp_depth(0), MIN_DEPTH);
        assert_eq!(clamp_depth(48), 48);
        assert_eq!(clamp_depth(1_000_000), MAX_DEPTH);
    }

    #[test]
    fn history_row_wrap_around() {
        let n = 4;
        // head = next-to-write; newest written = head+n−1 mod n.
        assert_eq!(history_row(0, 0, n), 3);
        assert_eq!(history_row(0, 1, n), 2);
        assert_eq!(history_row(0, 3, n), 0);
        // After writing one more (head=1), the newest is row 0.
        assert_eq!(history_row(1, 0, n), 0);
        assert_eq!(history_row(1, 1, n), 3);
        // All 48 slices in one head state hit distinct rows.
        let rows: Vec<usize> = (0..48).map(|s| history_row(7, s, 48)).collect();
        let mut uniq = rows.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), 48);
    }
}

