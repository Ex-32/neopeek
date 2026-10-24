//! Window lifecycle: winit 0.30 `ApplicationHandler` with the render shell
//! from `neopeek-render` plus input mapping (keys + mouse camera controls,
//! PLAN.md §4.3) and the audio pipeline (capture → DSP → frame ring).

use std::sync::Arc;
use std::time::Instant;

use winit::application::ApplicationHandler;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use neopeek_render::shell::Renderer;
use neopeek_render::views::ViewSettings;

use crate::dsp::AudioPipeline;
use crate::input::{handle_camera_key, handle_key};

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
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum DragButton {
    Left,
    Middle,
}

impl App {
    pub fn new(device: Option<String>, autospin: bool) -> Self {
        Self {
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
        }
    }

    fn request_redraw(&self) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
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
                self.request_redraw();
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
                if let Some(renderer) = self.renderer.as_mut() {
                    // `--autospin`: continuous slow yaw (sndpeek's charm).
                    let now = Instant::now();
                    if self.autospin {
                        let dt = self
                            .last_frame
                            .map_or(0.0, |t| now.duration_since(t).as_secs_f32());
                        renderer.camera_mut().spin(dt);
                    }
                    self.last_frame = Some(now);
                    renderer.render(&self.settings);
                }
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

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        self.request_redraw();
    }
}

fn renderer_camera(app: &mut App) -> &mut neopeek_render::camera::Camera {
    app.renderer
        .as_mut()
        .expect("keyboard event before renderer init should not happen")
        .camera_mut()
}
