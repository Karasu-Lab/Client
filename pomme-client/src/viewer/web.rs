//! The viewer in a browser: a winit window over the page's canvas, a GPU
//! context the browser hands out asynchronously, and a frame per redraw.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use web_sys::HtmlCanvasElement;
use web_time::Instant;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::platform::web::{EventLoopExtWebSys, WindowAttributesExtWebSys};
use winit::window::{Window, WindowId};

use super::{GpuContext, Viewer, ViewerConfig, ViewerError, WorldSource};

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("couldn't start the event loop: {0}")]
    EventLoop(#[from] winit::error::EventLoopError),
}

/// The running viewer, once the browser has handed out a GPU.
#[derive(Clone, Default)]
pub struct ViewerHandle {
    viewer: Rc<RefCell<Option<Viewer>>>,
}

impl ViewerHandle {
    /// Whether the GPU is up and the viewer is drawing.
    pub fn running(&self) -> bool {
        self.viewer.borrow().is_some()
    }

    pub fn set_ui_enabled(&self, enabled: bool) {
        if let Some(viewer) = self.viewer.borrow_mut().as_mut() {
            viewer.set_ui_enabled(enabled);
        }
    }

    pub fn debug_lines(&self) -> Vec<String> {
        self.viewer
            .borrow()
            .as_ref()
            .map(Viewer::debug_lines)
            .unwrap_or_default()
    }
}

struct App {
    canvas: HtmlCanvasElement,
    config: Option<ViewerConfig>,
    source: Arc<dyn WorldSource>,
    handle: ViewerHandle,
    window: Option<Arc<Window>>,
    last_frame: Instant,
}

impl App {
    fn open(&mut self, window: Arc<Window>) {
        let Some(config) = self.config.take() else {
            return;
        };
        let canvas = self.canvas.clone();
        let source = Arc::clone(&self.source);
        let slot = Rc::clone(&self.handle.viewer);
        wasm_bindgen_futures::spawn_local(async move {
            let context = match GpuContext::from_canvas(canvas).await {
                Ok(context) => context,
                Err(error) => {
                    tracing::error!("No GPU: {error}");
                    return;
                }
            };
            match Viewer::new(context, Arc::clone(&window), config, source) {
                Ok(viewer) => {
                    *slot.borrow_mut() = Some(viewer);
                    window.request_redraw();
                }
                Err(error) => tracing::error!("Couldn't start the viewer: {error}"),
            }
        });
    }

    fn redraw(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32().min(0.25);
        self.last_frame = now;
        let mut slot = self.handle.viewer.borrow_mut();
        let Some(viewer) = slot.as_mut() else {
            return;
        };
        if let Err(error) = viewer.frame(dt) {
            tracing::error!("Viewer stopped: {error}");
            *slot = None;
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes().with_canvas(Some(self.canvas.clone()));
        match event_loop.create_window(attributes) {
            Ok(window) => {
                let window = Arc::new(window);
                self.window = Some(Arc::clone(&window));
                self.open(window);
            }
            Err(error) => tracing::error!("Couldn't create the window: {error}"),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(viewer) = self.handle.viewer.borrow_mut().as_mut() {
                    viewer.resize(size.width, size.height);
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && !event.repeat
                    && event.physical_key == PhysicalKey::Code(KeyCode::F3) =>
            {
                if let Some(viewer) = self.handle.viewer.borrow_mut().as_mut() {
                    viewer.toggle_debug_screen();
                }
            }
            WindowEvent::RedrawRequested => {
                self.redraw();
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }
}

/// Starts the viewer over `canvas`, drawing `source` once the browser has
/// handed out a GPU. The returned handle reaches the viewer from then on.
///
/// # Errors
/// Fails if the event loop can't start.
pub fn start(
    canvas: HtmlCanvasElement,
    config: ViewerConfig,
    source: Arc<dyn WorldSource>,
) -> Result<ViewerHandle, StartError> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let handle = ViewerHandle::default();
    event_loop.spawn_app(App {
        canvas,
        config: Some(config),
        source,
        handle: handle.clone(),
        window: None,
        last_frame: Instant::now(),
    });
    Ok(handle)
}

/// A viewer error, so the caller's `?` crosses the crate boundary.
pub type Error = ViewerError;
