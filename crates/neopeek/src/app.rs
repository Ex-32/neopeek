//! Window lifecycle: winit 0.30 `ApplicationHandler` with the render shell
//! from `neopeek-render` plus input mapping (keys + mouse camera controls,
//! PLAN.md §4.3) and the audio pipeline (capture → DSP → frame ring).

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use neopeek_render::hud::{FeatureSmooth, HudData, HudSources, MeterState};
use neopeek_render::shell::Renderer;
use neopeek_render::views::ViewSettings;

use crate::dsp::AudioPipeline;
use crate::input::{handle_camera_key, handle_key};

use neopeek_dsp::WINDOW_FRAMES;
use neopeek_theme::{cycler::Cycler, Theme};

/// Fallback redraw tick: one redraw per ~display frame at 60 Hz. Used when
/// the monitor's refresh rate is unknown. Pure pacing input, see
/// [`redraw_due`].
const DEFAULT_REDRAW_TICK: Duration = Duration::from_nanos(16_666_667);

/// Redraw pacing decision for `about_to_wait` (PLAN.md §2.3: "No new frame →
/// reuse last history (cheap), still render"): request a redraw immediately
/// when fresh DSP frames are pending (low latency), and otherwise once per
/// redraw tick since the last render — camera/theme/freeze changes stay live
/// even with zero frames. Pure so the pacing can be unit-tested headlessly.
pub(crate) fn redraw_due(
    now: Instant,
    last_render: Option<Instant>,
    frames_pending: bool,
    tick: Duration,
) -> bool {
    if frames_pending {
        return true;
    }
    last_render.is_none_or(|t| now.duration_since(t) >= tick)
}

pub struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    settings: ViewSettings,
    audio: Option<AudioPipeline>,
    device: Option<String>,

    // camera interaction state (M5)
    cursor: Option<PhysicalPosition<f64>>,
    drag: Option<DragButton>,
    modifiers: winit::keyboard::ModifiersState,
    autospin: bool,
    last_frame: Option<Instant>,
    // HUD state (M6)
    meter: MeterState,
    smooth: FeatureSmooth,
    /// Smoothed frames-per-second (0 until the first two frames).
    fps: f32,
    /// Smoothed frame time, ms.
    frame_ms: f32,
    // theming (M7)
    cycler: Cycler,
    /// `--watch-theme`: reload feed for the loaded custom file.
    watch: Option<neopeek_theme::watch::ReloadWatcher>,
    watch_path: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum DragButton {
    Left,
    Middle,
}

impl App {
    pub fn new(device: Option<String>, autospin: bool, theme_arg: Option<String>, watch: bool) -> Self {
        let (init, warn) = neopeek_theme::resolve(theme_arg.as_deref());
        if let Some(msg) = warn {
            tracing::warn!("{msg}");
        }
        tracing::info!(theme = %init.theme.name, "theme loaded");
        // `--watch-theme` needs a *file* theme; built-ins watch nothing.
        let (watch, watch_path) = if watch {
            match init.path.clone() {
                Some(path) => {
                    match neopeek_theme::watch::ReloadWatcher::spawn(path.clone()) {
                        Some(w) => (Some(w), Some(path)),
                        None => {
                            tracing::warn!("could not start theme watcher");
                            (None, None)
                        }
                    }
                }
                None => {
                    tracing::info!("--watch-theme: no theme file loaded, nothing to watch");
                    (None, None)
                }
            }
        } else {
            (None, None)
        };
        let mut app = Self {
            window: None,
            renderer: None,
            settings: ViewSettings::default(),
            audio: None,
            device,
            cursor: None,
            drag: None,
            modifiers: winit::keyboard::ModifiersState::empty(),
            autospin,
            last_frame: None,
            meter: MeterState::default(),
            smooth: FeatureSmooth::default(),
            fps: 0.0,
            frame_ms: 0.0,
            cycler: Cycler::new(init),
            watch,
            watch_path,
        };
        app.apply_theme_knobs();
        app
    }

    fn theme(&self) -> &Theme {
        &self.cycler.current().theme
    }

    /// `T`: next theme; re-seeds the theme-owned view knobs.
    pub fn cycle_theme(&mut self) {
        self.cycler.cycle();
        self.apply_theme_knobs();
        let t = self.theme().clone();
        tracing::info!(theme = %t.name, "theme cycled");
        if let Some(r) = self.renderer.as_mut() {
            r.set_theme_ramp(&t);
        }
    }

