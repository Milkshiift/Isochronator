#![forbid(unsafe_code)]
#![feature(test)]
extern crate test;


use std::f32::consts::PI;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use bytemuck::{Pod, Zeroable};
use clap::Parser;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
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

#[derive(Parser, Debug)]
#[command(author, version, about = "A simple isochronic tone and visual stimulus generator.")]
struct Args {
    /// The primary frequency of the isochronic tones in Hz.
    #[arg(short, long, default_value_t = 20.0)]
    frequency: f64,

    /// The duration of the audio fade-in/out ramp in seconds. Low values may produce clicks.
    #[arg(short, long, default_value_t = 0.005)]
    ramp_duration: f32,

    /// The audio volume (0.0 to 1.0).
    #[arg(short, long, default_value_t = 0.25)]
    amplitude: f32,

    /// The frequency of the audible sine wave tone in Hz.
    #[arg(short, long, default_value_t = 440.0)]
    tone_hz: f32,

    /// The 'on' color of the screen flash (RRGGBB hex).
    #[arg(long, value_parser = parse_color, default_value = "ffffff")]
    on_color: Color,

    /// The 'off' color of the screen flash (RRGGBB hex).
    #[arg(long, value_parser = parse_color, default_value = "000000")]
    off_color: Color,

    /// Run in a headless mode for a few seconds to generate PGO profile data.
    #[arg(long)]
    headless_profile: bool,
}

struct Config {
    tone_hz: f32,
    on_color: Color,
    off_color: Color,
    ramp_duration: f32,
    amplitude: f32,
}

#[derive(Debug, Clone)]
struct TimingState {
    start_time: Instant,
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
        Self { start_time: Instant::now(), period_secs, pulse_duration_secs }
    }
}

struct App {
    window: Option<Arc<Window>>,
    pixels: Option<Pixels<'static>>,
    timing_state: Arc<TimingState>,
    config: Config,
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

/// Calculates the audio amplitude envelope for a given sample time to prevent clicks.
fn get_audio_envelope(sample_time: f32, period: f32, pulse_duration: f32, ramp_duration: f32) -> f32 {
    let time_in_cycle = sample_time % period;
    if time_in_cycle > pulse_duration { return 0.0; }

    let ramp_down_start = pulse_duration - ramp_duration;
    if time_in_cycle < ramp_duration { // Ramp up
        0.5 * (1.0 - (PI * time_in_cycle / ramp_duration).cos())
    } else if time_in_cycle > ramp_down_start && pulse_duration > 2.0 * ramp_duration { // Ramp down
        0.5 * (1.0 + (PI * (time_in_cycle - ramp_down_start) / ramp_duration).cos())
    } else { // Hold
        1.0
    }
}

fn setup_audio(timing_state: Arc<TimingState>, config: &Config) -> Result<cpal::Stream> {
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
    app_config: &Config,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    let channels = config.channels as usize;
    let sample_rate = config.sample_rate.0 as f32;
    let start_time = timing_state.start_time;
    let period_secs = timing_state.period_secs as f32;
    let pulse_duration_secs = timing_state.pulse_duration_secs as f32;
    let tone_hz = app_config.tone_hz;
    let amplitude = app_config.amplitude;
    let ramp_duration = app_config.ramp_duration;

    let data_callback = move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
        let elapsed_secs = start_time.elapsed().as_secs_f32();
        for i in 0..data.len() / channels {
            let sample_time = elapsed_secs + (i as f32 / sample_rate);
            let envelope = get_audio_envelope(sample_time, period_secs, pulse_duration_secs, ramp_duration);
            let sine_val = (sample_time * tone_hz * 2.0 * PI).sin();
            let value = sine_val * amplitude * envelope;

            for channel in 0..channels {
                data[i * channels + channel] = value;
            }
        }
    };

    device.build_output_stream(config, data_callback, |err| error!("Audio stream error: {err}"), None)
}

fn run_headless_profile(timing_state: &TimingState, config: &Config) {
    info!("Running in headless profiling mode for 2 seconds...");
    let start = Instant::now();
    let duration = Duration::from_secs(2);

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

    let sample_rate = 44100.0;
    let sample_duration = 1.0 / sample_rate;
    let mut sample_time = 0.0f32;
    for _ in 0..(sample_rate as usize * 2) {
        let envelope = black_box(get_audio_envelope(
            sample_time,
            timing_state.period_secs as f32,
            timing_state.pulse_duration_secs as f32,
            config.ramp_duration,
        ));
        let _value = black_box((sample_time * config.tone_hz * 2.0 * PI).sin() * config.amplitude * envelope);
        sample_time += sample_duration;
    }

    info!("Headless profiling run finished.");
}


fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();

    let timing_state = Arc::new(TimingState::new(args.frequency));
    let config = Config {
        tone_hz: args.tone_hz,
        on_color: args.on_color,
        off_color: args.off_color,
        ramp_duration: args.ramp_duration,
        amplitude: args.amplitude,
    };

    if args.headless_profile {
        run_headless_profile(&timing_state, &config);
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