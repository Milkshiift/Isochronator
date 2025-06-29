#![forbid(unsafe_code)]
#![feature(test)]
extern crate test;

use std::f32::consts::{PI, TAU};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use argh::FromArgs;
use bytemuck::{Pod, Zeroable};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use eframe::egui;
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
const MINIMAL_WINDOW_BACKGROUND: Color = Color { r: 0x22, g: 0x22, b: 0x22, a: 0xff };

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
struct Color {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

impl Color {
    fn lerp(start: Color, end: Color, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let r = start.r as f32 + (end.r as f32 - start.r as f32) * t;
        let g = start.g as f32 + (end.g as f32 - start.g as f32) * t;
        let b = start.b as f32 + (end.b as f32 - start.b as f32) * t;
        Self { r: r as u8, g: g as u8, b: b as u8, a: 0xff }
    }
}

// --- ARGH and Configuration ---

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
/// When run without arguments, a GUI control panel is launched.
struct Args {
    /// the primary frequency of the isochronic tone/binaural beat in Hz.
    #[argh(option, short = 'f', default = "20.0")]
    frequency: f64,

    /// the duration of the audio fade-in/out ramp in seconds. Low values may produce clicks.
    #[argh(option, short = 'r', default = "0.005")]
    ramp_duration: f32,

    /// the audio volume (0.0 to 1.0).
    #[argh(option, short = 'a', default = "0.5")]
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

    /// run in true headless mode (audio only, no window).
    #[argh(switch)]
    headless: bool,

    /// run headless for a few seconds to generate PGO profile data (no audio/window).
    #[argh(switch)]
    headless_profile: bool,

    #[argh(switch)]
    /// run an audio-only session with a minimal window (for GUI use).
    minimal_window: bool,
}

#[derive(Debug, Clone)]
struct AppConfig {
    frequency: f64,
    tone_hz: f32,
    on_color: Color,
    off_color: Color,
    ramp_duration: f32,
    amplitude: f32,
    binaural: bool,
    minimal_window: bool,
}

impl From<&Args> for AppConfig {
    fn from(args: &Args) -> Self {
        Self {
            frequency: args.frequency,
            tone_hz: args.tone_hz,
            on_color: args.on_color,
            off_color: args.off_color,
            ramp_duration: args.ramp_duration,
            amplitude: args.amplitude,
            binaural: args.binaural,
            minimal_window: args.minimal_window,
        }
    }
}

// --- Core Application Logic ---

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
        let mut attributes = Window::default_attributes();
        let (width, height): (u32, u32);

        if self.config.minimal_window {
            attributes.title = "Isochronator Session (Audio Only)".to_string();
            let size = LogicalSize::new(200.0, 50.0);
            attributes.inner_size = Some(size.into());

            width = size.width as u32;
            height = size.height as u32;
        } else {
            attributes.title = format!("Isochronator Session - {:.2} Hz", self.config.frequency);
            let size = LogicalSize::new(854.0, 480.0);
            attributes.inner_size = Some(size.into());

            width = size.width as u32;
            height = size.height as u32;
        }

        let window = Arc::new(event_loop.create_window(attributes).unwrap());
        self.window = Some(window.clone());

        let surface_texture =
            SurfaceTexture::new(width, height, window.clone());
        let pixels =
            Pixels::new(width, height, surface_texture).expect("Failed to create Pixels context");

        self.pixels = Some(pixels);
        self.last_frame_instant = Instant::now();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if let WindowEvent::CloseRequested
        | WindowEvent::KeyboardInput {
            event: KeyEvent { logical_key: Key::Named(NamedKey::Escape), .. },
            ..
        } = event
        {
            event_loop.exit();
            return;
        }

