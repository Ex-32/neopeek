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
use neopeek_render::idle::{IdleAction, IdleGate};
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

/// Composed pacing decision for `about_to_wait`: the [`IdleGate`]'s
/// [`IdleAction`] plus the existing [`redraw_due`] tick pacing →
/// `(request a redraw now, control-flow deadline)`.
///
/// - `Redraw`: the ordinary path — request a redraw only when
///   [`redraw_due`] says so (fresh frames pending, or the tick has
///   elapsed since the last render — keeps camera/theme/freeze changes
///   live without busy-spinning), and sleep anchored to the last render
///   to avoid tick drift (re-arming from `now` when the deadline
///   already passed so a stalled present can't hot-loop).
/// - `Sleep(poll)`: idle — no redraw at all; wake at `now + poll` for
///   the next silent drain + re-evaluation.
///
/// Pure so the gate-in-the-loop pacing is unit-tested headlessly.
pub(crate) fn wait_plan(
    now: Instant,
    last_render: Option<Instant>,
    frames_pending: bool,
    tick: Duration,
    action: IdleAction,
) -> (bool, Instant) {
    match action {
        IdleAction::Redraw => {
            let deadline = match last_render {
                Some(t) if t + tick > now => t + tick,
                _ => now + tick,
            };
            (redraw_due(now, last_render, frames_pending, tick), deadline)
        }
        IdleAction::Sleep(poll) => (false, now + poll),
    }
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
    /// Idle-pause gate (`--idle-pause`): stops redraws while audio is
    /// silent/absent — state machine in `neopeek_render::idle`; fed from
    /// the drain loop (per render) and `poll_audio` (per idle poll), and
    /// from `note_input` on state-changing input events.
    idle_gate: IdleGate,
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
    pub fn new(
        device: Option<String>,
        autospin: bool,
        theme_arg: Option<String>,
        watch: bool,
        depth: usize,
        idle_pause_secs: u64,
    ) -> Self {
        let (init, warn) = neopeek_theme::resolve(theme_arg.as_deref());
        if let Some(msg) = warn {
            tracing::warn!("{msg}");
        }
        tracing::info!(theme = %init.theme.name, "theme loaded");
        // sndpeek parity: dump the whole key map once at startup.
        tracing::info!(keybinds = %crate::input::keybind_block(), "available keys");
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
            settings: {
                let mut s = ViewSettings::default();
                // Same clamp the runtime depth keys apply.
                s.set_depth_clamped(depth);
                // CLI knob (`--idle-pause`, default 5, 0 = disabled); the
                // dump reports the live value from here.
                s.idle_pause = Duration::from_secs(idle_pause_secs);
                s
            },
            audio: None,
            device,
            cursor: None,
            drag: None,
            modifiers: winit::keyboard::ModifiersState::empty(),
            autospin,
            last_frame: None,
            idle_gate: IdleGate::new(Duration::from_secs(idle_pause_secs)),
            meter: MeterState::default(),
            smooth: FeatureSmooth::default(),
            fps: 0.0,
            frame_ms: 0.0,
            cycler: Cycler::new(init),
            watch,
            watch_path,
        };
        tracing::info!(depth = app.settings.depth, "waterfall history depth");
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
        // Theme cycle changes the picture: an idle-paused display resumes.
        self.idle_gate.note_input(Instant::now());
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
        // Borrow the theme through the field path (`self.theme()` would
        // take a whole-struct `&self`, clashing with the `self.renderer`
        // mut borrow below). No per-frame clone; disjoint borrows only.
        let theme = &self.cycler.current().theme;
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
        // machines live in the binary. The closure captures only
        // disjoint `App` fields plus a shared reborrow of `renderer`;
        // the borrow ends when the `Option<HudData>` is materialized
        // (HudData owns its strings), so `render` below can take the
        // renderer mutably again. No per-frame copies: adapter name and
        // latest frame are passed by reference now (previously one
        // String clone + a ~29 KB `Frame` copy per frame).
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
                    adapter: renderer.adapter_name(),
                    rate: renderer.rate(),
                    window_frames: WINDOW_FRAMES as u32,
                    depth: renderer.history_depth(),
                    fps: self.fps,
                    frame_ms: self.frame_ms,
                    peak_hold_ms,
                    settings: &self.settings,
                    latest: renderer.latest_frame(),
                },
                &mut meter,
                &mut smooth,
            )
        });
        self.meter = meter;
        self.smooth = smooth;
        let scale = self.window.as_ref().map_or(1.0, |w| w.scale_factor()) as f32;
        let drain = renderer.render(&self.settings, hud.as_ref(), scale, theme);
        // The renderer just drained pending frames: fold their count and
        // max bin magnitude into the idle gate (silence = max == 0.0; a
        // non-silent frame resumes rendering).
        self.idle_gate.note_drain(now, drain.frames, drain.frame_max);
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

    /// Whether undrained DSP frames are waiting in the ring.
    fn frames_pending(&self) -> bool {
        self.renderer.as_ref().is_some_and(|r| r.frames_pending())
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
        match Renderer::new(window.clone(), frame_cons, self.settings.depth, self.settings.fft_size) {
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
                self.idle_gate.note_input(Instant::now());
                self.request_redraw();
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::MouseInput { state, button, .. } => {
                // Drag press/release opens an activity window (the drag
                // that follows orbits/pans; release stops it).
                self.idle_gate.note_input(Instant::now());
                match (state, button) {
                    (ElementState::Pressed, winit::event::MouseButton::Left) => {
                        self.drag = Some(DragButton::Left);
                    }
                    (ElementState::Pressed, winit::event::MouseButton::Middle) => {
                        self.drag = Some(DragButton::Middle);
                    }
                    (ElementState::Released, _) => self.drag = None,
                    _ => {}
                }
            }
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
                    self.idle_gate.note_input(Instant::now());
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
                    self.idle_gate.note_input(Instant::now());
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                match event.logical_key.as_ref() {
                    Key::Named(NamedKey::Escape) => event_loop.exit(),
                    // PageUp/PageDown: waterfall history depth (named keys
                    // carry no character, so route by name into the same
                    // `handle_key` flow character keys use).
                    Key::Named(named @ (NamedKey::PageUp | NamedKey::PageDown)) => {
                        let c = match named {
                            NamedKey::PageUp => "PageUp",
                            _ => "PageDown",
                        };
                        let (_, desc) = handle_key(&mut self.settings, c);
                        // Depth lives in the grid/texture buffers: sync the
                        // renderer right after the settings change.
                        if let Some(renderer) = self.renderer.as_mut() {
                            renderer.set_depth(self.settings.depth);
                        }
                        if let Some(d) = desc {
                            tracing::info!("{d}");
                            self.idle_gate.note_input(Instant::now());
                        }
                    }
                    Key::Character(c) => {
                        // `T`: theme cycle (App-level state, not settings).
                        if c == "T" {
                            self.cycle_theme();
                        }
                        // sndpeek parity: handlers report what they
                        // changed; we log it (INFO) — keys changing
                        // nothing (quit, unmapped) stay silent.
                        let (mut quit, mut desc) = handle_key(&mut self.settings, c);
                        let (camera_quit, camera_desc) = handle_camera_key(renderer_camera(self), c);
                        quit |= camera_quit;
                        desc = desc.or(camera_desc);
                        if let Some(d) = desc {
                            tracing::info!("{d}");
                            // A key that changed settings/camera opens an
                            // activity window (frozen/muted bypass the
                            // gate; toggles render once immediately).
                            self.idle_gate.note_input(Instant::now());
                        }
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
                // Reload changes the picture: an idle-paused display resumes.
                self.idle_gate.note_input(Instant::now());
            }
        }

        // Redraw pacing (PLAN.md §2.3/§4.2) with the idle gate
        // (`--idle-pause`, state machine in `neopeek_render::idle`): the
        // app renders at display cadence while there is activity — fresh
        // DSP frames (low latency) or the redraw tick keeping
        // camera/theme/freeze changes live — but once nothing has been
        // audible for `idle_pause` seconds it stops requesting redraws
        // entirely and wakes once per poll (1 s) to drain the ring
        // silently and re-evaluate. Input events and resumed audio
        // return the loop to the ordinary tick-paced path. On Wayland
        // the actual dispatch of a requested redraw is additionally
        // aligned with the compositor frame callback
        // (`Renderer::render` calls `window.pre_present_notify()`), so
        // the tick is a fallback for platforms without frame callbacks
        // (X11) and for missed ones.
        let now = Instant::now();
        let tick = self.redraw_tick();
        // States that must keep rendering unconditionally — the user
        // froze the scene to look at it (`f`), muted ingestion but may
        // still orbit (`m`), autospin animates the camera every frame —
        // and `--idle-pause 0` (gate disabled). Only genuine audio
        // idleness pauses.
        let keep_rendering = self.autospin
            || self.settings.frozen
            || self.settings.muted
            || self.idle_gate.threshold().is_zero();
        let frames_pending;
        let action = if keep_rendering {
            frames_pending = self.frames_pending();
            IdleAction::Redraw
        } else {
            // While idle: drain pending frames silently (no draw) so the
            // gate can tell a silently flowing monitor stream from
            // resumed audio before deciding; `poll_audio` is a no-op
            // when frozen/muted anyway.
            if self.idle_gate.is_idle(now) {
                if let Some(renderer) = self.renderer.as_mut() {
                    let drain = renderer.poll_audio(&self.settings);
                    self.idle_gate.note_drain(now, drain.frames, drain.frame_max);
                }
            }
            // Measured after the poll-drain: silence keeps the ring
            // drained so `decide` can sleep; frames_pending then only
            // reports audio that arrived in the last instants.
            frames_pending = self.frames_pending();
            // `input_dirty` is always `false` here: state-changing input
            // events call `idle_gate.note_input` directly in
            // `window_event`, whose latch `decide` consumes.
            self.idle_gate.decide(now, frames_pending, false)
        };

        // Pure pacing composition (unit-tested): request a redraw only
        // when due, sleep on the tick deadline (anchored to the last
        // render) or on the gate's poll deadline while idle.
        let (redraw, deadline) = wait_plan(now, self.last_frame, frames_pending, tick, action);
        if redraw {
            self.request_redraw();
        }
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

    // --- wait_plan: the gate-in-the-loop pacing composition (pure) ---

    /// Redraw action keeps the existing behavior exactly: request only
    /// when due (fresh frames pending → immediate; within the tick with
    /// nothing pending → quiet) and anchor the deadline to the last
    /// render.
    #[test]
    fn wait_plan_redraw_matches_old_pacing() {
        let now = Instant::now();
        let just_rendered = now.checked_sub(Duration::from_millis(5)).unwrap();

        let (redraw, deadline) =
            wait_plan(now, Some(just_rendered), true, DEFAULT_REDRAW_TICK, IdleAction::Redraw);
        assert!(redraw, "pending frames redraw immediately");
        assert_eq!(deadline, just_rendered + DEFAULT_REDRAW_TICK, "anchored, no drift");

        let (redraw, _) =
            wait_plan(now, Some(just_rendered), false, DEFAULT_REDRAW_TICK, IdleAction::Redraw);
        assert!(!redraw, "within the tick, nothing pending → quiet");
    }

    /// Sleep action: no redraw at all, wake at one poll past `now`.
    #[test]
    fn wait_plan_sleep_waits_for_poll() {
        let now = Instant::now();
        let long_ago = now.checked_sub(Duration::from_secs(30)).unwrap();
        let poll = Duration::from_millis(250);
        let (redraw, deadline) =
            wait_plan(now, Some(long_ago), false, DEFAULT_REDRAW_TICK, IdleAction::Sleep(poll));
        assert!(!redraw, "idle → never request_redraw");
        assert_eq!(deadline, now + poll);
        // wait_plan trusts the action: pending-ness is `decide`'s
        // business (it never returns Sleep with fresh frames), so a
        // hypothetical Sleep-with-pending still stays quiet here.
        let (redraw, _) =
            wait_plan(now, Some(long_ago), true, DEFAULT_REDRAW_TICK, IdleAction::Sleep(poll));
        assert!(!redraw);
    }

    /// The whole loop, composed: loud → tick-paced redraws; silence →
    /// Sleep polls after `idle_pause`; new sound → Redraw within one
    /// poll (resume-latency budget, short poll for a crisp test).
    #[test]
    fn gate_composition_loud_silent_resume() {
        let base = Instant::now();
        let poll = Duration::from_millis(50);
        let threshold = Duration::from_millis(200);
        let tick = DEFAULT_REDRAW_TICK;
        let mut gate = IdleGate::with_poll(threshold, poll);

        // Loud frame drains at t = 0: active → tick-paced redraw.
        gate.note_drain(base, 3, 0.4);
        let now = base;
        let action = gate.decide(now, false, false);
        let (redraw, _) = wait_plan(now, None, false, tick, action);
        assert!(redraw, "first wake renders");

        // Silence flows while still under the threshold: the countdown
        // keeps rendering, one redraw per tick (a render just happened a
        // tick ago, so it is due again now).
        let mut now = base + tick;
        while now < base + threshold {
            gate.note_drain(now, 2, 0.0); // silent frames keep flowing
            let action = gate.decide(now, false, false);
            assert_eq!(action, IdleAction::Redraw, "countdown keeps rendering at {now:?}");
            let (redraw, deadline) = wait_plan(now, Some(now - tick), false, tick, action);
            assert!(redraw && deadline == now + tick, "tick-paced, not hot");
            now += tick;
        }
        // (Activity is still the loud frame at `base`: silent frames in
        // the countdown don't extend it, so the gate crosses the
        // threshold right about now.)

        // Well past the last silent frame: idle — no redraws, poll sleeps.
        now += threshold + Duration::from_millis(1);
        gate.note_drain(now, 0, 0.0); // empty ring: prove-nothing drain
        let action = gate.decide(now, false, false);
        assert_eq!(action, IdleAction::Sleep(poll));
        let (redraw, _) = wait_plan(now, Some(now - tick), false, tick, action);
        assert!(!redraw);

        // A new sound lands just after a poll (worst case): the next
        // poll drains it and resumes within one poll.
        let onset = now + Duration::from_millis(10);
        let resume = onset + poll - Duration::from_millis(5);
        gate.note_drain(resume, 47, 0.3);
        let action = gate.decide(resume, false, false);
        assert_eq!(action, IdleAction::Redraw, "resumed audio redraws");
        let (redraw, _) = wait_plan(resume, Some(now), false, tick, action);
        assert!(redraw, "resume render is due (last render is poll-stale)");
        assert!(
            resume.duration_since(onset) <= poll,
            "decision within one poll of onset"
        );
    }
}
