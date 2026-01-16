use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use log::{error, info, warn};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Fullscreen, Window, WindowId};

use crate::{AppConfig, TimingState, audio};

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
            .map_err(|e| anyhow::anyhow!("No suitable GPU adapter: {e}"))?;

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("Entrainment"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::Performance,
                ..Default::default()
            })
            .await?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps.formats[0];

        // Fifo (vsync) provides most consistent frame timing for entrainment
        // Mailbox adds latency variance, Immediate causes tearing
        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Fifo) {
            wgpu::PresentMode::Fifo
        } else {
            caps.present_modes[0]
        };

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 1,
        };
        surface.configure(&device, &config);

        info!(
            "GPU: {:?}, present: {:?}, format: {:?}",
            adapter.get_info().name,
            present_mode,
            format
        );

        Ok(Self {
            surface,
            device,
            queue,
            config,
        })
    }

    #[inline]
    fn reconfigure(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.device, &self.config);
    }

    #[inline]
    fn render(&self, color: wgpu::Color) -> Result<(), wgpu::SurfaceError> {
        let output = self.surface.get_current_texture()?;
        let view = output.texture.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());

        drop(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
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
            occlusion_query_set: None,
            timestamp_writes: None,
        }));

        self.queue.submit(Some(encoder.finish()));
        output.present();
        Ok(())
    }
}

/// High-precision pulse duty ratio calculation.
#[inline]
fn pulse_duty_ratio(t0: f64, t1: f64, period: f64, pulse_dur: f64) -> f64 {
    debug_assert!(period > 0.0);
    debug_assert!(pulse_dur >= 0.0 && pulse_dur <= period);

    let dt = t1 - t0;
    if dt < 1e-12 {
        // Zero-length interval: return instantaneous state
        let phase = t1 - (t1 / period).floor() * period;
        return if phase < pulse_dur { 1.0 } else { 0.0 };
    }

    // Cumulative on-time from t=0 to t, using stable phase computation
    #[inline(always)]
    fn cumulative(t: f64, period: f64, pulse_dur: f64) -> f64 {
        if t <= 0.0 {
            return 0.0;
        }
        let cycles = (t / period).floor();
        let phase = t - cycles * period; // More precise than t % period for large t
        cycles * pulse_dur + phase.min(pulse_dur)
    }

    let on_time = cumulative(t1, period, pulse_dur) - cumulative(t0, period, pulse_dur);
    (on_time / dt).clamp(0.0, 1.0)
}

/// Pre-computed linear colors to avoid per-frame sRGB conversion
struct LinearColors {
    off: [f32; 4],
    on: [f32; 4],
}

impl LinearColors {
    fn new(config: &AppConfig) -> Self {
        Self {
            off: config.off_color.to_linear_f32(),
            on: config.on_color.to_linear_f32(),
        }
    }

    #[inline]
    fn lerp(&self, t: f32) -> wgpu::Color {
        let inv = 1.0 - t;
        wgpu::Color {
            r: (self.off[0] * inv + self.on[0] * t) as f64,
            g: (self.off[1] * inv + self.on[1] * t) as f64,
            b: (self.off[2] * inv + self.on[2] * t) as f64,
            a: 1.0,
        }
    }
}

struct SessionApp {
    window: Option<Arc<Window>>,
    gpu: Option<GpuState>,
    timing: Arc<TimingState>,
    config: AppConfig,
    audio_stream: Option<cpal::Stream>,
    linear_colors: Option<LinearColors>,
    prev_frame_time: f64,
}

impl SessionApp {
    fn reconfigure_surface(&mut self) {
        if let (Some(gpu), Some(window)) = (&mut self.gpu, &self.window) {
            let size = window.inner_size();
            gpu.reconfigure(size.width, size.height);
        }
    }
}