        if let (Some(window), Some(pixels)) = (&self.window, self.pixels.as_mut()) {
            match event {
                WindowEvent::RedrawRequested => {
                    if self.config.minimal_window {
                        let frame: &mut [Color] = bytemuck::cast_slice_mut(pixels.frame_mut());
                        frame.fill(MINIMAL_WINDOW_BACKGROUND);
                    } else {
                        let current_frame = Instant::now();
                        let start_of_frame = self.last_frame_instant.duration_since(self.timing_state.start_time);
                        let end_of_frame = current_frame.duration_since(self.timing_state.start_time);
                        self.last_frame_instant = current_frame;

                        let on_ratio = get_on_ratio(
                            start_of_frame.as_secs_f64(),
                            end_of_frame.as_secs_f64(),
                            self.timing_state.period_secs,
                            self.timing_state.pulse_duration_secs,
                        ) as f32;

                        let color = Color::lerp(self.config.off_color, self.config.on_color, on_ratio);
                        let frame: &mut [Color] = bytemuck::cast_slice_mut(pixels.frame_mut());
                        frame.fill(color);
                    }

                    if let Err(err) = pixels.render() {
                        error!("pixels.render failed: {err:?}");
                        event_loop.exit();
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

fn get_on_ratio(interval_start: f64, interval_end: f64, period: f64, pulse_duration: f64) -> f64 {
    let interval_duration = interval_end - interval_start;
    if interval_duration <= 0.0 {
        return 0.0;
    }

    let total_on_time_until = |t: f64| {
        if t <= 0.0 { return 0.0; }
        let num_full_cycles = (t / period).floor();
        let time_in_current_cycle = t % period;
        let on_time_in_current_cycle = time_in_current_cycle.min(pulse_duration);

        num_full_cycles * pulse_duration + on_time_in_current_cycle
    };

    let on_time_in_interval = total_on_time_until(interval_end) - total_on_time_until(interval_start);

    on_time_in_interval / interval_duration
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
            let left = (self.left_tone_phase * TAU).sin();
            let right = (self.right_tone_phase * TAU).sin();
            (left, right)
        } else {
            let envelope = self.get_isochronic_envelope();
            let val = (self.left_tone_phase * TAU).sin() * envelope;
            (val, val)
        };

        self.pulse_phase += self.pulse_phase_inc;
        if self.pulse_phase >= 1.0 {
            self.pulse_phase -= 1.0;
        }
        self.left_tone_phase = (self.left_tone_phase + self.left_tone_phase_inc) % 1.0;
        self.right_tone_phase = (self.right_tone_phase + self.right_tone_phase_inc) % 1.0;

        (left_val * self.amplitude, right_val * self.amplitude)
    }

    fn get_isochronic_envelope(&self) -> f32 {
        if self.pulse_phase > self.pulse_width_in_phase {
            return 0.0;
        }

        let ramp_up = self.pulse_phase / self.ramp_duration_in_phase;
        let ramp_down = (self.pulse_width_in_phase - self.pulse_phase) / self.ramp_duration_in_phase;

        let envelope = ramp_up.min(ramp_down).min(1.0);
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

    let mut engine = AudioEngine::new(sample_rate, &timing_state, app_config);

    let data_callback = move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
        for frame in data.chunks_mut(channels) {
            let (left, right) = engine.next_sample();

            if let [l, r] = frame {
                *l = left;
                *r = right;
            } else {
                for (i, sample) in frame.iter_mut().enumerate() {
                    *sample = if i % 2 == 0 { left } else { right };
                }
            }
        }
    };

    device.build_output_stream(config, data_callback, |err| error!("Audio stream error: {err}"), None)
}

fn run_headless_profile(timing_state: &TimingState, config: &AppConfig) {
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
        )) as f32;
        let _color = black_box(Color::lerp(config.off_color, config.on_color, on_ratio));
        current_time += frame_time_60fps;
    }

    let sample_rate = 44100.0;
    let mut engine = AudioEngine::new(sample_rate, timing_state, config);
    for _ in 0..(sample_rate as usize * 2) {
        black_box(engine.next_sample());
    }

    info!("Headless profiling run finished.");
}

fn run_session(config: AppConfig) -> Result<()> {
    let timing_state = Arc::new(TimingState::new(config.frequency));
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
    Ok(())
}

struct ControlPanelApp {
    frequency: f64,
    tone_hz: f32,
    ramp_duration: f32,
    amplitude: f32,
    binaural: bool,
    audio_only: bool,
    on_color: [f32; 3],
    off_color: [f32; 3],
}

