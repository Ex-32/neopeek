//! Non-visual CLI actions for M2: `--list-devices` and `--dump-pcm`.

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
    unreachable!("has_cli_action guarantees an action")
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
