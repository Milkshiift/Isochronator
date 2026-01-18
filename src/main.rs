#![windows_subsystem = "windows"]
#![forbid(unsafe_code)]

use anyhow::{Context, Result};
use argh::FromArgs;
use bytemuck::{Pod, Zeroable};
use eframe::egui;
use env_logger::Env;
use log::info;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::str::FromStr;
use std::sync::Arc;

mod audio;
mod program;
mod visuals;

use program::{Params, Program, Settings};

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Color
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// RGBA color in sRGB color space.
#[repr(C)]
#[derive(Default, Copy, Clone, Debug, Pod, Zeroable, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const WHITE: Self = Self { r: 255, g: 255, b: 255, a: 255 };
    pub const BLACK: Self = Self { r: 0, g: 0, b: 0, a: 255 };

    /// Convert to egui color format.
    #[inline]
    pub const fn to_egui(self) -> egui::Color32 {
        egui::Color32::from_rgba_premultiplied(self.r, self.g, self.b, self.a)
    }

    /// Convert sRGB component to linear light.
    #[inline]
    fn srgb_to_linear(v: u8) -> f64 {
        let v = f64::from(v) / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }

    /// Convert linear light to sRGB component.
    #[inline]
    fn linear_to_srgb(v: f64) -> u8 {
        let v = if v <= 0.0031308 {
            v * 12.92
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        };
        (v.clamp(0.0, 1.0) * 255.0).round() as u8
    }

    /// Perceptually correct linear interpolation between two colors.
    /// Converts to linear space, interpolates, then back to sRGB.
    #[inline]
    pub fn lerp(a: Self, b: Self, t: f32) -> Self {
        let t = f64::from(t.clamp(0.0, 1.0));
        let inv = 1.0 - t;

        Self {
            r: Self::linear_to_srgb(Self::srgb_to_linear(a.r) * inv + Self::srgb_to_linear(b.r) * t),
            g: Self::linear_to_srgb(Self::srgb_to_linear(a.g) * inv + Self::srgb_to_linear(b.g) * t),
            b: Self::linear_to_srgb(Self::srgb_to_linear(a.b) * inv + Self::srgb_to_linear(b.b) * t),
            a: 255,
        }
    }

    /// Convert to linear RGB for GPU operations.
    #[inline]
    pub fn to_linear(self) -> [f64; 3] {
        [
            Self::srgb_to_linear(self.r),
            Self::srgb_to_linear(self.g),
            Self::srgb_to_linear(self.b),
        ]
    }
}

impl FromStr for Color {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.strip_prefix('#').unwrap_or(s);
        if s.len() != 6 {
            return Err("expected #RRGGBB format".into());
        }
        Ok(Self {
            r: u8::from_str_radix(&s[0..2], 16).map_err(|e| format!("red: {e}"))?,
            g: u8::from_str_radix(&s[2..4], 16).map_err(|e| format!("green: {e}"))?,
            b: u8::from_str_radix(&s[4..6], 16).map_err(|e| format!("blue: {e}"))?,
            a: 255,
        })
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// CLI
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[derive(FromArgs, Debug)]
/// Brain entrainment with isochronic/binaural audio and visual stimulation.
struct Args {
    /// program file path (.ent format)
    #[argh(positional)]
    program: Option<PathBuf>,

    /// run profiling workload for PGO optimization
    #[argh(switch)]
    profile: bool,
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// GUI
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

const DEFAULT_PROGRAM: &str = r#"
// Example entrainment program
// Starts with alpha (10 Hz), transitions to theta (6 Hz), then fades out

00:00 freq=10 tone=200 vol=0.0 duty=0.5 on=#FFFFFF off=#000000
00:05 vol=0.8 >linear
01:00 freq=6 >smooth
02:00 vol=0.0 >linear
"#;

#[derive(PartialEq, Eq, Clone, Copy)]
enum GuiMode {
    Simple,
    Program,
}

struct ControlPanel {
    mode: GuiMode,

