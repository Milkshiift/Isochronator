#![forbid(unsafe_code)]
#![feature(test)]
extern crate test;

use std::f32::consts::PI;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use argh::FromArgs;
use bytemuck::{Pod, Zeroable};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use env_logger::Env;
use log::{error, info};
use pixels::{Pixels, SurfaceTexture};
use test::black_box;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Fullscreen, Window, WindowId};

const PULSE_WIDTH: f64 = 0.5;

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
struct Color {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

impl Color {
    fn lerp(start: Color, end: Color, t: f64) -> Self {
        let t = t.clamp(0.0, 1.0) as f32;
        let r = start.r as f32 + (end.r as f32 - start.r as f32) * t;
        let g = start.g as f32 + (end.g as f32 - start.g as f32) * t;
        let b = start.b as f32 + (end.b as f32 - start.b as f32) * t;
        Self { r: r as u8, g: g as u8, b: b as u8, a: 0xff }
    }
}

fn parse_color(s: &str) -> Result<Color, String> {
    let s = s.strip_prefix('#').unwrap_or(s);
    if s.len() != 6 {
        return Err("Color must be a 6-digit hex string (e.g., FF0000)".to_string());
    }
    let r = u8::from_str_radix(&s[0..2], 16).map_err(|e| e.to_string())?;
    let g = u8::from_str_radix(&s[2..4], 16).map_err(|e| e.to_string())?;
    let b = u8::from_str_radix(&s[4..6], 16).map_err(|e| e.to_string())?;
    Ok(Color { r, g, b, a: 0xff })
}

fn default_on_color() -> Color {
    parse_color("ffffff").unwrap()
}

fn default_off_color() -> Color {
    parse_color("000000").unwrap()
}

#[derive(FromArgs, Debug)]
/// A simple isochronic/binaural tone and visual stimulus generator.
struct Args {
    /// the primary frequency of the isochronic tones in Hz. In binaural mode, this becomes the beat frequency.
    #[argh(option, short = 'f', default = "20.0")]
    frequency: f64,

    /// the duration of the audio fade-in/out ramp in seconds. Low values may produce clicks.
    #[argh(option, short = 'r', default = "0.005")]
    ramp_duration: f32,

    /// the audio volume (0.0 to 1.0).
    #[argh(option, short = 'a', default = "0.25")]
    amplitude: f32,

    /// the frequency of the audible sine wave tone in Hz.
    #[argh(option, short = 't', default = "440.0")]
    tone_hz: f32,

    /// enable binaural beat mode instead of isochronic tones
    #[argh(switch, short = 'b')]
    binaural: bool,

    /// the 'on' color of the screen flash (RRGGBB hex).
    #[argh(option, from_str_fn(parse_color), default = "default_on_color()")]
    on_color: Color,

    /// the 'off' color of the screen flash (RRGGBB hex).
    #[argh(option, from_str_fn(parse_color), default = "default_off_color()")]
    off_color: Color,

    /// run in headless mode (audio only, no visuals).
    #[argh(switch)]
    headless: bool,

    /// run in a headless mode for a few seconds to generate PGO profile data (no audio output).
    #[argh(switch)]
    headless_profile: bool,
}

#[derive(Debug, Clone)]
struct AppConfig {
    tone_hz: f32,
    on_color: Color,
    off_color: Color,
    ramp_duration: f32,
    amplitude: f32,
    binaural: bool,
}

#[derive(Debug, Clone)]
struct TimingState {
    start_time: Instant,
    frequency: f64,
    period_secs: f64,
    pulse_duration_secs: f64,
}

impl TimingState {
    fn new(frequency: f64) -> Self {
        let period_secs = 1.0 / frequency;
        let pulse_duration_secs = period_secs * PULSE_WIDTH;
        info!(
            "Starting session: {:.2} Hz, period: {:.4}s, pulse: {:.4}s",
            frequency, period_secs, pulse_duration_secs
        );
        Self { start_time: Instant::now(), frequency, period_secs, pulse_duration_secs }
    }
}

struct App {
    window: Option<Arc<Window>>,
    pixels: Option<Pixels<'static>>,
    timing_state: Arc<TimingState>,
    config: AppConfig,
    _stream: cpal::Stream,
    last_frame_instant: Instant,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let frequency = 1.0 / self.timing_state.period_secs;
        let mut attributes = Window::default_attributes();
        attributes.title = format!("Isochronator - {:.2} Hz", frequency);
        attributes.inner_size = Some(LogicalSize::new(1280.0, 720.0).into());
        attributes.min_inner_size = Some(LogicalSize::new(320.0, 240.0).into());

        let window = Arc::new(event_loop.create_window(attributes).unwrap());
        self.window = Some(window.clone());

