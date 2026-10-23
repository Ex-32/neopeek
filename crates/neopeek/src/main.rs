//! neopeek — modern sndpeek-style audio visualizer (binary crate).
//!
//! M1 scope: open a winit window, initialize wgpu, clear to the background
//! color. Waterfall, HUD, and capture land in later milestones (PLAN.md §8).

mod app;
mod renderer;

use tracing_subscriber::EnvFilter;

fn main() {
    // RUST_LOG-aware; defaults to info.
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let event_loop = winit::event_loop::EventLoop::new().expect("create winit event loop");
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);

    let mut app_state = app::App::default();
    if let Err(err) = event_loop.run_app(&mut app_state) {
        tracing::error!("event loop error: {err}");
        std::process::exit(1);
    }
}
