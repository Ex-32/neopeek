//! neopeek — modern sndpeek-style audio visualizer (binary crate).
//!
//! M2 scope: CLI actions (list devices / dump PCM) + the M1 window shell.
//! Waterfall, HUD, and sustained capture land in later milestones.

mod app;
mod cli_actions;
mod renderer;

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

    let event_loop = winit::event_loop::EventLoop::new().expect("create winit event loop");
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);

    let mut app_state = app::App::default();
    if let Err(err) = event_loop.run_app(&mut app_state) {
        tracing::error!("event loop error: {err}");
        std::process::exit(1);
    }
}
