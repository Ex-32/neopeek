//! neopeek-audio: audio capture backends, device discovery, PCM feed.
//!
//! M2 scope (PLAN.md §5): PulseAudio-protocol capture via `libpulse-binding`
//! — covers PipeWire hosts transparently through `pipewire-pulse`. Default
//! is capture of the default sink's monitor: "what the host hears".
//!
//! Threading model (PLAN.md §2.3): one capture thread of ours owns the
//! libpulse `Mainloop`, `Context`, and `Stream` entirely (the binding hides
//! the stream pointer from its callbacks, so we use the *polled* model —
//! `iterate` to wake up on data, then `peek`/`discard` to drain). A 50 ms
//! timer event keeps the loop wakeable so shutdown is graceful (≤100 ms).
//! Samples are pushed into a lock-free SPSC ring; on ring overflow the
//! tail of the chunk is dropped (the audio server is never blocked), and
//! the DSP side will skip forward to the newest data (drop-oldest overall).
//!
//! ALSA fallback lives in `alsa.rs` once `cpal` lands (notification in the
//! fallback chain, PLAN.md §3.3).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use libpulse_binding as pulse;
use pulse::mainloop::api::Mainloop as MainloopTrait;
use pulse::mainloop::standard::{IterateResult, Mainloop};
use pulse::stream::{PeekResult, State as StreamState};
use pulse::time::{MicroSeconds, MonotonicTs};
use ringbuf::{
    HeapRb, SharedRb, storage::Heap,
    traits::{Producer, Split},
    wrap::caching::{CachingCons, CachingProd},
};
use tracing::{debug, info};

/// Requested capture layout (sndpeek-parity constants, research doc):
/// stereo float32, 48 kHz (Linux default), 1024-frame chunks — one
/// waterfall slice per chunk (≈21.3 ms @ 48 kHz).
pub const CHANNELS: usize = 2;
pub const REQUESTED_RATE: u32 = 48_000;
pub const CHUNK_FRAMES: usize = 1024;

/// Bytes per chunk of interleaved f32 stereo PCM.
pub const CHUNK_BYTES: usize = CHUNK_FRAMES * CHANNELS * 4;

/// PCM ring capacity: ~1 s of interleaved stereo f32 at the requested rate.
const PCM_RING_FRAMES: usize = REQUESTED_RATE as usize * CHANNELS;

/// Wakeup period for the stop-polling heartbeat.
const STOP_CHECK_PERIOD_US: u64 = 50_000;

pub type PcmRb = SharedRb<Heap<f32>>;
pub type PcmProd = CachingProd<Arc<PcmRb>>;
pub type PcmCons = CachingCons<Arc<PcmRb>>;

/// Startup handshake payload: negotiated (source, rate, channels), or
/// the failure that prevented reaching stream-Ready.
type HandshakeMsg = Result<(String, u32, u8), CaptureError>;

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("libpulse unavailable (library or daemon missing)")]
    NoContext,
    #[error("pulse: {0}")]
    Pulse(String),
    #[error("capture stream failed to become ready")]
    NotReady,
    #[error("handshake with capture thread failed: {0}")]
    Handshake(String),
    #[error("server negotiated non-f32 format ({0}); interleaved f32 parsing would be invalid")]
    UnsupportedNegotiatedFormat(String),
}