    /// Rainbow default + Lissajous scale are theme-owned; re-applied on
    /// every theme application (startup and `T`).
    fn apply_theme_knobs(&mut self) {
        let (rainbow, liss) = {
            let t = self.theme();
            (t.waterfall.rainbow, t.lissajous.scale)
        };
        self.settings.rainbow = rainbow;
        self.settings.liss_scale = liss;
    }

    /// Renders one frame: drains pending DSP frames into the history ring,
    /// rebuilds the HUD state, draws the scene + HUD passes, and presents.
    /// Called from `RedrawRequested` and once synchronously from `resumed`.
    fn draw_frame(&mut self) {
        // Copy the active theme out first (small; ~6 short strings, buys
        // clean disjoint borrows below).
        let theme = self.theme().clone();
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        // `--autospin`: continuous slow yaw (sndpeek's charm).
        let now = Instant::now();
        if self.autospin {
            let dt = self
                .last_frame
                .map_or(0.0, |t| now.duration_since(t).as_secs_f32());
            renderer.camera_mut().spin(dt);
        }
        // Frame-time stats for the HUD (EMA over redraws). Tracked at
        // redraw time — not present time — so a failed surface texture
        // still paces retry redraws through the redraw tick.
        if let Some(t) = self.last_frame.replace(now) {
            let ms = now.duration_since(t).as_secs_f32() * 1000.0;
            self.frame_ms = if self.frame_ms == 0.0 {
                ms
            } else {
                self.frame_ms * 0.9 + ms * 0.1
            };
            self.fps = 1000.0 / self.frame_ms.max(0.01);
        }
        // Assemble the HUD before render: the meter/peak-hold state
        // machines live in the binary, the drained frames inside the
        // renderer (copied out above). The closure captures only disjoint
        // `App` fields.
        let (adapter, rate, depth, latest) = (
            renderer.adapter_name().to_owned(),
            renderer.rate(),
            renderer.history_depth(),
            renderer.latest_frame().copied(),
        );
        let mut meter = self.meter;
        let mut smooth = self.smooth;
        let peak_hold_ms = theme.meters.peak_hold_ms;
        let hud = self.settings.hud_on.then(|| {
            let (source_name, backend) = match self.audio.as_ref() {
                Some(audio) if audio.capture_lost() => ("(capture lost)", "pulse"),
                Some(audio) => (audio.source_name(), "pulse"),
                None => ("(no capture)", "none"),
            };
            HudData::assemble(
                HudSources {
                    source_name,
                    backend,
                    adapter: &adapter,
                    rate,
                    window_frames: WINDOW_FRAMES as u32,
                    depth,
                    fps: self.fps,
                    frame_ms: self.frame_ms,
                    peak_hold_ms,
                    settings: &self.settings,
                    latest: latest.as_ref(),
                },
                &mut meter,
                &mut smooth,
            )
        });
        self.meter = meter;
        self.smooth = smooth;
        let scale = self.window.as_ref().map_or(1.0, |w| w.scale_factor()) as f32;
        renderer.render(&self.settings, hud.as_ref(), scale, &theme);
    }