    // Simple mode parameters
    freq: f64,
    tone: f32,
    vol: f32,
    duty: f32,
    on_color: [f32; 3],
    off_color: [f32; 3],
    binaural: bool,
    headless: bool,

    // Program mode state
    program_text: String,
    program_error: Option<String>,

    // Active session management
    active_session: Option<Child>,
}

impl Default for ControlPanel {
    fn default() -> Self {
        Self {
            mode: GuiMode::Simple,
            freq: 10.0,
            tone: 200.0,
            vol: 0.5,
            duty: 0.5,
            on_color: [1.0, 1.0, 1.0],
            off_color: [0.0, 0.0, 0.0],
            binaural: false,
            headless: false,
            program_text: DEFAULT_PROGRAM.trim().into(),
            program_error: None,
            active_session: None,
        }
    }
}

impl ControlPanel {
    /// Build a constant program from simple mode settings.
    fn build_simple_program(&self) -> Program {
        let params = Params {
            freq: self.freq,
            tone: self.tone,
            vol: self.vol,
            duty: self.duty.clamp(0.01, 0.99),
            on: Color {
                r: (self.on_color[0] * 255.0) as u8,
                g: (self.on_color[1] * 255.0) as u8,
                b: (self.on_color[2] * 255.0) as u8,
                a: 255,
            },
            off: Color {
                r: (self.off_color[0] * 255.0) as u8,
                g: (self.off_color[1] * 255.0) as u8,
                b: (self.off_color[2] * 255.0) as u8,
                a: 255,
            },
        };
        Program::constant(
            params,
            Settings {
                binaural: self.binaural,
                headless: self.headless,
            },
        )
    }

    /// Convert simple mode settings to program text.
    fn sync_to_text(&mut self) {
        self.program_text = self.build_simple_program().to_source();
        self.program_error = None;
    }

    /// Launch a new entrainment session.
    fn launch(&mut self) {
        self.stop();

        let source = match self.mode {
            GuiMode::Simple => self.build_simple_program().to_source(),
            GuiMode::Program => self.program_text.clone(),
        };

        // Validate program syntax
        if let Err(e) = Program::parse(&source) {
            self.program_error = Some(format!("Parse error: {e}"));
            return;
        }
        self.program_error = None;

        // Write to temporary file
        let mut path = std::env::temp_dir();
        path.push("isochronator_session.ent");

        if let Err(e) = std::fs::write(&path, &source) {
            self.program_error = Some(format!("Failed to write temp file: {e}"));
            return;
        }

        // Spawn session process
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("isochronator"));

        match Command::new(&exe).arg(&path).spawn() {
            Ok(child) => {
                info!("Launched session: {:?} {:?}", exe, path);
                self.active_session = Some(child);
            }
            Err(e) => {
                self.program_error = Some(format!("Failed to spawn process: {e}"));
            }
        }
    }

    /// Stop the active session if running.
    fn stop(&mut self) {
        if let Some(mut child) = self.active_session.take() {
            let _ = child.kill();
            let _ = child.wait();
            info!("Session stopped");
        }
    }

    /// Poll and clean up finished child processes.
    fn poll_session(&mut self) {
        if let Some(child) = &mut self.active_session {
            if matches!(child.try_wait(), Ok(Some(_))) {
                self.active_session = None;
            }
        }
    }
}

impl eframe::App for ControlPanel {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_session();

        egui::CentralPanel::default().show(ctx, |ui| {
            // Header
            ui.horizontal(|ui| {
                ui.heading("🧠 Isochronator");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.selectable_value(&mut self.mode, GuiMode::Program, "📝 Program");
                    ui.selectable_value(&mut self.mode, GuiMode::Simple, "🎛️ Simple");
                });
            });
            ui.separator();

            // Content based on mode
            match self.mode {
                GuiMode::Simple => self.ui_simple_mode(ui),
                GuiMode::Program => self.ui_program_mode(ui),
            }

