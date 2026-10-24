//! SPSC `Frame` ring between the DSP thread (producer) and the render
//! thread (consumer), PLAN.md §2.3. Capacity 256 slices ≈ 5.5 s — the ring
//! exists to absorb render hiccups, never to queue long; if it fills the
//! DSP thread drops *newest* (negligible: the consumer drains every wake at
//! frame cadence, and at 60 fps it needs ≤3 slices/wake for 60 Hz audio).
//!
//! Semantics tested headlessly here:
//! - `drain_into` pops **oldest-first** (FIFO) up to `max`;
//! - pushing into a full ring fails (`try_push` → `None`) rather than
//!   blocking the DSP thread;
//! - the buffer is reused across drains (no per-wake allocation once warm).

use ringbuf::{
    HeapRb, SharedRb, storage::Heap,
    traits::{Consumer as _, Split as _},
    wrap::caching::{CachingCons, CachingProd},
};
use std::sync::Arc;

use neopeek_dsp::Frame;

/// Ring capacity: 256 × `Frame` (≈4 MB with L/R storage).
pub const FRAME_RING_FRAMES: usize = 256;

pub type FrameRb = SharedRb<Heap<Frame>>;
pub type FrameProd = CachingProd<Arc<FrameRb>>;
pub type FrameCons = CachingCons<Arc<FrameRb>>;

/// Split a fresh ring into (producer, consumer).
pub fn frame_pair() -> (FrameProd, FrameCons) {
    HeapRb::<Frame>::new(FRAME_RING_FRAMES).split()
}

/// Pops up to `max` frames oldest-first into `out` (which retains its
/// capacity across calls). Returns the count drained.
pub fn drain_into(cons: &mut FrameCons, out: &mut Vec<Frame>, max: usize) -> usize {
    let mut n = 0;
    while n < max {
        let Some(frame) = cons.try_pop() else {
            break;
        };
        out.push(frame);
        n += 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::traits::Producer as _;

    fn frame(seq: u64) -> Frame {
        Frame {
            seq,
            ..Frame::default()
        }
    }

    // Frame needs Default for the test ring; check it compiles.
    #[test]
    fn drain_is_fifo_oldest_first() {
        let (mut prod, mut cons) = frame_pair();
        for seq in 0..8u64 {
            assert!(prod.try_push(frame(seq)).is_ok());
        }
        out_is_fifo(&mut cons);
    }

    fn out_is_fifo(cons: &mut FrameCons) {
        let mut out = Vec::with_capacity(FRAME_RING_FRAMES);
        let n = drain_into(cons, &mut out, 16);
        assert_eq!(n, 8);
        for (i, f) in out.iter().enumerate() {
            assert_eq!(f.seq, i as u64, "FIFO order violated at {i}");
        }
        // The next drain finds nothing.
        let mut out2 = Vec::new();
        assert_eq!(drain_into(cons, &mut out2, 16), 0);
        assert_eq!(out2.len(), 0);
    }

    #[test]
    fn full_ring_rejects_new_frames_without_blocking() {
        let (mut prod, mut cons) = frame_pair();
        let mut pushed = 0;
        while prod.try_push(frame(pushed as u64)).is_ok() {
            pushed += 1;
        }
        // The consumer is idle: the ring fills to capacity exactly.
        assert_eq!(pushed, FRAME_RING_FRAMES, "ring did not fill: {pushed}");
        // …and the next push is refused rather than blocking.
        assert!(prod.try_push(frame(u64::MAX)).is_err());
        // Draining oldest-first frees a slot for exactly one new frame.
        let mut out = Vec::new();
        assert_eq!(drain_into(&mut cons, &mut out, 1), 1);
        assert_eq!(out[0].seq, 0);
        assert!(prod.try_push(frame(u64::MAX)).is_ok());
    }

    #[test]
    fn drain_capacity_is_reused() {
        let (mut prod, mut cons) = frame_pair();
        let mut out: Vec<Frame> = Vec::with_capacity(FRAME_RING_FRAMES);
        for seq in 0..10u64 {
            prod.try_push(frame(seq)).unwrap();
        }
        assert_eq!(drain_into(&mut cons, &mut out, FRAME_RING_FRAMES), 10);
        for seq in 10..20u64 {
            prod.try_push(frame(seq)).unwrap();
        }
        assert_eq!(drain_into(&mut cons, &mut out, FRAME_RING_FRAMES), 10);
        // Vec was reserved once — capacity never shrank below what the
        // first drain needed.
        assert!(out.capacity() >= FRAME_RING_FRAMES);
    }
}