        let window_size = window.inner_size();
        let surface_texture =
            SurfaceTexture::new(window_size.width, window_size.height, window.clone());
        let pixels = Pixels::new(window_size.width, window_size.height, surface_texture).unwrap();
        self.pixels = Some(pixels);
        self.last_frame_instant = Instant::now();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let (Some(window), Some(pixels)) = (&self.window, self.pixels.as_mut()) else { return; };

        match event {
            WindowEvent::RedrawRequested => {
                let current_frame = Instant::now();
                let start_of_frame = self.last_frame_instant.duration_since(self.timing_state.start_time);
                let end_of_frame = current_frame.duration_since(self.timing_state.start_time);
                self.last_frame_instant = current_frame;

                let on_ratio = get_on_ratio(
                    start_of_frame.as_secs_f64(),
                    end_of_frame.as_secs_f64(),
                    self.timing_state.period_secs,
                    self.timing_state.pulse_duration_secs,
                );

                let color = Color::lerp(self.config.off_color, self.config.on_color, on_ratio);
                let frame: &mut [Color] = bytemuck::cast_slice_mut(pixels.frame_mut());
                frame.fill(color);

                if let Err(err) = pixels.render() {
                    error!("pixels.render failed: {err:?}");
                    event_loop.exit();
                }
            }
            WindowEvent::CloseRequested | WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key: Key::Named(NamedKey::Escape), .. }, ..
            } => event_loop.exit(),

            WindowEvent::KeyboardInput {
                event: KeyEvent {
                    logical_key: Key::Named(NamedKey::F11),
                    state: ElementState::Pressed, ..
                }, ..
            } => {
                let new_fullscreen = if window.fullscreen().is_some() {
                    None
                } else {
                    Some(Fullscreen::Borderless(window.current_monitor()))
                };
                window.set_fullscreen(new_fullscreen);
            }
            WindowEvent::Resized(size) => {
                if let Err(err) = pixels.resize_surface(size.width, size.height) {
                    error!("pixels.resize_surface failed: {err:?}");
                    event_loop.exit();
                }
                if let Err(err) = pixels.resize_buffer(size.width, size.height) {
                    error!("pixels.resize_buffer failed: {err:?}");
                    event_loop.exit();
                }
                window.request_redraw();
            }
            _ => (),
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

/// Calculates the fraction of time a pulse was "on" during an interval for visual antialiasing.
fn get_on_ratio(interval_start: f64, interval_end: f64, period: f64, pulse_duration: f64) -> f64 {
    let interval_duration = interval_end - interval_start;
    if interval_duration <= 0.0 { return 0.0; }
    let mut total_on_time = 0.0;
    let mut current_cycle_start = (interval_start / period).floor() * period;
    while current_cycle_start < interval_end {
        let pulse_on_start = current_cycle_start;
        let pulse_on_end = current_cycle_start + pulse_duration;
        let overlap_start = interval_start.max(pulse_on_start);
        let overlap_end = interval_end.min(pulse_on_end);
        total_on_time += (overlap_end - overlap_start).max(0.0);
        current_cycle_start += period;
    }
    total_on_time / interval_duration
}

struct AudioEngine {
    amplitude: f32,
    binaural: bool,
    pulse_width_in_phase: f32,
    ramp_duration_in_phase: f32,

    pulse_phase: f32,
    pulse_phase_inc: f32,

    left_tone_phase: f32,
    right_tone_phase: f32,
    left_tone_phase_inc: f32,
    right_tone_phase_inc: f32,
}

impl AudioEngine {
    fn new(sample_rate: f32, timing: &TimingState, config: &AppConfig) -> Self {
        let beat_frequency = timing.frequency as f32;
        let period_secs = timing.period_secs as f32;

        let left_freq = config.tone_hz;
        let right_freq = if config.binaural {
            config.tone_hz + beat_frequency
        } else {
            config.tone_hz
        };

        let pulse_phase_inc = beat_frequency / sample_rate;
        let left_tone_phase_inc = left_freq / sample_rate;
        let right_tone_phase_inc = right_freq / sample_rate;

        let ramp_duration_in_phase = (config.ramp_duration / period_secs).min(0.5);
        let pulse_width_in_phase = PULSE_WIDTH as f32;

        Self {
            amplitude: config.amplitude,
            binaural: config.binaural,
            pulse_width_in_phase,
            ramp_duration_in_phase,
            pulse_phase: 0.0,
            pulse_phase_inc,
            left_tone_phase: 0.0,
            right_tone_phase: 0.0,
            left_tone_phase_inc,
            right_tone_phase_inc,
        }
    }

