//! neopeek — modern sndpeek-style audio visualizer (binary crate).
//!
//! M4 scope: live window with the waterfall/waveform/Lissajous pipeline
//! fed by capture + DSP threads. The winit/wgpu shell itself now lives in
//! `neopeek-render`; the binary keeps wiring, app state, and input mapping.
//! HUD + theming land in later milestones.

mod app;
mod cli_actions;
mod dsp;
mod input;
mod stats;

use neopeek_config::{Cli, Parser as _};
use tracing_subscriber::EnvFilter;

fn main() {
    // RUST_LOG-aware; defaults to info.
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let cli = Cli::parse();
    if cli.has_cli_action() {
        if let Err(err) = cli_actions::run(&cli) {
            tracing::error!("{err}");
            std::process::exit(1);
        }
        return;
    }

    // Opt-in memory diagnostics (`--stats-interval N`, 0 = off): the
    // user runs this on a real host to verify the RAM fixes land.
    let stats_interval = cli.stats_interval_secs();
    if stats_interval > 0 {
        stats::spawn(stats_interval);
    }

    let event_loop = match winit::event_loop::EventLoop::new() {
        Ok(el) => el,
        Err(err) => {
            eprintln!(
                "no windowing runtime libraries found -- if you built via nix, \
                 ensure the wrapper includes wayland/x11 libs\n({err})"
            );
            std::process::exit(1);
        }
    };
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);

    let mut app_state = app::App::new(
        cli.device.clone(),
        cli.autospin,
        cli.theme.clone(),
        cli.watch_theme,
        cli.depth(),
        cli.idle_pause,
    );
    if let Err(err) = event_loop.run_app(&mut app_state) {
        tracing::error!("event loop error: {err}");
        std::process::exit(1);
    }
}
