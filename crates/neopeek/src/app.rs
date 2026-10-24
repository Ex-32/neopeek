//! Window lifecycle: winit 0.30 `ApplicationHandler` with the render shell
//! from `neopeek-render` plus our input mapping and the audio pipeline
//! (capture → DSP → frame ring) started alongside the window.

use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};

use winit::window::{Window, WindowId};

use neopeek_render::shell::Renderer;
use neopeek_render::views::ViewSettings;

use crate::dsp::AudioPipeline;
use crate::input::handle_key;

pub struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    settings: ViewSettings,
    audio: Option<AudioPipeline>,
    device: Option<String>,
}

impl App {
    pub fn new(device: Option<String>) -> Self {
        Self {
            window: None,
            renderer: None,
            settings: ViewSettings::default(),
            audio: None,
            device,
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
                // Frame consumer dropped → any DSP thread would drain into
                // a dead ring; bail before starting audio.
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::RedrawRequested => {
                if let Some(renderer) = self.renderer.as_mut() {
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
            WindowEvent::KeyboardInput { event, .. } => {
                // Ignore key repeats? sndpeek repeats every scale key; keep
                // repeat behavior (default) and react to presses only.
                if event.state != ElementState::Pressed {
                    return;
                }
                match event.logical_key.as_ref() {
                    Key::Named(NamedKey::Escape) => event_loop.exit(),
                    Key::Character(c) if handle_key(&mut self.settings, c) => event_loop.exit(),
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

impl App {
    fn request_redraw(&self) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}