    fn next_sample(&mut self) -> (f32, f32) {
        let (left_val, right_val) = if self.binaural {
            // Binaural: two continuous sine waves with slightly different frequencies.
            // No envelope is needed.
            let left = (self.left_tone_phase * 2.0 * PI).sin();
            let right = (self.right_tone_phase * 2.0 * PI).sin();
            (left, right)
        } else {
            // Isochronic: one sine wave pulsed on and off.
            let envelope = self.get_isochronic_envelope();
            let val = (self.left_tone_phase * 2.0 * PI).sin() * envelope;
            (val, val)
        };

        self.pulse_phase = (self.pulse_phase + self.pulse_phase_inc) % 1.0;
        self.left_tone_phase = (self.left_tone_phase + self.left_tone_phase_inc) % 1.0;
        self.right_tone_phase = (self.right_tone_phase + self.right_tone_phase_inc) % 1.0;

        (left_val * self.amplitude, right_val * self.amplitude)
    }

    fn get_isochronic_envelope(&self) -> f32 {
        // If we are outside the pulse width, the sound is off.
        if self.pulse_phase > self.pulse_width_in_phase {
            return 0.0;
        }

        // Calculate progress through the ramp-up and ramp-down phases.
        let ramp_up = self.pulse_phase / self.ramp_duration_in_phase;
        let ramp_down = (self.pulse_width_in_phase - self.pulse_phase) / self.ramp_duration_in_phase;

        // The envelope is the minimum of the two ramps. This elegantly handles
        // the "hold" phase (where both ramps are > 1.0) and overlapping ramps
        // (where the pulse is shorter than two ramp durations).
        let envelope = ramp_up.min(ramp_down).min(1.0);

        // Apply a cosine curve for a smooth, non-linear ramp.
        0.5 * (1.0 - (PI * envelope).cos())
    }
}


fn setup_audio(timing_state: Arc<TimingState>, config: &AppConfig) -> Result<cpal::Stream> {
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or_else(|| anyhow::anyhow!("No default audio device"))?;
    info!("Audio output device: {}", device.name()?);

    let stream_config = device.default_output_config()?.into();
    let stream = build_audio_stream(&device, &stream_config, timing_state, config)?;
    stream.play()?;
    Ok(stream)
}

fn build_audio_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    timing_state: Arc<TimingState>,
    app_config: &AppConfig,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    let channels = config.channels as usize;
    let sample_rate = config.sample_rate.0 as f32;

    let engine = Arc::new(Mutex::new(AudioEngine::new(
        sample_rate,
        &timing_state,
        app_config,
    )));

    let data_callback = move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
        let mut engine = engine.lock().unwrap();
        for frame in data.chunks_mut(channels) {
            let (left, right) = engine.next_sample();

            for (i, sample) in frame.iter_mut().enumerate() {
                *sample = if i % 2 == 0 { left } else { right };
            }
        }
    };

    device.build_output_stream(config, data_callback, |err| error!("Audio stream error: {err}"), None)
}

// For PGO
fn run_headless_profile(timing_state: &TimingState, config: &AppConfig) {
    info!("Running in headless profiling mode for 2 seconds...");
    let start = Instant::now();
    let duration = Duration::from_secs(2);

    // Profile visual component
    let mut current_time = 0.0;
    let frame_time_60fps = 1.0 / 60.0;
    while Instant::now().duration_since(start) < duration {
        let on_ratio = black_box(get_on_ratio(
            current_time,
            current_time + frame_time_60fps,
            timing_state.period_secs,
            timing_state.pulse_duration_secs,
        ));
        let _color = black_box(Color::lerp(config.off_color, config.on_color, on_ratio));
        current_time += frame_time_60fps;
    }

    // Profile audio component
    let sample_rate = 44100.0;
    let mut engine = AudioEngine::new(sample_rate, timing_state, config);
    for _ in 0..(sample_rate as usize * 2) {
        black_box(engine.next_sample());
    }

    info!("Headless profiling run finished.");
}


fn main() -> Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();
    let args: Args = argh::from_env();

    // argh doesn't have declarative `conflicts_with`, so we check manually.
    if args.headless && args.headless_profile {
        eprintln!("Error: --headless and --headless-profile cannot be used at the same time.");
        std::process::exit(1);
    }

    let timing_state = Arc::new(TimingState::new(args.frequency));
    let config = AppConfig {
        tone_hz: args.tone_hz,
        on_color: args.on_color,
        off_color: args.off_color,
        ramp_duration: args.ramp_duration,
        amplitude: args.amplitude,
        binaural: args.binaural,
    };

    if args.headless_profile {
        run_headless_profile(&timing_state, &config);
    } else if args.headless {
        info!("Running in headless (audio-only) mode. Press Ctrl-C to exit.");

        let _stream = setup_audio(timing_state, &config)?;

        // Park the main thread to keep the program alive.
        std::thread::park();
    } else {
        let event_loop = EventLoop::new()?;
        event_loop.set_control_flow(ControlFlow::Poll);

        let stream = setup_audio(timing_state.clone(), &config)?;

        let mut app = App {
            window: None,
            pixels: None,
            timing_state,
            config,
            _stream: stream,
            last_frame_instant: Instant::now(),
        };

        event_loop.run_app(&mut app)?;
    }

    Ok(())
}