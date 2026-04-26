// =============================================================================
// Orbis — Application Entry Point & Event Handler
// =============================================================================
// Winit application handler and window management.
// =============================================================================

use std::sync::Arc;
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::WindowAttributes,
};

use crate::{GpuState, ViewMode, QUAD_HALF_WIDTH};
use crate::{settings, i18n};

struct App {
    gpu: Option<GpuState>,
}

impl App {
    fn new() -> Self {
        Self { gpu: None }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }

        // Load window icon from assets/icon/icon.png (resized to 64x64)
        let window_icon = {
            let icon_path = crate::app_path("assets/icon/icon.png");
            match image::open(&icon_path) {
                Ok(img) => {
                    let resized = img.resize_exact(64, 64, image::imageops::FilterType::Lanczos3);
                    let rgba = resized.to_rgba8();
                    let (w, h) = rgba.dimensions();
                    winit::window::Icon::from_rgba(rgba.into_raw(), w, h).ok()
                }
                Err(e) => {
                    log::warn!("Could not load window icon: {}", e);
                    None
                }
            }
        };

        let mut window_attrs = WindowAttributes::default()
            .with_title("Orbis — Real-Time Earth Viewer")
            .with_inner_size(winit::dpi::LogicalSize::new(1280, 720))
            .with_maximized(true)
            .with_visible(false);
        if let Some(icon) = window_icon {
            window_attrs = window_attrs.with_window_icon(Some(icon));
        }

        let window = Arc::new(
            event_loop
                .create_window(window_attrs)
                .expect("Failed to create window!"),
        );

        self.gpu = Some(pollster::block_on(GpuState::new(window)));

        if let Some(gpu) = &self.gpu {
            gpu.window.set_visible(true);
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        let gpu = match &mut self.gpu {
            Some(g) => g,
            None => return,
        };

        let egui_consumed = gpu.gui.handle_event(&gpu.window, &event);

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        physical_key: PhysicalKey::Code(key_code),
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                ..
            } if !egui_consumed => match key_code {
                KeyCode::Escape => event_loop.exit(),

                KeyCode::KeyL => {
                    gpu.gui_state.panel_open = !gpu.gui_state.panel_open;
                }

                KeyCode::KeyR => {
                    match gpu.view_mode {
                        ViewMode::Globe3D => gpu.camera.reset(),
                        ViewMode::Map2D => {
                            gpu.map_zoom = 1.0;
                            gpu.map_pan = (0.0, 0.0);
                        }
                    }
                    gpu.window.request_redraw();
                }

                KeyCode::KeyT => {
                    gpu.gui_state.time_live = !gpu.gui_state.time_live;
                    if gpu.gui_state.time_live {
                        let now = chrono::Utc::now();
                        gpu.gui_state.selected_year = chrono::Datelike::year(&now);
                        gpu.gui_state.selected_month = chrono::Datelike::month(&now);
                        gpu.gui_state.selected_day = chrono::Datelike::day(&now);
                        gpu.gui_state.selected_hour = chrono::Timelike::hour(&now);
                        gpu.gui_state.selected_minute = chrono::Timelike::minute(&now);
                        gpu.gui_state.date_changed = true;
                    }
                }

                KeyCode::KeyG => {
                    gpu.gui_state.labels_visible = !gpu.gui_state.labels_visible;
                }

                KeyCode::KeyK => {
                    gpu.gui_state.legend_open = !gpu.gui_state.legend_open;
                }

                KeyCode::KeyM => {
                    gpu.view_mode = match gpu.view_mode {
                        ViewMode::Globe3D => {
                            log::info!("Switching to 2D map view");
                            gpu.map_zoom = 1.0;
                            gpu.map_pan = (0.0, 0.0);
                            ViewMode::Map2D
                        }
                        ViewMode::Map2D => {
                            log::info!("Switching to 3D globe");
                            ViewMode::Globe3D
                        }
                    };
                    gpu.gui_state.view_mode_map = gpu.view_mode == ViewMode::Map2D;
                    gpu.window.request_redraw();
                }

                _ => {}
            },

            WindowEvent::Resized(physical_size) => {
                gpu.resize(physical_size.width, physical_size.height);
            }

            WindowEvent::DroppedFile(path) => {
                let ext = path.extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                if ext == "geojson" || ext == "json" || ext == "shp" {
                    log::info!("File dropped: {:?}", path);
                    gpu.gui_state.dropped_files.push(path);
                }
            }

            WindowEvent::MouseInput { state, button, .. } if !egui_consumed => {
                if button == winit::event::MouseButton::Left {
                    gpu.mouse_pressed = state == winit::event::ElementState::Pressed;
                    if !gpu.mouse_pressed {
                        gpu.last_mouse_pos = None;
                    }
                }
            }

            WindowEvent::CursorMoved { position, .. } if !egui_consumed => {
                if gpu.mouse_pressed {
                    if let Some((last_x, last_y)) = gpu.last_mouse_pos {
                        let dx = position.x - last_x;
                        let dy = position.y - last_y;

                        match gpu.view_mode {
                            ViewMode::Globe3D => {
                                gpu.camera.orbit(dx as f32, dy as f32);
                                // Break satellite follow on manual orbit
                                if gpu.gui_state.follow_satellite.is_some() {
                                    gpu.gui_state.follow_satellite = None;
                                }
                            }
                            ViewMode::Map2D => {
                                let visible_half_h = QUAD_HALF_WIDTH / (2.0 * gpu.map_zoom);
                                let visible_half_w = visible_half_h * gpu.camera.aspect;
                                let window_w = gpu.config.width as f32;
                                let window_h = gpu.config.height as f32;

                                gpu.map_pan.0 -= (dx as f32 / window_w) * visible_half_w * 2.0;
                                gpu.map_pan.1 += (dy as f32 / window_h) * visible_half_h * 2.0;
                            }
                        }
                        gpu.window.request_redraw();
                    }
                    gpu.last_mouse_pos = Some((position.x, position.y));
                }
            }

            WindowEvent::MouseWheel { delta, .. } if !egui_consumed => {
                let scroll = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y,
                    winit::event::MouseScrollDelta::PixelDelta(pos) => pos.y as f32 / 50.0,
                };

                match gpu.view_mode {
                    ViewMode::Globe3D => {
                        gpu.camera.zoom(scroll);
                    }
                    ViewMode::Map2D => {
                        let zoom_speed = 0.1;
                        gpu.map_zoom *= 1.0 + scroll * zoom_speed;
                        gpu.map_zoom = gpu.map_zoom.clamp(0.5, 20.0);
                    }
                }
                gpu.window.request_redraw();
            }

            WindowEvent::RedrawRequested => {
                match gpu.render() {
                    Ok(_) => {}
                    Err(wgpu::SurfaceError::Lost) => {
                        gpu.resize(gpu.config.width, gpu.config.height);
                    }
                    Err(wgpu::SurfaceError::OutOfMemory) => event_loop.exit(),
                    Err(e) => log::warn!("Render error: {:?}", e),
                }
                // Frame rate capped by VSync (PresentMode::Fifo)
                gpu.window.request_redraw();
            }

            _ => {}
        }
    }
}

pub(crate) fn run() {
    env_logger::init();
    // Load settings early to get language preference
    let settings = settings::Settings::load();
    i18n::init(settings.language.as_deref());
    let event_loop = EventLoop::new().expect("Failed to create event loop!");
    let mut app = App::new();
    event_loop.run_app(&mut app).expect("Event loop crashed!");
}
