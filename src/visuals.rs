use std::sync::Arc;
use std::time::{Duration, Instant};
use anyhow::Result;
use log::error;
use pixels::{Pixels, SurfaceTexture};
use test::black_box;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Fullscreen, Window, WindowId};
use crate::{audio, AppConfig, Color, TimingState, MINIMAL_WINDOW_BACKGROUND};

struct SessionApp {
    window: Option<Arc<Window>>,
    pixels: Option<Pixels<'static>>,
    timing_state: Arc<TimingState>,
    config: AppConfig,
    _stream: cpal::Stream,
    last_frame_instant: Instant,
}

impl ApplicationHandler for SessionApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let mut attributes = Window::default_attributes();
        let (width, height) = if self.config.minimal_window {
            attributes.title = "Isochronator Session (Audio Only)".to_string();
            let size = LogicalSize::new(200.0, 50.0);
            attributes.inner_size = Some(size.into());
            (size.width as u32, size.height as u32)
        } else {
            attributes.title = format!("Isochronator Session - {:.2} Hz", self.config.frequency);
            let size = LogicalSize::new(854.0, 480.0);
            attributes.inner_size = Some(size.into());
            (size.width as u32, size.height as u32)
        };

        let window = Arc::new(event_loop.create_window(attributes).unwrap());
        self.window = Some(window.clone());
        let surface_texture = SurfaceTexture::new(width, height, window.clone());
        self.pixels = Some(Pixels::new(width, height, surface_texture).expect("Failed to create Pixels context"));
        self.last_frame_instant = Instant::now();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if let WindowEvent::CloseRequested | WindowEvent::KeyboardInput { event: KeyEvent { logical_key: Key::Named(NamedKey::Escape), .. }, .. } = event {
            event_loop.exit();
            return;
        }

        if let (Some(window), Some(pixels)) = (&self.window, self.pixels.as_mut()) {
            match event {
                WindowEvent::RedrawRequested => {
                    let frame: &mut [Color] = bytemuck::cast_slice_mut(pixels.frame_mut());
                    if self.config.minimal_window {
                        frame.fill(MINIMAL_WINDOW_BACKGROUND);
                    } else {
                        let current_frame = Instant::now();
                        let start_of_frame = self.last_frame_instant.duration_since(self.timing_state.start_time);
                        let end_of_frame = current_frame.duration_since(self.timing_state.start_time);
                        self.last_frame_instant = current_frame;
                        let on_ratio = get_on_ratio(start_of_frame.as_secs_f64(), end_of_frame.as_secs_f64(), &self.timing_state) as f32;
                        let color = Color::lerp(self.config.off_color, self.config.on_color, on_ratio);
                        frame.fill(color);
                    }
                    if let Err(err) = pixels.render() { error!("pixels.render failed: {err:?}"); event_loop.exit(); }
                }
                WindowEvent::KeyboardInput { event: KeyEvent { logical_key: Key::Named(NamedKey::F11), state: ElementState::Pressed, .. }, .. } => {
                    let new_fullscreen = if window.fullscreen().is_some() { None } else { Some(Fullscreen::Borderless(window.current_monitor())) };
                    window.set_fullscreen(new_fullscreen);
                }
                WindowEvent::Resized(size) => {
                    if let Err(err) = pixels.resize_surface(size.width, size.height) { error!("pixels.resize_surface failed: {err:?}"); event_loop.exit(); }
                    if let Err(err) = pixels.resize_buffer(size.width, size.height) { error!("pixels.resize_buffer failed: {err:?}"); event_loop.exit(); }
                }
                _ => (),
            }
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

fn get_on_ratio(interval_start: f64, interval_end: f64, timing: &TimingState) -> f64 {
    let interval_duration = interval_end - interval_start;
    if interval_duration <= 0.0 { return 0.0; }
    let (period, pulse_duration) = (timing.period_secs, timing.pulse_duration_secs);

    let total_on_time_until = |t: f64| -> f64 {
        if t <= 0.0 { return 0.0; }
        let num_full_cycles = (t / period).floor();
        let time_in_current_cycle = t % period;
        num_full_cycles * pulse_duration + time_in_current_cycle.min(pulse_duration)
    };

    let on_time_in_interval = total_on_time_until(interval_end) - total_on_time_until(interval_start);
    on_time_in_interval / interval_duration
}

pub fn run_headless_profile(timing_state: &TimingState, config: &AppConfig) {
    let start = Instant::now();
    let mut current_time = 0.0;
    let frame_time_60fps = 1.0 / 60.0;
    while Instant::now().duration_since(start) < Duration::from_secs(2) {
        let on_ratio = black_box(get_on_ratio(current_time, current_time + frame_time_60fps, timing_state)) as f32;
        let _color = black_box(Color::lerp(config.off_color, config.on_color, on_ratio));
        current_time += frame_time_60fps;
    }
    let mut engine = audio::AudioEngine::new(44100.0, timing_state, config);
    for _ in 0..(44100 * 2) { black_box(engine.next_sample()); }
}

pub fn run_session(config: AppConfig) -> Result<()> {
    let timing_state = Arc::new(TimingState::new(config.frequency));
    let stream = audio::setup_audio(timing_state.clone(), &config)?;

    let mut app = SessionApp {
        window: None,
        pixels: None,
        timing_state,
        config,
        _stream: stream,
        last_frame_instant: Instant::now(),
    };

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    event_loop.run_app(&mut app)?;
    Ok(())
}