impl From<pulse::error::PAErr> for CaptureError {
    fn from(err: pulse::error::PAErr) -> Self {
        CaptureError::Pulse(format!("{err:?}"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// PulseAudio source name (e.g. `alsa_output.pci-0000_….monitor`).
    pub name: String,
    /// Human description (e.g. "Built-in Audio Analog Stereo").
    pub description: String,
    /// `true` for sink monitors (capture of "what the host hears").
    pub monitor: bool,
    pub rate: u32,
    pub channels: u8,
}

/// Number of PCM samples dropped due to ring overflow since stream start.
pub type OverflowCounter = Arc<AtomicU64>;

/// Incremented once when the stream dies *after* the startup handshake
/// (0 = live/never-lost). Lightweight late-failure signal for consumers
/// that no longer hold the handshake channel (see [`Capture::lost`]).
pub type LostCounter = Arc<AtomicU64>;

fn bump_overflow(counter: &OverflowCounter, n: u64) {
    if n > 0 {
        counter.fetch_add(n, Ordering::Relaxed);
    }
}

/// A live capture thread plus its info; the consumer end of the PCM ring
/// is handed out by [`Capture::start`]. Dropping `Capture` stops the
/// thread gracefully (≤ ~100 ms) and joins it.
pub struct Capture {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    overflow: OverflowCounter,
    lost: LostCounter,
    pub source_name: String,
    pub rate: u32,
    pub channels: u8,
}

impl Capture {
    /// Start capturing. `device` selects a source by name; `None` records
    /// the default sink's monitor (on PipeWire hosts this automatically
    /// rides `pipewire-pulse`).
    pub fn start(device: Option<String>) -> Result<(Self, PcmCons), CaptureError> {
        let (prod, cons) = HeapRb::<f32>::new(PCM_RING_FRAMES).split();
        let overflow: OverflowCounter = Arc::new(AtomicU64::new(0));
        let lost: LostCounter = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel::<HandshakeMsg>();

        let handle = std::thread::Builder::new()
            .name("neopeek-capture".into())
            .spawn({
                let overflow = overflow.clone();
                let lost = lost.clone();
                let stop = stop.clone();
                move || capture_thread(device, prod, overflow, lost, stop, tx)
            })
            .map_err(|err| CaptureError::Handshake(err.to_string()))?;

        let result = rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(handshake_recv_error);
        match result {
            Ok(Ok((source_name, rate, channels))) => {
                info!(source = %source_name, rate, "capture started");
                Ok((
                    Self {
                        stop,
                        handle: Some(handle),
                        overflow,
                        lost,
                        source_name,
                        rate,
                        channels,
                    },
                    cons,
                ))
            }
            Ok(Err(err)) => {
                // capture thread failed; join it before returning
                let _ = handle.join();
                Err(err)
            }
            Err(err) => {
                self_stop(&stop, handle);
                Err(err)
            }
        }
    }

    /// Total PCM samples dropped by the capture ring so far (diagnostics).
    pub fn overflow(&self) -> u64 {
        self.overflow.load(Ordering::Relaxed)
    }

    /// `true` once the stream has failed *after* a successful startup
    /// handshake (device unplugged, server died, …). The capture thread
    /// has exited; the PCM ring simply goes silent. A transient log line
    /// (`capture lost: …`) accompanies the transition.
    pub fn lost(&self) -> bool {
        self.lost.load(Ordering::Acquire) != 0
    }
}

/// Classify the handshake-channel wait failure for the error report.
fn handshake_recv_error(err: std::sync::mpsc::RecvTimeoutError) -> CaptureError {
    use std::sync::mpsc::RecvTimeoutError;
    match err {
        RecvTimeoutError::Timeout => {
            CaptureError::Handshake("timed out waiting on channel".into())
        }
        RecvTimeoutError::Disconnected => {
            CaptureError::Handshake("capture thread exited before handshake".into())
        }
    }
}

/// Stop + join on the aborted-startup path (timeout before handshake).
fn self_stop(stop: &std::sync::atomic::AtomicBool, handle: std::thread::JoinHandle<()>) {
    stop.store(true, Ordering::Release);
    let _ = handle.join();
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            // The heartbeat timer guarantees the thread checks `stop`
            // within ~100 ms even while no audio flows.
            let _ = handle.join();
        }
    }
}

fn capture_thread(
    device: Option<String>,
    mut prod: PcmProd,
    overflow: OverflowCounter,
    lost: LostCounter,
    stop: Arc<AtomicBool>,
    tx: std::sync::mpsc::Sender<HandshakeMsg>,
) {
    // `capture_thread_run` delivers the startup handshake itself, at the
    // moment the stream reaches Ready (it takes the `Sender` out of
    // `handshake` while doing so). If the function returns with the
    // channel still in place, startup failed before the handshake could
    // go out — deliver the real error so `Capture::start` reports it.
    let mut handshake = Some(tx);
    let outcome = capture_thread_run(device, &mut prod, &overflow, &stop, &mut handshake);
    if let Some(tx) = handshake {
        debug_assert!(outcome.is_err(), "run ended without sending the handshake");
        let _ = tx.send(outcome);
    } else if let Err(err) = outcome {
        // Late failure (stream/context died mid-drain): the consumer no
        // longer holds the handshake channel, so surface it via log +
        // the shared counter (`Capture::lost`).
        tracing::error!("capture lost: {err}");
        lost.fetch_add(1, Ordering::Release);
    }
}

fn capture_thread_run(
    device: Option<String>,
    prod: &mut PcmProd,
    overflow: &OverflowCounter,
    stop: &AtomicBool,
    handshake: &mut Option<std::sync::mpsc::Sender<HandshakeMsg>>,
) -> Result<(String, u32, u8), CaptureError> {
    let Some(mut ml) = Mainloop::new() else {
        return Err(CaptureError::NoContext);
    };

    // Heartbeat timer so every `iterate(true)` below — including the
    // connect/state waits before the stream exists — observes `stop`
    // even though `iterate` blocks on no other event traffic. The timer
    // re-arms itself on every call (periodic tick); kept alive to run
    // until thread exit. Created before anything connects so that a
    // stop during startup is honored within ~100 ms, too.
    let heartbeat_tv = MicroSeconds(STOP_CHECK_PERIOD_US);
    let _heartbeat = ml
        .new_timer_event_rt(
            MonotonicTs::now().checked_add(heartbeat_tv).unwrap_or_else(MonotonicTs::now),
            Box::new(move |mut ev: pulse::mainloop::events::timer::TimeEventRef<_>| {
                let next = MonotonicTs::now()
                    .checked_add(heartbeat_tv)
                    .unwrap_or_else(MonotonicTs::now);
                ev.restart_rt(next);
            }),
        )
        .ok_or_else(|| CaptureError::Pulse("alloc timer".into()))?;

    let Some(mut ctx) = pulse::context::Context::new(&ml, "neopeek") else {
        return Err(CaptureError::NoContext);
    };
    ctx.connect(None, pulse::context::FlagSet::NOFLAGS, None)
        .map_err(|err| CaptureError::Pulse(format!("connect: {err}")))?;
    loop {
        match ctx.get_state() {
            pulse::context::State::Ready => break,
            pulse::context::State::Failed | pulse::context::State::Terminated => {
                return Err(CaptureError::NoContext);
            }
            _ => {}
        }
        if stop.load(Ordering::Relaxed) {
            return Err(CaptureError::Handshake("stopped during startup".into()));
        }
        match ml.iterate(true) {
            IterateResult::Success(_) => {}
            IterateResult::Quit(_) => {
                return Err(CaptureError::Pulse("mainloop quit".into()));
            }
            IterateResult::Err(err) => {
                return Err(CaptureError::Pulse(format!("mainloop: {err:?}")));
            }
        }
    }

    // Resolve the source: explicit --device, else default sink's monitor.
    let source_name = match device.as_deref() {
        Some(name) => name.to_owned(),
        None => default_monitor_source(&mut ctx, &mut ml, stop)?,
    };

    let spec = pulse::sample::Spec {
        format: pulse::sample::Format::F32le,
        rate: REQUESTED_RATE,
        channels: CHANNELS as u8,
    };
    debug_assert!(spec.is_valid());
    let attr = pulse::def::BufferAttr {
        maxlength: u32::MAX,
        tlength: 0,
        prebuf: 0,
        minreq: 0,
        fragsize: CHUNK_BYTES as u32,
    };

    let mut stream =
        pulse::stream::Stream::new(&mut ctx, "neopeek-capture", &spec, None)
            .ok_or_else(|| CaptureError::Pulse("alloc stream".into()))?;
    stream
        .connect_record(
            Some(&source_name),
            Some(&attr),
            pulse::stream::FlagSet::ADJUST_LATENCY,
        )
        .map_err(|err| CaptureError::Pulse(format!("connect_record: {err}")))?;
    loop {
        match stream.get_state() {
            StreamState::Ready => break,
            StreamState::Failed | StreamState::Terminated => return Err(CaptureError::NotReady),
            _ => {}
        }
        if stop.load(Ordering::Relaxed) {
            return Err(CaptureError::NotReady);
        }
        match ml.iterate(true) {
            IterateResult::Success(_) => {}
            IterateResult::Quit(_) => {
                return Err(CaptureError::Pulse("mainloop quit".into()));
            }
            IterateResult::Err(err) => {
                return Err(CaptureError::Pulse(format!("mainloop: {err:?}")));
            }
        }
    }

    // The server may resample or change the channel count, so the *actual*
    // spec (what `drain_stream` will see on the wire) is queried from the
    // stream once it is Ready — not the requested one. `drain_stream`
    // assumes interleaved little-endian f32, which the request pins via
    // `Format::F32le`; any other negotiated format would make those parses
    // misinterpret the bytes, so it is refused rather than guessed at.
    // (Rare in practice: the server serves the requested format whenever
    // the source permits it, so the refusal is a safety net.)
    let Some(negotiated) = stream.get_sample_spec() else {
        return Err(CaptureError::Pulse("sample spec unavailable".into()));
    };
    let (rate, channels) = negotiated_spec(*negotiated)?;

    // Handshake goes out NOW — context and stream are Ready and the
    // negotiated spec is validated — while this same thread continues
    // into the drain loop below. (Historically the result was sent only
    // when this function returned, i.e. at shutdown or on error, so
    // `Capture::start` always timed out on a healthy server.)
    if let Some(tx) = handshake.take() {
        let _ = tx.send(Ok((source_name.clone(), rate, channels)));
    }

    // Polled drain loop: `iterate(true)` wakes on the record socket (or the
    // heartbeat), then we consume everything locally pending.
    while !stop.load(Ordering::Acquire) {
        match ml.iterate(true) {
            IterateResult::Success(_) => {}
            IterateResult::Quit(_) => break,
            IterateResult::Err(err) => {
                return Err(CaptureError::Pulse(format!("mainloop: {err:?}")));
            }
        }
        drain_stream(&mut stream, prod, overflow);
    }

    Ok((source_name, rate, channels))
}

/// Validate the negotiated sample spec: `drain_stream` parses the wire
/// bytes as interleaved little-endian f32, so any other negotiated
/// format would make those parses misinterpret the bytes and is refused
/// rather than guessed at. Returns the (rate, channels) to report in
/// the handshake. Pure so the accept/refuse decision is unit-testable.
fn negotiated_spec(spec: pulse::sample::Spec) -> Result<(u32, u8), CaptureError> {
    if spec.format != pulse::sample::Format::F32le {
        return Err(CaptureError::UnsupportedNegotiatedFormat(
            format!("{:?}", spec.format),
        ));
    }
    Ok((spec.rate, spec.channels))
}

fn drain_stream(
    stream: &mut pulse::stream::Stream,
    prod: &mut PcmProd,
    overflow: &OverflowCounter,
) {
    // No scratch buffer: samples are parsed straight into the SPSC ring via
    // `push_iter`, so the hot path performs zero heap allocation regardless
    // of how large a peek fragment the server hands us.
    while !stream.readable_size().is_none() {
        match stream.peek() {
            Ok(PeekResult::Data(data)) => {
                let total = data.len() / 4;
                let pushed = prod.push_iter(
                    data.chunks_exact(4)
                        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                );
                bump_overflow(overflow, (total - pushed) as u64);
                if pushed < total {
                    debug!(dropped = total - pushed, "pcm ring overflow");
                }
                let _ = stream.discard();
            }
            Ok(PeekResult::Hole(_)) => {
                let _ = stream.discard();
            }
            Ok(PeekResult::Empty) => break,
            Err(err) => {
                debug!("stream peek error: {err}");
                break;
            }
        }
    }
}

/// Resolve the monitor source of the default sink (e.g. `X.monitor`).
fn default_monitor_source(
    ctx: &mut pulse::context::Context,
    ml: &mut Mainloop,
    stop: &AtomicBool,
) -> Result<String, CaptureError> {
    let result: Arc<std::sync::Mutex<Option<String>>> = Arc::default();
    let share = result.clone();
    ctx.introspect().get_server_info(move |info| {
        if let Some(sink) = &info.default_sink_name {
            *share.lock().unwrap() = Some(format!("{sink}.monitor"));
        }
    });
    while result.lock().unwrap().is_none() {
        if stop.load(Ordering::Acquire) {
            return Err(CaptureError::Handshake("stopped during startup".into()));
        }
        if ml.iterate(true).is_error() {
            return Err(CaptureError::NoContext);
        }
    }
    let found = result.lock().unwrap().clone();
    found.ok_or(CaptureError::Pulse(
        "server reported no default sink; pass --device".into(),
    ))
}

/// Enumerate sources (mics + sink monitors). Blocking; short-lived context.
/// Used by `--list-devices`.
pub fn list_sources() -> Result<Vec<DeviceInfo>, CaptureError> {
    use pulse::callbacks::ListResult;
    use pulse::context::introspect::SourceInfo;

    let Some(mut ml) = Mainloop::new() else {
        return Err(CaptureError::NoContext);
    };
    let Some(mut ctx) = pulse::context::Context::new(&ml, "neopeek-list") else {
        return Err(CaptureError::NoContext);
    };
    ctx.connect(None, pulse::context::FlagSet::NOFLAGS, None)
        .map_err(|err| CaptureError::Pulse(format!("connect: {err}")))?;
    loop {
        match ctx.get_state() {
            pulse::context::State::Ready => break,
            pulse::context::State::Failed | pulse::context::State::Terminated => {
                return Err(CaptureError::NoContext);
            }
            _ => {}
        }
        if ml.iterate(true).is_error() {
            return Err(CaptureError::NoContext);
        }
    }

    let collector = Arc::new(std::sync::Mutex::new(Vec::<DeviceInfo>::new()));
    let done = Arc::new(AtomicBool::new(false));
    let done_for_cb = done.clone();
    let push = collector.clone();
    ctx.introspect().get_source_info_list(move |r: ListResult<&SourceInfo>| match r {
        ListResult::Item(info) => push.lock().unwrap().push(DeviceInfo {
            name: info.name.as_ref().map(|s| s.to_string()).unwrap_or_default(),
            description: info
                .description
                .as_ref()
                .map(|s| s.to_string())
                .unwrap_or_default(),
            monitor: info.monitor_of_sink.is_some(),
            rate: info.sample_spec.rate,
            channels: info.sample_spec.channels,
        }),
        ListResult::End | ListResult::Error => done_for_cb.store(true, Ordering::Release),
    });
    while !done.load(Ordering::Acquire) {
        if ml.iterate(true).is_error() {
            return Err(CaptureError::NoContext);
        }
    }
    let devices = collector.lock().unwrap().clone();
    debug!(n = devices.len(), "sources listed");
    Ok(devices)
}

/// Convert interleaved little-endian f32 PCM bytes into f32 samples.
pub fn convert_le_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::traits::Consumer;
    use std::time::Instant;

