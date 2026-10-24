//! DSP thread: consumes the capture PCM ring and pushes [`Frame`]s into
//! the render frame ring (PLAN.md §2.3). Started with the window; joined
//! on exit. Hot path is allocation-free (fixed stack buffers; the ring
//! backpressure drops the *newest* frame, an event the ring's own count
//! tracks — combined with the render side always draining, drop-oldest is
//! what the consumer sees).
//!
//! Windows are exactly 1024 frames (≈21.3 ms @48 kHz), matching
//! `dump_fft`'s staging cadence: when the producer is only partially
//! filled the thread sleeps briefly; no busy spinning.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::thread::JoinHandle;

use neopeek_audio::{Capture, PcmCons, CHANNELS};
use neopeek_dsp::{Engine, WINDOW_FRAMES};
use neopeek_render::frame_ring::FrameProd;

// ringbuf trait methods (pop_slice, try_push).
use ringbuf::traits::{Consumer as _, Producer as _};

pub struct AudioPipeline {
    /// Kept alive for its `Drop` (graceful capture-thread stop); its
    /// diagnostics surface through `source_name`/`overflow`.
    capture: Capture,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    rate: u32,
}

impl AudioPipeline {
    /// Starts the capture handshake and then the DSP loop.
    pub fn start(device: Option<String>, frame_prod: FrameProd) -> Result<Self, CaptureError> {
        let (capture, pcm_cons) = Capture::start(device)?;
        let rate = capture.rate;
        let stop = Arc::new(AtomicBool::new(false));
        let handle = std::thread::Builder::new()
            .name("neopeek-dsp".into())
            .spawn({
                let stop = stop.clone();
                move || dsp_loop(pcm_cons, frame_prod, stop)
            })
            .map_err(|err| CaptureError::Handshake(err.to_string()))?;
        Ok(Self {
            capture,
            stop,
            handle: Some(handle),
            rate,
        })
    }

    /// Negotiated capture rate (drives the drift animation).
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Source name as negotiated by the capture handshake (HUD bottom line).
    pub fn source_name(&self) -> &str {
        &self.capture.source_name
    }

    /// `true` once the capture stream died *after* a successful startup
    /// handshake (device unplugged, server gone). The waterfall then
    /// renders flat; the app can use this for a HUD notice.
    pub fn capture_lost(&self) -> bool {
        self.capture.lost()
    }
}

impl Drop for AudioPipeline {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        // `Capture`'s own Drop joins the capture thread.
    }
}

fn dsp_loop(mut pcm: PcmCons, mut prod: FrameProd, stop: Arc<AtomicBool>) {
    let mut engine = Engine::new();
    let window_len = WINDOW_FRAMES * CHANNELS;
    let mut window_buf = [0.0f32; WINDOW_FRAMES * CHANNELS];
    let mut tmp = [0.0f32; 1024]; // pop scratch, reused
    let mut filled = 0usize;

    while !stop.load(Ordering::Acquire) {
        let popped = pcm.pop_slice(&mut tmp);
        let space = window_len - filled;
        let take = popped.min(space);
        window_buf[filled..filled + take].copy_from_slice(&tmp[..take]);
        filled += take;

        let mut processed = false;
        if filled == window_len {
            if let Some(frame) = engine.process_window(&window_buf) {
                // Full ring → drop the newest frame; the consumer drains
                // every wake at 60 fps, so this is a hiccup-only event.
                if let Err(_frame) = prod.try_push(frame) {
                    tracing::debug!("frame ring full; dropped one frame");
                }
            }
            filled = 0;
            processed = true;
        }

        if popped == 0 && !processed {
            // Nothing new this pass: a 2 ms nap keeps the thread wakeable
            // (stop is checked each loop) without burning a core.
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

type CaptureError = neopeek_audio::CaptureError;