impl Default for ControlPanelApp {
    fn default() -> Self {
        Self {
            frequency: 20.0,
            tone_hz: 440.0,
            ramp_duration: 0.005,
            amplitude: 0.5,
            binaural: false,
            audio_only: false,
            on_color: [1.0, 1.0, 1.0],
            off_color: [0.0, 0.0, 0.0],
        }
    }
}

impl ControlPanelApp {
    fn color_to_hex(color: [f32; 3]) -> String {
        let r = (color[0] * 255.0) as u8;
        let g = (color[1] * 255.0) as u8;
        let b = (color[2] * 255.0) as u8;
        format!("{r:02X}{g:02X}{b:02X}")
    }
}

impl eframe::App for ControlPanelApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Isochronator Control Panel");
            ui.add_space(10.0);

            egui::Grid::new("settings_grid")
                .num_columns(2)
                .spacing([40.0, 4.0])
                .striped(true)
                .show(ui, |ui| {
                    ui.label("Frequency (Hz):");
                    ui.add(egui::Slider::new(&mut self.frequency, 0.1..=50.0).logarithmic(true));
                    ui.end_row();

                    ui.label("Tone Frequency (Hz):");
                    ui.add(egui::Slider::new(&mut self.tone_hz, 20.0..=1000.0).logarithmic(true));
                    ui.end_row();

                    ui.label("Ramp Duration (s):");
                    ui.add(egui::Slider::new(&mut self.ramp_duration, 0.001..=0.02).logarithmic(true));
                    ui.end_row();

                    ui.label("Volume:");
                    ui.add(egui::Slider::new(&mut self.amplitude, 0.0..=1.0));
                    ui.end_row();

                    ui.label("On Color:");
                    ui.color_edit_button_rgb(&mut self.on_color);
                    ui.end_row();

                    ui.label("Off Color:");
                    ui.color_edit_button_rgb(&mut self.off_color);
                    ui.end_row();
                });

            ui.add_space(10.0);
            ui.checkbox(&mut self.binaural, "Binaural Mode");
            ui.checkbox(&mut self.audio_only, "Audio Only (Minimal Window)");
            ui.add_space(20.0);

            if ui.button("Launch Session").clicked() {
                let exe = std::env::current_exe().expect("Failed to get current executable");
                let mut command = Command::new(exe);

                if self.audio_only {
                    command.arg("--minimal-window");
                }

                command.arg("-f").arg(self.frequency.to_string());
                command.arg("-t").arg(self.tone_hz.to_string());
                command.arg("-r").arg(self.ramp_duration.to_string());
                command.arg("-a").arg(self.amplitude.to_string());
                command.arg("--on-color").arg(Self::color_to_hex(self.on_color));
                command.arg("--off-color").arg(Self::color_to_hex(self.off_color));

                if self.binaural {
                    command.arg("-b");
                }

                if let Err(e) = command.spawn() {
                    error!("Failed to launch session: {}", e);
                }
            }
        });
    }
}

fn run_gui() -> Result<()> {
    info!("No arguments provided, launching GUI control panel.");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, 360.0])
            .with_resizable(false)
            .with_title("Isochronator Control Panel"),
        ..Default::default()
    };

    eframe::run_native(
        "Isochronator Control Panel",
        options,
        Box::new(|_cc| Ok(Box::<ControlPanelApp>::default())),
    ).map_err(|e| anyhow::anyhow!("GUI error: {}", e))
}


fn main() -> Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();

    // If there are no arguments, run the GUI. Otherwise, run as a CLI app.
    if std::env::args().len() == 1 {
        return run_gui();
    }

    // --- CLI Mode ---
    let args: Args = argh::from_env();

    if args.headless && args.headless_profile {
        error!("Error: --headless and --headless-profile cannot be used at the same time.");
        std::process::exit(1);
    }

    let config = AppConfig::from(&args);

    if args.headless_profile {
        let timing_state = TimingState::new(config.frequency);
        run_headless_profile(&timing_state, &config);
    } else if args.headless {
        info!("Running in true headless (audio-only) mode. Press Ctrl-C to exit.");
        let timing_state = Arc::new(TimingState::new(config.frequency));
        let _stream = setup_audio(timing_state, &config)?;
        std::thread::park(); // Keep the audio stream alive
    } else {
        // This handles both full visual sessions and minimal window sessions.
        run_session(config)?;
    }

    Ok(())
}