    #[test]
    fn convert_roundtrip() {
        let orig = [0.0f32, -1.0, 0.5, 1.0, -0.25];
        let mut bytes = Vec::with_capacity(orig.len() * 4);
        for s in orig {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        assert_eq!(convert_le_f32(&bytes), orig);
    }

    #[test]
    fn convert_ignores_partial_tail() {
        assert_eq!(convert_le_f32(&[0u8; 6]).len(), 1);
    }

    #[test]
    fn ring_push_slice_drops_tail_when_full() {
        let rb = HeapRb::<f32>::new(4);
        let (mut prod, mut cons) = rb.split();
        assert_eq!(prod.push_slice(&[1.0, 2.0, 3.0, 4.0]), 4);
        // full: newer slice dropped, oldest data preserved for slow readers
        assert_eq!(prod.push_slice(&[5.0, 6.0]), 0);
        let mut out = [0f32; 4];
        assert_eq!(cons.pop_slice(&mut out), 4);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(prod.push_slice(&[5.0, 6.0]), 2);
    }

    // ---- startup handshake & spec validation ---------------------------

    /// The negotiated spec is accepted when it matches what
    /// `drain_stream` parses (interleaved LE f32); rate/channels ride
    /// through as negotiated by the server.
    #[test]
    fn negotiated_spec_accepts_f32le() {
        let spec = pulse::sample::Spec {
            format: pulse::sample::Format::F32le,
            rate: 44_100, // server may resample below the requested 48k
            channels: 2,
        };
        assert_eq!(negotiated_spec(spec).unwrap(), (44_100, 2));
    }

    /// Any non-f32 negotiated format is refused before the handshake, so
    /// the parse in `drain_stream` can never reinterpret bytes.
    #[test]
    fn negotiated_spec_rejects_non_f32() {
        let spec = pulse::sample::Spec {
            format: pulse::sample::Format::S16le,
            rate: 48_000,
            channels: 2,
        };
        assert!(matches!(
            negotiated_spec(spec),
            Err(CaptureError::UnsupportedNegotiatedFormat(_))
        ));
    }

    /// A capture thread that exits with an error result (e.g. no audio
    /// server, as in this headless environment) makes `Capture::start`
    /// return that error and join cleanly — well before the 5 s timeout.
    /// On a host with a working server the call succeeds instead and
    /// there is nothing headless to assert here.
    #[test]
    fn start_reports_thread_error_promptly() {
        let started = Instant::now();
        match Capture::start(None) {
            Err(_) => assert!(
                started.elapsed() < Duration::from_secs(5),
                "startup error arrived after the handshake timeout ({started:?})"
            ),
            // A real server appeared: join cleanly, nothing to assert.
            Ok((capture, _cons)) => drop(capture),
        }
    }

    /// A capture thread that panics before the handshake must not wedge
    /// `Capture::start`: the sender is dropped → the recv sees
    /// Disconnected (not Timeout), and the panicked handle joins without
    /// hanging. Exercised through the same channel+join mechanics
    /// `start` uses, since the real thread needs a pulse server.
    #[test]
    fn start_mechanics_joins_panicked_thread() {
        let (tx, rx) = std::sync::mpsc::channel::<Result<(String, u32, u8), CaptureError>>();
        let handle = std::thread::Builder::new()
            .name("test-panic".into())
            .spawn(move || {
                drop(tx); // all senders gone → the recv must see Disconnected
                panic!("simulated capture thread crash");
            })
            .unwrap();
        let started = Instant::now();
        let err = handshake_recv_error(
            rx.recv_timeout(Duration::from_secs(1))
                .expect_err("sender dropped on panic"),
        );
        assert!(
            matches!(&err, CaptureError::Handshake(msg) if msg.contains("exited before handshake")),
            "unexpected classification: {err:?}"
        );
        // join on a panicked handle returns Err; must not hang.
        assert!(handle.join().is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    /// Timeout path: when nothing arrives, the wait fails with the
    /// timeout wording and the stop flag is raised so the thread exits;
    /// the join must complete promptly (the heartbeat guarantees the
    /// real thread wakes; here the test thread watches `stop` directly).
    #[test]
    fn start_mechanics_timeout_stops_and_joins() {
        let (tx, rx) = std::sync::mpsc::channel::<Result<(String, u32, u8), CaptureError>>();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let handle = std::thread::Builder::new()
            .name("test-idle".into())
            .spawn(move || {
                // Stand-in for `ml.iterate(true)` on the heartbeat: wake
                // periodically, honor stop.
                while !stop_thread.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                let _ = tx.send(Err(CaptureError::Handshake("stopped during startup".into())));
            })
            .unwrap();
        let started = Instant::now();
        let err = handshake_recv_error(
            rx.recv_timeout(Duration::from_millis(50))
                .expect_err("nothing sent within 50 ms"),
        );
        assert!(
            matches!(&err, CaptureError::Handshake(msg) if msg.contains("timed out")),
            "unexpected classification: {err:?}"
        );
        self_stop(&stop, handle);
        assert!(started.elapsed() < Duration::from_secs(2), "join did not complete promptly");
    }
}