            ui.add_space(12.0);
            ui.separator();

            // Controls
            ui.horizontal(|ui| {
                if self.active_session.is_some() {
                    if ui.button("⏹ Stop Session").clicked() {
                        self.stop();
                    }
                    ui.spinner();
                    ui.label("Session running...");
                } else if ui.button("▶ Launch Session").clicked() {
                    self.launch();
                }

                if let Some(err) = &self.program_error {
                    ui.colored_label(egui::Color32::RED, err);
                }
            });
        });
    }

    fn on_exit(&mut self) {
        self.stop();
    }
}

impl ControlPanel {
    fn ui_simple_mode(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("simple_grid")
            .num_columns(2)
            .spacing([20.0, 8.0])
            .striped(true)
            .show(ui, |ui| {
                ui.label("Frequency (Hz)");
                ui.add(egui::Slider::new(&mut self.freq, 0.5..=50.0).logarithmic(true));
                ui.end_row();

                ui.label("Carrier Tone (Hz)");
                ui.add(egui::Slider::new(&mut self.tone, 50.0..=500.0).logarithmic(true));
                ui.end_row();

                ui.label("Volume");
                ui.add(egui::Slider::new(&mut self.vol, 0.0..=1.0));
                ui.end_row();

                ui.label("Duty Cycle");
                ui.add(egui::Slider::new(&mut self.duty, 0.1..=0.9));
                ui.end_row();

                ui.label("On Color");
                ui.color_edit_button_rgb(&mut self.on_color);
                ui.end_row();

                ui.label("Off Color");
                ui.color_edit_button_rgb(&mut self.off_color);
                ui.end_row();

                ui.label("Audio Mode");
                ui.checkbox(&mut self.binaural, "Binaural beats");
                ui.end_row();

                ui.label("Display");
                ui.checkbox(&mut self.headless, "Audio only (no visuals)");
                ui.end_row();
            });

        ui.add_space(8.0);
        if ui.button("Export to Program →").clicked() {
            self.sync_to_text();
            self.mode = GuiMode::Program;
        }
    }

    fn ui_program_mode(&mut self, ui: &mut egui::Ui) {
        ui.label("Entrainment Program:");
        ui.add_space(4.0);

        egui::ScrollArea::vertical()
            .max_height(300.0)
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.program_text)
                        .code_editor()
                        .desired_width(f32::INFINITY)
                        .desired_rows(15),
                );
            });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Format: ");
            ui.code("MM:SS param=value >curve");
        });
    }
}

fn run_gui() -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([480.0, 520.0])
            .with_title("Isochronator"),
        ..Default::default()
    };

    eframe::run_native(
        "Isochronator",
        options,
        Box::new(|_cc| Ok(Box::<ControlPanel>::default())),
    )
        .map_err(|e| anyhow::anyhow!("GUI error: {e}"))
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Entry Point
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

fn main() -> Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info"))
        .filter_module("wgpu_core", log::LevelFilter::Warn)
        .filter_module("wgpu_hal", log::LevelFilter::Warn)
        .filter_module("naga", log::LevelFilter::Warn)
        .init();

    let args: Args = argh::from_env();

    // No arguments: launch GUI
    if args.program.is_none() && !args.profile {
        return run_gui();
    }

    // Profile mode: run CPU benchmark for PGO
    if args.profile {
        info!("Running profile workload...");
        let program = Program::parse(DEFAULT_PROGRAM)?;
        visuals::run_profile(Arc::new(program));
        info!("Profile complete");
        return Ok(());
    }

    // Session mode: load and run program
    let path = args.program.context("No program file specified")?;
    let program = Program::load(&path).with_context(|| format!("Loading {}", path.display()))?;

    info!(
        "Starting session: duration={:.1}s, binaural={}, headless={}",
        program.duration, program.settings.binaural, program.settings.headless
    );

    visuals::run_session(Arc::new(program))
}