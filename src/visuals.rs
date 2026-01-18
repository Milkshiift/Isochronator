use crate::audio::{self, SyncState};
use crate::program::Program;
use anyhow::{Context, Result};
use log::{error, info, warn};
use std::hint::black_box;
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Fullscreen, Window, WindowId};

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// GPU State
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

struct GpuState {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
}

impl GpuState {
    async fn new(window: Arc<Window>) -> Result<Self> {
        let size = window.inner_size();

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });

        let surface = instance.create_surface(window)?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .context("No compatible GPU adapter found")?;

        info!("GPU adapter: {}", adapter.get_info().name);

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await?;

        let caps = surface.get_capabilities(&adapter);

        // Prefer sRGB format for correct color rendering
        let format = caps
            .formats
            .iter()
            .find(|f| f.is_srgb())
            .copied()
            .unwrap_or(caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo, // VSync for smooth visuals
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };

        surface.configure(&device, &config);

        Ok(Self {
            surface,
            device,
            queue,
            config,
        })
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    fn render(&self, color: wgpu::Color) -> Result<(), wgpu::SurfaceError> {
        let output = self.surface.get_current_texture()?;
        let view = output.texture.create_view(&Default::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());

        // Clear to the specified color
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Clear Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(color),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        output.present();

        Ok(())
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Session Application
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

struct SessionApp {
    window: Option<Arc<Window>>,
    gpu: Option<GpuState>,
    program: Arc<Program>,

    // Audio state
    audio_stream: Option<cpal::Stream>,
    sync: Arc<SyncState>,

    // Session control
    session_complete: bool,
}

impl SessionApp {
    fn new(program: Arc<Program>) -> Self {
        Self {
            window: None,
            gpu: None,
            program,
            audio_stream: None,
            sync: Arc::new(SyncState::new()),
            session_complete: false,
        }
    }

    /// Calculate the visual color based on current audio state.
    fn compute_visual_color(&self) -> wgpu::Color {
        if self.program.settings.headless {
            return wgpu::Color {
                r: 0.1,
                g: 0.1,
                b: 0.1,
                a: 1.0,
            };
        }

        // Get current playback time from audio sync state
        let time = self.sync.playback_time();
        let params = self.program.params_at(time);

        // Get phase synchronized with audio
        let phase = self.sync.visual_phase(params.freq);

        // Determine if we're in the "on" portion of the duty cycle
        let brightness = if phase < params.duty as f64 { 1.0 } else { 0.0 };

        // Interpolate between off and on colors in linear space
        let on = params.on.to_linear();
        let off = params.off.to_linear();

        wgpu::Color {
            r: off[0] + (on[0] - off[0]) * brightness,
            g: off[1] + (on[1] - off[1]) * brightness,
            b: off[2] + (on[2] - off[2]) * brightness,
            a: 1.0,
        }
    }

    /// Check if the session should end.
    fn check_session_complete(&mut self) {
        if self.session_complete {
            return;
        }

        let duration = self.program.duration;
        if !duration.is_finite() {
            return; // Infinite program never ends
        }

        let time = self.sync.playback_time();
        if time >= duration {
            info!("Session complete at {time:.1}s");
            self.session_complete = true;
        }
    }
}

impl ApplicationHandler for SessionApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Start audio if not already running
        if self.audio_stream.is_none() {
            match audio::start(self.program.clone(), self.sync.clone()) {
                Ok(stream) => {
                    self.audio_stream = Some(stream);
                    info!("Audio started");
                }
                Err(e) => {
                    error!("Failed to start audio: {e}");
                    event_loop.exit();
                    return;
                }
            }
        }

        // Create window
        let headless = self.program.settings.headless;
        let (title, size) = if headless {
            ("Isochronator (Audio Only)", LogicalSize::new(320.0, 120.0))
        } else {
            ("Isochronator", LogicalSize::new(854.0, 480.0))
        };

        let attrs = Window::default_attributes()
            .with_title(title)
            .with_inner_size(size);

        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                error!("Failed to create window: {e}");
                event_loop.exit();
                return;
            }
        };

        self.window = Some(window.clone());

        // Initialize GPU
        match pollster::block_on(GpuState::new(window)) {
            Ok(gpu) => {
                self.gpu = Some(gpu);
                info!("GPU initialized");
            }
            Err(e) => {
                error!("Failed to initialize GPU: {e}");
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                info!("Window closed");
                event_loop.exit();
            }

            WindowEvent::KeyboardInput {
                event:
                KeyEvent {
                    logical_key: Key::Named(NamedKey::Escape),
                    ..
                },
                ..
            } => {
                info!("Escape pressed");
                event_loop.exit();
            }

            WindowEvent::KeyboardInput {
                event:
                KeyEvent {
                    logical_key: Key::Named(NamedKey::F11),
                    state: ElementState::Pressed,
                    ..
                },
                ..
            } => {
                if let Some(window) = &self.window {
                    let fullscreen = if window.fullscreen().is_none() {
                        Some(Fullscreen::Borderless(window.current_monitor()))
                    } else {
                        None
                    };
                    window.set_fullscreen(fullscreen);
                }
            }

            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.gpu {
                    gpu.resize(size.width, size.height);
                }
            }

            WindowEvent::RedrawRequested => {
                // Check session completion first to handle mutable self borrow
                self.check_session_complete();
                if self.session_complete {
                    event_loop.exit();
                    return;
                }

                // Compute color before borrowing window/gpu references
                let color = self.compute_visual_color();

                let (Some(gpu), Some(window)) = (&self.gpu, &self.window) else {
                    return;
                };

                match gpu.render(color) {
                    Ok(()) => {}
                    Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                        let size = window.inner_size();
                        if let Some(gpu) = &mut self.gpu {
                            gpu.resize(size.width, size.height);
                        }
                    }
                    Err(wgpu::SurfaceError::OutOfMemory) => {
                        error!("GPU out of memory");
                        event_loop.exit();
                    }
                    Err(e) => {
                        warn!("Render error: {e:?}");
                    }
                }
            }

            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Request continuous redraws
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Entry Points
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Run a full entrainment session with audio and visuals.
pub fn run_session(program: Arc<Program>) -> Result<()> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = SessionApp::new(program);
    event_loop.run_app(&mut app)?;

    Ok(())
}

/// Run a profiling workload for PGO optimization.
pub fn run_profile(program: Arc<Program>) {
    let sync = Arc::new(SyncState::new());
    let mut engine = audio::AudioEngine::new(48000.0, program, sync);

    let mut buffer = vec![0.0f32; 1024];

    // Simulate 100 seconds of audio processing
    for _ in 0..4800 {
        engine.process(&mut buffer, 2);
        black_box(&buffer);
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Tests
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program::{Params, Settings};
    use crate::Color;

    #[test]
    fn color_to_linear_conversion() {
        let white = Color::WHITE.to_linear();
        assert!((white[0] - 1.0).abs() < 0.01);
        assert!((white[1] - 1.0).abs() < 0.01);
        assert!((white[2] - 1.0).abs() < 0.01);

        let black = Color::BLACK.to_linear();
        assert!(black[0] < 0.01);
        assert!(black[1] < 0.01);
        assert!(black[2] < 0.01);
    }

    #[test]
    fn profile_completes() {
        let program = Arc::new(Program::constant(Params::default(), Settings::default()));
        run_profile(program);
    }
}