impl ApplicationHandler for SessionApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Initialize audio first - it's the timing reference
        if self.audio_stream.is_none() {
            match audio::setup_audio(self.timing.clone(), &self.config) {
                Ok(s) => self.audio_stream = Some(s),
                Err(e) => {
                    error!("Audio init failed: {e}");
                    event_loop.exit();
                    return;
                }
            }
        }

        let (title, size): (&str, LogicalSize<f64>) = if self.config.minimal_window {
            ("Entrainment (Audio Only)", LogicalSize::new(200.0, 50.0))
        } else {
            (
                &format!("Entrainment - {:.2} Hz", self.config.frequency),
                LogicalSize::new(854.0, 480.0),
            )
        };

        let attrs = Window::default_attributes()
            .with_title(title)
            .with_inner_size(size);

        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                error!("Window creation failed: {e}");
                event_loop.exit();
                return;
            }
        };
        self.window = Some(window.clone());

        match pollster::block_on(GpuState::new(window)) {
            Ok(s) => self.gpu = Some(s),
            Err(e) => {
                error!("GPU init failed: {e}");
                event_loop.exit();
                return;
            }
        }

        // Pre-compute linear colors once
        self.linear_colors = Some(LinearColors::new(&self.config));
        self.prev_frame_time = 0.0;
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested
            | WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        ..
                    },
                ..
            } => event_loop.exit(),

            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.gpu {
                    gpu.reconfigure(size.width, size.height);
                }
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
                if let Some(w) = &self.window {
                    let fs = w
                        .fullscreen()
                        .is_none()
                        .then(|| Fullscreen::Borderless(w.current_monitor()));
                    w.set_fullscreen(fs);
                    // Immediately reconfigure for new dimensions
                    self.reconfigure_surface();
                }
            }

            WindowEvent::RedrawRequested => {
                let (Some(gpu), Some(window)) = (&mut self.gpu, &self.window) else {
                    return;
                };

                // Sample time exactly once per frame for consistency
                let now = Instant::now()
                    .saturating_duration_since(self.timing.start_time)
                    .as_secs_f64();

                let color = if self.config.minimal_window {
                    wgpu::Color {
                        r: 0.02,
                        g: 0.02,
                        b: 0.02,
                        a: 1.0,
                    }
                } else if let Some(colors) = &self.linear_colors {
                    let ratio = pulse_duty_ratio(
                        self.prev_frame_time,
                        now,
                        self.timing.period_secs,
                        self.timing.pulse_duration_secs,
                    ) as f32;
                    colors.lerp(ratio)
                } else {
                    wgpu::Color::BLACK
                };

                self.prev_frame_time = now;

                loop {
                    match gpu.render(color) {
                        Ok(()) => break,
                        Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                            let size = window.inner_size();
                            gpu.reconfigure(size.width, size.height);
                        }
                        Err(wgpu::SurfaceError::OutOfMemory) => {
                            error!("Out of GPU memory");
                            event_loop.exit();
                            return;
                        }
                        Err(wgpu::SurfaceError::Timeout) => {
                            warn!("Surface timeout, skipping frame");
                            break;
                        }
                        Err(e) => {
                            error!("Render error: {e:?}");
                            break;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _: &ActiveEventLoop) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
}

pub fn run_headless_profile(timing: &TimingState, config: &AppConfig) {
    const BUFFER_SIZE: usize = 512;
    const SAMPLE_RATE: f32 = 44100.0;
    const CHANNELS: usize = 2;

    const TOTAL_SECONDS: f64 = 3.0;
    const TOTAL_SAMPLES: usize = (SAMPLE_RATE as f64 * TOTAL_SECONDS) as usize;
    const NUM_CHUNKS: usize = TOTAL_SAMPLES / BUFFER_SIZE;

    let chunks_per_video_frame = (SAMPLE_RATE / BUFFER_SIZE as f32) / 60.0;

    let mut engine =
        audio::AudioEngine::new(SAMPLE_RATE, timing.frequency, timing.period_secs, config);

    let colors = LinearColors::new(config);

    let mut buffer = vec![0.0f32; BUFFER_SIZE * CHANNELS];

    let mut last_frame_time = 0.0;
    let mut current_time = 0.0;
    let time_per_chunk = BUFFER_SIZE as f64 / SAMPLE_RATE as f64;

    info!(
        "Running PGO profile: {} chunks (~{}s)...",
        NUM_CHUNKS, TOTAL_SECONDS
    );

    for i in 0..NUM_CHUNKS {
        // Profile Audio (Hot Path)
        engine.process_buffer(&mut buffer, CHANNELS);
        black_box(());

        current_time += time_per_chunk;

        // Profile Video (Hot Path)
        if (i as f32 % chunks_per_video_frame) < 1.0 {
            let r = black_box(pulse_duty_ratio(
                last_frame_time,
                current_time,
                timing.period_secs,
                timing.pulse_duration_secs,
            )) as f32;

            black_box(colors.lerp(r));
            last_frame_time = current_time;
        }
    }

    info!("Profile complete");
}

pub fn run_session(config: AppConfig, timing: Arc<TimingState>) -> Result<()> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    event_loop.run_app(&mut SessionApp {
        window: None,
        gpu: None,
        timing,
        config,
        audio_stream: None,
        linear_colors: None,
        prev_frame_time: 0.0,
    })?;
    Ok(())
}
