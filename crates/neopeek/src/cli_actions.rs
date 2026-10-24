//! Non-visual CLI actions for M2/M3: `--list-devices`, `--dump-pcm`, and
//! the `--dump-fft` pipeline debug readout.

use std::io::Write;

use neopeek_audio::{Capture, CHANNELS};
use neopeek_config::Cli;
use ringbuf::traits::Consumer;

pub fn run(cli: &Cli) -> Result<(), String> {
    if cli.list_devices {
        let mut sources: Vec<_> = neopeek_audio::list_sources()
            .map_err(|err| err.to_string())?
            .into_iter()
            .collect();
        // Sink monitors first — they are the interesting default captures.
        sources.sort_by(|a, b| b.monitor.cmp(&a.monitor).then_with(|| a.name.cmp(&b.name)));
        for s in sources {
            let kind = if s.monitor { "[monitor]" } else { "[input]  " };
            println!(
                "{kind} {:40} {:>9} {:2}ch  {desc}",
                s.name,
                format!("{} Hz", s.rate),
                s.channels,
                desc = s.description,
            );
        }
        return Ok(());
    }

    if cli.dump_pcm.is_some() {
        return dump_pcm(cli);
    }

    if cli.dump_fft.is_some() {
        return dump_fft(cli);
    }

    if cli.print.is_some() {
        return print_features(cli);
    }

    unreachable!("has_cli_action guarantees an action")
}

/// argmax over a slice of magnitudes.
fn argmax(slice: &[f32]) -> usize {
    slice
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map(|(k, _)| k)
        .unwrap_or(0)
}

fn dump_pcm(cli: &Cli) -> Result<(), String> {
    let seconds = cli.dump_seconds();
    let (capture, mut cons) = Capture::start(cli.device.clone())
        .map_err(|err| format!("capture unavailable: {err}"))?;
    eprintln!(
        "capturing {seconds:.1} s from {} ({} Hz, {} ch) — writing interleaved f32 PCM to stdout…",
        capture.source_name, capture.rate, capture.channels
    );

    // Interleaved sample totals (frames × channels).
    let per_frame = CHANNELS;
    let target_samples = (seconds * capture.rate as f64).round() as usize * per_frame;
    let mut interleaved = vec![0f32; neopeek_audio::CHUNK_FRAMES * per_frame];

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut written = 0usize;
    while written < target_samples {
        let popped = cons.pop_slice(&mut interleaved);
        if popped == 0 {
            // Bounded wait; the dump stays responsive to Ctrl+C.
            std::thread::sleep(std::time::Duration::from_millis(2));
            continue;
        }
        for sample in &interleaved[..popped] {
            let bytes = sample.to_le_bytes();
            match out.write_all(&bytes) {
                Ok(()) => {}
                // `neopeek --dump-pcm | head -c …` is a valid use.
                Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => return Ok(()),
                Err(err) => return Err(format!("stdout write: {err}")),
            }
        }
        written += popped;
    }
    Ok(())
}

fn dump_fft(cli: &Cli) -> Result<(), String> {
    // One capture+FFT code path, two formatters: `--dump-fft` prints the
    // verbose per-slice debug readout, `--print` sndpeek's parity format
    // (centroid/rolloff in Hz, plain floats; documented in README).
    let fft_size = cli.fft();
    run_fft_capture(cli, cli.dump_fft_seconds(), move |capture, frame| {
        let hz = bins_to_hz(capture.rate, fft_size);
        format!(
            "seq={} rms=({:.4},{:.4}) peak=({:.4},{:.4}) centroid={:.1}Hz flux={:.3} ro50={:.1}Hz ro80={:.1}Hz peak_bin={}",
            frame.seq,
            frame.rms[0], frame.rms[1],
            frame.peak[0], frame.peak[1],
            hz(frame.centroid), frame.flux,
            hz(frame.rolloff50), hz(frame.rolloff80),
            argmax(&frame.bins[..frame.bin_count as usize]),
        )
    })
}

fn print_features(cli: &Cli) -> Result<(), String> {
    let fft_size = cli.fft();
    run_fft_capture(cli, cli.print_seconds(), move |capture, frame| {
        let hz = bins_to_hz(capture.rate, fft_size);
        format!(
            "{:.1} {:.3} {:.4} {:.1} {:.1}",
            hz(frame.centroid),
            frame.flux,
            (frame.rms[0] + frame.rms[1]) / 2.0,
            hz(frame.rolloff50),
            hz(frame.rolloff80),
        )
    })
}

/// Bins → Hz at the negotiated rate (HUD/shared conversion); the FFT
/// size is the `--fft` rung the pipeline runs at.
fn bins_to_hz(rate: u32, fft_size: usize) -> impl Fn(f32) -> f64 + Copy {
    move |bins: f32| bins as f64 * (rate as f64) / fft_size as f64
}

fn run_fft_capture(
    cli: &Cli,
    seconds: f64,
    fmt_line: impl Fn(&neopeek_audio::Capture, neopeek_dsp::Frame) -> String,
) -> Result<(), String> {
    use neopeek_dsp::Engine;

    let (capture, mut cons) = Capture::start(cli.device.clone())
        .map_err(|err| format!("capture unavailable: {err}"))?;
    eprintln!(
        "running FFT pipeline for {seconds:.1} s from {} ({} Hz, fft {}) — dumping per-slice features…",
        capture.source_name,
        capture.rate,
        cli.fft()
    );

    let window_bytes = neopeek_dsp::WINDOW_FRAMES * CHANNELS;
    // Staging buffer of pending samples; bounded at 8 windows — when it
    // overflows we drop the oldest whole windows (drop-oldest overall).
    let mut staging: Vec<f32> = Vec::with_capacity(window_bytes * 8);
    // Scratch buffer reused across pops (no per-iteration allocation).
    let mut tmp = vec![0f32; window_bytes];
    let mut engine = Engine::new(neopeek_dsp::DEFAULT_FFT_SIZE);
    let mut processed = 0usize;
    let target_slices =
        (seconds * capture.rate as f64 / neopeek_dsp::WINDOW_FRAMES as f64).ceil() as usize;

    while processed < target_slices {
        let popped = cons.pop_slice(&mut tmp);
        if popped > 0 {
            staging.extend_from_slice(&tmp[..popped]);
            let max_len = window_bytes * 8;
            if staging.len() > max_len {
                // drop whole windows from the front, keep the newest 4
                let drop_len = staging.len() - window_bytes * 4;
                let aligned = drop_len - (drop_len % 2); // keep sample alignment
                staging.drain(..aligned);
            }
        } else if staging.len() >= window_bytes {
            // no new data — keep draining what's pending
        } else {
            std::thread::sleep(std::time::Duration::from_millis(2));
            continue;
        }
        if staging.len() < window_bytes {
            continue;
        }
        let frame_slice: Vec<f32> = staging.drain(..window_bytes).collect();
        if let Some(frame) = engine.process_window(&frame_slice) {
            println!("{}", fmt_line(&capture, frame));
            processed += 1;
        }
    }
    Ok(())
}
