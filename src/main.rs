#![forbid(unsafe_code)]
#![feature(test)]
extern crate test;

use std::process::Command;
use std::sync::Arc;
use std::time::Instant;
use anyhow::Result;
use argh::FromArgs;
use bytemuck::{Pod, Zeroable};
use eframe::egui;
use eframe::egui::SliderClamping;
use env_logger::Env;
use log::{error, info};

// Declare the other modules
mod audio;
mod visuals;

// --- SHARED DATA STRUCTURES ---

pub const MINIMAL_WINDOW_BACKGROUND: Color = Color { r: 0x22, g: 0x22, b: 0x22, a: 0xff };

#[repr(C)]
#[derive(Default, Copy, Clone, Debug, Pod, Zeroable)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub fn lerp(start: Color, end: Color, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let r = start.r as f32 + (end.r as f32 - start.r as f32) * t;
        let g = start.g as f32 + (end.g as f32 - start.g as f32) * t;
        let b = start.b as f32 + (end.b as f32 - start.b as f32) * t;
        Self { r: r as u8, g: g as u8, b: b as u8, a: 0xff }
    }
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub frequency: f64,
    pub tone_hz: f32,
    pub on_color: Color,
    pub off_color: Color,
    pub ramp_duration: f32,
    pub amplitude: f32,
    pub binaural: bool,
    pub minimal_window: bool,
}

#[derive(Debug, Clone)]
pub struct TimingState {
    pub start_time: Instant,
    pub frequency: f64,
    pub period_secs: f64,
    pub pulse_duration_secs: f64,
}

impl TimingState {
    pub fn new(frequency: f64) -> Self {
        let period_secs = 1.0 / frequency;
        let pulse_duration_secs = period_secs * audio::PULSE_WIDTH;
        info!(
            "Starting session: {:.2} Hz, period: {:.4}s, pulse: {:.4}s",
            frequency, period_secs, pulse_duration_secs
        );
        Self { start_time: Instant::now(), frequency, period_secs, pulse_duration_secs }
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

fn default_on_color() -> Color { parse_color("ffffff").unwrap() }
fn default_off_color() -> Color { parse_color("000000").unwrap() }

#[derive(FromArgs, Debug)]
/// A simple isochronic/binaural tone and visual stimulus generator.
/// Run without arguments for a GUI control panel.
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
            egui::Grid::new("settings_grid").num_columns(2).spacing([40.0, 4.0]).striped(true).show(ui, |ui| {
                ui.label("Frequency (Hz):"); ui.add(egui::Slider::new(&mut self.frequency, 0.1..=50.0).logarithmic(true).clamping(SliderClamping::Never)); ui.end_row();
                ui.label("Tone Frequency (Hz):"); ui.add(egui::Slider::new(&mut self.tone_hz, 20.0..=1000.0).logarithmic(true).clamping(SliderClamping::Never)); ui.end_row();
                ui.label("Audio Smoothing (s):"); ui.add(egui::Slider::new(&mut self.ramp_duration, 0.001..=0.02).logarithmic(true)); ui.end_row();
                ui.label("Volume:"); ui.add(egui::Slider::new(&mut self.amplitude, 0.0..=1.0)); ui.end_row();
                ui.label("On Color:"); ui.color_edit_button_rgb(&mut self.on_color); ui.end_row();
                ui.label("Off Color:"); ui.color_edit_button_rgb(&mut self.off_color); ui.end_row();
            });
            ui.add_space(10.0);
            ui.checkbox(&mut self.binaural, "Binaural Mode");
            ui.checkbox(&mut self.audio_only, "Audio Only (Minimal Window)");
            ui.add_space(20.0);

            if ui.button("Launch Session").clicked() {
                let exe = std::env::current_exe().expect("Failed to get current executable");
                let mut command = Command::new(exe);
                if self.audio_only { command.arg("--minimal-window"); }
                command.arg("-f").arg(self.frequency.to_string());
                command.arg("-t").arg(self.tone_hz.to_string());
                command.arg("-r").arg(self.ramp_duration.to_string());
                command.arg("-a").arg(self.amplitude.to_string());
                command.arg("--on-color").arg(Self::color_to_hex(self.on_color));
                command.arg("--off-color").arg(Self::color_to_hex(self.off_color));
                if self.binaural { command.arg("-b"); }
                if let Err(e) = command.spawn() { error!("Failed to launch session: {}", e); }
            }
        });
    }
}

fn run_gui() -> Result<()> {
    info!("No arguments provided, launching GUI control panel.");

    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
        .expect("The icon data must be valid");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, 360.0])
            .with_resizable(false)
            .with_title("Isochronator Control Panel")
            .with_icon(icon),
        ..Default::default()
    };
    eframe::run_native("Isochronator Control Panel", options, Box::new(|_cc| Ok(Box::<ControlPanelApp>::default()))).map_err(|e| anyhow::anyhow!("GUI error: {}", e))
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();

    if std::env::args().len() <= 1 {
        return run_gui();
    }

    let args: Args = argh::from_env();
    if args.headless && args.headless_profile {
        error!("Error: --headless and --headless-profile cannot be used at the same time.");
        std::process::exit(1);
    }

    let config = AppConfig::from(&args);

    if args.headless_profile {
        let timing_state = TimingState::new(config.frequency);
        visuals::run_headless_profile(&timing_state, &config);
    } else if args.headless {
        info!("Running in true headless (audio-only) mode. Press Ctrl-C to exit.");
        let timing_state = Arc::new(TimingState::new(config.frequency));
        let _stream = audio::setup_audio(timing_state, &config)?;
        std::thread::park();
    } else {
        visuals::run_session(config)?;
    }

    Ok(())
}