    fn request_redraw(&self) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// Redraw tick: one redraw per display frame. Uses the current
    /// monitor's refresh rate when reported, falling back to 60 Hz.
    fn redraw_tick(&self) -> Duration {
        let mhz = self
            .window
            .as_ref()
            .and_then(|w| w.current_monitor())
            .and_then(|m| m.refresh_rate_millihertz());
        match mhz {
            Some(mhz) if mhz > 0 => Duration::from_nanos(1_000_000_000_000 / u64::from(mhz)),
            _ => DEFAULT_REDRAW_TICK,
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("neopeek")
            .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0));
        let window = match event_loop.create_window(attrs) {
            Ok(window) => Arc::new(window),
            Err(err) => {
                tracing::error!("failed to create window: {err}");
                event_loop.exit();
                return;
            }
        };
        // The ring lives frame-long; its producer half goes to the DSP
        // thread, the consumer to the renderer.
        let (frame_prod, frame_cons) = Renderer::frame_pair();
        match Renderer::new(window.clone(), frame_cons) {
            Ok(mut renderer) => {
                renderer.set_theme_ramp(self.theme());
                match AudioPipeline::start(self.device.take(), frame_prod) {
                    Ok(audio) => {
                        renderer.set_rate(audio.rate());
                        self.audio = Some(audio);
                    }
                    Err(err) => {
                        // Keep the window open (HUD will report this in M6);
                        // the waterfall renders flat without capture.
                        tracing::error!("capture unavailable: {err}");
                    }
                }
                self.window = Some(window);
                self.renderer = Some(renderer);
                // Present once, synchronously: the first buffer commit is
                // what maps the window on Wayland. Relying on
                // `RedrawRequested` here would be a trap — winit dispatches
                // it only once a compositor frame callback is available,
                // and that callback is scheduled by the present itself
                // (`pre_present_notify`) — a never-mapped surface would
                // wait for a callback that never fires. Observed on the
                // host as "wgpu surface initialized" but no window.
                self.draw_frame();
            }
            Err(err) => {
                tracing::error!("failed to initialize renderer: {err:#}");
                // Frame consumer dropped → a DSP thread would push into a
                // dead ring; bail before starting audio. (None started:
                // renderer init failed first.)
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::RedrawRequested => {
                // Render exactly once per event; the pacing decision that
                // schedules the next redraw lives in `about_to_wait`.
                self.draw_frame();
            }
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.resize(size.width.max(1), size.height.max(1));
                }
                self.request_redraw();
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::MouseInput { state, button, .. } => match (state, button) {
                (ElementState::Pressed, winit::event::MouseButton::Left) => {
                    self.drag = Some(DragButton::Left);
                }
                (ElementState::Pressed, winit::event::MouseButton::Middle) => {
                    self.drag = Some(DragButton::Middle);
                }
                (ElementState::Released, _) => self.drag = None,
                _ => {}
            },
            WindowEvent::CursorMoved { position, .. } => {
                let Some(renderer) = self.renderer.as_mut() else {
                    return;
                };
                let last = self.cursor.replace(position);
                if let (Some(prev), Some(drag_button)) = (last, self.drag) {
                    let (dx, dy) = (position.x - prev.x, position.y - prev.y);
                    // DPI-aware: radians/px scales down on HiDPI displays.
                    let dpi = self.window.as_ref().map_or(1.0, |w| w.scale_factor()) as f32;
                    // Pixel→radian default: a full ~200 px stroke ≈ 1 rad.
                    let rad_per_px = 0.005 / dpi.max(0.5);
                    if drag_button == DragButton::Left && !self.modifiers.shift_key() {
                        renderer.camera_mut().orbit(dx as f32 * rad_per_px, -dy as f32 * rad_per_px);
                    } else {
                        // shift-drag or middle-drag: pan in the camera plane,
                        // scaled by distance (see Camera::pan).
                        let pan_per_px = 0.002 / dpi.max(0.5);
                        renderer.camera_mut().pan(dx as f32 * pan_per_px, -dy as f32 * pan_per_px);
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if let Some(renderer) = self.renderer.as_mut() {
                    let factor = match delta {
                        MouseScrollDelta::LineDelta(_, y) => {
                            if y > 0.0 { 1.1 } else { 1.0 / 1.1 }
                        }
                        MouseScrollDelta::PixelDelta(pos) => {
                            // ~120 px per wheel notch on most platforms.
                            (1.1f32).powf((pos.y as f32 / 120.0).clamp(-3.0, 3.0))
                        }
                    };
                    renderer.camera_mut().dolly(factor);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                match event.logical_key.as_ref() {
                    Key::Named(NamedKey::Escape) => event_loop.exit(),
                    Key::Character(c) => {
                        // `T`: theme cycle (App-level state, not settings).
                        if c == "T" {
                            self.cycle_theme();
                        }
                        let quit =
                            handle_key(&mut self.settings, c)
                                || handle_camera_key(renderer_camera(self), c);
                        if quit {
                            event_loop.exit();
                        } else if c == "p" {
                            // sndpeek parity: dump all settings to console.
                            let renderer = self
                                .renderer
                                .as_ref()
                                .expect("renderer exists before key handling");
                            eprintln!(
                                "{}",
                                neopeek_render::dump::settings_string(
                                    &self.settings,
                                    renderer.camera(),
                                )
                            );
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // `--watch-theme`: swap in the latest re-parsed theme file.
        if let (Some(path), Some(w)) = (self.watch_path.as_deref(), &self.watch) {
            if let Some(theme) = w.try_latest() {
                tracing::info!(theme = %theme.name, "theme reloaded");
                self.cycler.reload(path, theme);
                let t = self.theme().clone();
                if let Some(r) = self.renderer.as_mut() {
                    r.set_theme_ramp(&t);
                }
            }
        }

        // Redraw pacing (PLAN.md §2.3/§4.2): the app renders continuously
        // at display cadence — with zero pending frames we still redraw so
        // camera/theme/freeze changes stay live — but never busy-spins:
        // a redraw is requested only when fresh DSP frames are pending
        // (low latency) or when the redraw tick has come due since the
        // last render. Between wakes the loop sleeps on the tick deadline
        // (`WaitUntil`) or on events. On Wayland the actual dispatch of
        // the requested redraw is additionally aligned with the
        // compositor frame callback (`Renderer::render` calls
        // `window.pre_present_notify()`), so the tick is a fallback for
        // platforms without frame callbacks (X11) and for missed ones.
        let now = Instant::now();
        let frames_pending = self
            .renderer
            .as_ref()
            .is_some_and(|r| r.frames_pending());
        let tick = self.redraw_tick();
        if redraw_due(now, self.last_frame, frames_pending, tick) {
            self.request_redraw();
        }
        // Sleep until the next tick; if a redraw is already scheduled (or
        // a frame callback is in flight) the loop wakes earlier anyway.
        // Anchoring to the last render instead of `now` avoids drift; if
        // the deadline already passed we re-arm from `now` so a stalled
        // present can't turn this into a hot no-op loop.
        let deadline = match self.last_frame {
            Some(t) if t + tick > now => t + tick,
            _ => now + tick,
        };
        event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
    }
}

fn renderer_camera(app: &mut App) -> &mut neopeek_render::camera::Camera {
    app.renderer
        .as_mut()
        .expect("keyboard event before renderer init should not happen")
        .camera_mut()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fresh DSP frames always redraw immediately, even right after a
    /// render (drain-then-render keeps the waterfall at slice latency).
    #[test]
    fn pending_frames_redraw_immediately() {
        let now = Instant::now();
        assert!(redraw_due(now, Some(now), true, DEFAULT_REDRAW_TICK));
    }

    /// Before the first render (or after a `resumed` that skipped it) we
    /// always redraw — the window must map on the first wake.
    #[test]
    fn no_last_render_redraws() {
        let now = Instant::now();
        assert!(redraw_due(now, None, false, DEFAULT_REDRAW_TICK));
    }

    /// Within the tick and with no pending frames: no redraw, the loop
    /// sleeps on the deadline instead (no busy spin).
    #[test]
    fn within_tick_is_quiet() {
        let now = Instant::now();
        let just_rendered = now.checked_sub(Duration::from_millis(5)).unwrap();
        assert!(!redraw_due(now, Some(just_rendered), false, DEFAULT_REDRAW_TICK));
    }

    /// At/after the tick: redraw due — keeps theme/camera changes live
    /// with zero frames (PLAN.md §2.3).
    #[test]
    fn tick_elapsed_redraws() {
        let now = Instant::now();
        let tick_ago = now.checked_sub(DEFAULT_REDRAW_TICK).unwrap();
        assert!(redraw_due(now, Some(tick_ago), false, DEFAULT_REDRAW_TICK));
        let over_tick = now.checked_sub(DEFAULT_REDRAW_TICK * 2).unwrap();
        assert!(redraw_due(now, Some(over_tick), false, DEFAULT_REDRAW_TICK));
    }

    /// A finer tick (high-refresh monitor) redraws sooner than the 60 Hz
    /// fallback would.
    #[test]
    fn tick_length_is_respected() {
        let now = Instant::now();
        let a_while_ago = now.checked_sub(Duration::from_millis(10)).unwrap();
        assert!(!redraw_due(now, Some(a_while_ago), false, DEFAULT_REDRAW_TICK));
        assert!(redraw_due(now, Some(a_while_ago), false, Duration::from_millis(9)));
    }
}
