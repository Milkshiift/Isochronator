//! Entrainment program format parsing and runtime interpolation.
//!
//! # File Format
//!
//! ```text
//! // Comments start with // or #
//!
//! // First keyframe must be at 00:00 and defines all initial values
//! 00:00 freq=10 tone=200 vol=0 duty=0.5 on=#FFFFFF off=#000000
//!
//! // Subsequent keyframes specify changes with optional transition curves
//! 00:10 vol=0.8 >linear          // Fade in over 10 seconds
//! 02:00 freq=6 >smooth           // Smooth ease to 6 Hz
//! 05:00 vol=0 >linear            // Fade out
//!
//! // Settings (only on first line): binaural, headless
//! ```

use crate::Color;
use anyhow::{bail, Context, Result};
use std::fmt::Write;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Curve
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Interpolation curve for transitions between keyframes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Curve {
    /// Instant change at the keyframe time (no interpolation).
    #[default]
    Step,
    /// Linear interpolation.
    Linear,
    /// Smooth ease-in-out (Hermite smoothstep).
    Smooth,
}

impl Curve {
    /// Apply the curve function to a normalized time value [0, 1].
    #[inline]
    pub fn apply(self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::Step => if t >= 1.0 { 1.0 } else { 0.0 },
            Self::Linear => t,
            Self::Smooth => t * t * (3.0 - 2.0 * t), // Hermite smoothstep
        }
    }

    fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "step" => Ok(Self::Step),
            "linear" => Ok(Self::Linear),
            "smooth" => Ok(Self::Smooth),
            _ => bail!("unknown curve '{s}' (expected: step, linear, smooth)"),
        }
    }

    fn to_str(self) -> Option<&'static str> {
        match self {
            Self::Step => None,
            Self::Linear => Some("linear"),
            Self::Smooth => Some("smooth"),
        }
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Params
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Parameters at a point in time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Params {
    /// Entrainment frequency in Hz (pulse/beat rate).
    pub freq: f64,
    /// Carrier tone frequency in Hz.
    pub tone: f32,
    /// Output volume [0, 1].
    pub vol: f32,
    /// Duty cycle for isochronic tones [0.01, 0.99].
    pub duty: f32,
    /// Visual color when pulse is on.
    pub on: Color,
    /// Visual color when pulse is off.
    pub off: Color,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            freq: 10.0,
            tone: 200.0,
            vol: 0.5,
            duty: 0.5,
            on: Color::WHITE,
            off: Color::BLACK,
        }
    }
}

impl Params {
    /// Linearly interpolate between two parameter sets.
    #[inline]
    pub fn lerp(a: &Self, b: &Self, t: f64) -> Self {
        let t32 = t as f32;
        let inv64 = 1.0 - t;
        let inv32 = 1.0 - t32;

        Self {
            freq: a.freq * inv64 + b.freq * t,
            tone: a.tone * inv32 + b.tone * t32,
            vol: a.vol * inv32 + b.vol * t32,
            duty: a.duty * inv32 + b.duty * t32,
            on: Color::lerp(a.on, b.on, t32),
            off: Color::lerp(a.off, b.off, t32),
        }
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Settings
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Session-level settings (set only at program start).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Settings {
    /// Use binaural beats instead of isochronic tones.
    pub binaural: bool,
    /// Disable visual output (audio only).
    pub headless: bool,
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Program
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// A single keyframe in the program timeline.
#[derive(Debug, Clone)]
struct Keyframe {
    time: f64,
    params: Params,
    curve: Curve,
}

/// An entrainment program with keyframes and settings.
#[derive(Debug)]
pub struct Program {
    keyframes: Vec<Keyframe>,
    pub settings: Settings,
    pub duration: f64,
    /// Cache for accelerating `params_at` lookups.
    cached_index: AtomicUsize,
}

impl Clone for Program {
    fn clone(&self) -> Self {
        Self {
            keyframes: self.keyframes.clone(),
            settings: self.settings,
            duration: self.duration,
            cached_index: AtomicUsize::new(0),
        }
    }
}

impl Program {
    /// Parse a program from source text.
    pub fn parse(source: &str) -> Result<Self> {
        let mut keyframes: Vec<Keyframe> = Vec::new();
        let mut settings = Settings::default();
        let mut current = Params::default();

        for (line_idx, line) in source.lines().enumerate() {
            let line_num = line_idx + 1;
            let line = line.trim();

            // Skip empty lines and comments
            if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
                continue;
            }

            let is_first = keyframes.is_empty();
            let kf = parse_line(line, &mut current, &mut settings, is_first)
                .with_context(|| format!("line {line_num}"))?;

            // Validate timestamp ordering
            if let Some(last) = keyframes.last() {
                if kf.time <= last.time {
                    bail!("line {line_num}: timestamps must strictly increase");
                }
            } else if kf.time != 0.0 {
                bail!("line {line_num}: first keyframe must be at 00:00");
            }

            keyframes.push(kf);
        }

        if keyframes.is_empty() {
            bail!("program contains no keyframes");
        }

        let last_time = keyframes.last().unwrap().time;

        let duration = if last_time > 0.0 { last_time } else { f64::INFINITY };

        Ok(Self {
            keyframes,
            settings,
            duration,
            cached_index: AtomicUsize::new(0),
        })
    }

    /// Load a program from a file.
    pub fn load(path: &Path) -> Result<Self> {
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("reading '{}'", path.display()))?;
        Self::parse(&source).with_context(|| format!("parsing '{}'", path.display()))
    }

    /// Create a constant (infinite duration) program from fixed parameters.
    pub fn constant(params: Params, settings: Settings) -> Self {
        Self {
            keyframes: vec![Keyframe {
                time: 0.0,
                params,
                curve: Curve::Step,
            }],
            settings,
            duration: f64::INFINITY,
            cached_index: AtomicUsize::new(0),
        }
    }

    /// Get interpolated parameters at the given time.
    ///
    /// Uses a cache to accelerate sequential lookups (O(1) for forward playback).
    #[inline]
    pub fn params_at(&self, time: f64) -> Params {
        let n = self.keyframes.len();

        // Fast paths for common cases
        if n == 1 {
            return self.keyframes[0].params;
        }
        if time <= 0.0 {
            return self.keyframes[0].params;
        }
        if time >= self.duration {
            return self.keyframes[n - 1].params;
        }

        // Try cached segment first (hot path for sequential access)
        let mut idx = self.cached_index.load(Ordering::Relaxed);

        // Validate cache: check if time is in [keyframes[idx-1].time, keyframes[idx].time)
        let cache_valid = idx > 0
            && idx < n
            && self.keyframes[idx - 1].time <= time
            && time < self.keyframes[idx].time;

        if !cache_valid {
            // Binary search for the segment containing time
            idx = self.keyframes.partition_point(|k| k.time <= time);
            self.cached_index.store(idx, Ordering::Relaxed);
        }

        // Interpolate between keyframes[idx-1] and keyframes[idx]
        let from = &self.keyframes[idx - 1];
        let to = &self.keyframes[idx];

        let span = to.time - from.time;
        let t = if span > 1e-12 {
            (time - from.time) / span
        } else {
            1.0
        };

        Params::lerp(&from.params, &to.params, to.curve.apply(t))
    }

    /// Export the program back to source format.
    pub fn to_source(&self) -> String {
        let mut out = String::with_capacity(256);
        out.push_str("// Entrainment Program\n");

        for (i, kf) in self.keyframes.iter().enumerate() {
            out.push_str(&format_timestamp(kf.time));

            let p = &kf.params;

            if i == 0 {
                // First keyframe: write all parameters
                write!(
                    out,
                    " freq={:.2} tone={:.0} vol={:.2} duty={:.2}",
                    p.freq, p.tone, p.vol, p.duty
                ).unwrap();
                write!(out, " on=#{:02X}{:02X}{:02X}", p.on.r, p.on.g, p.on.b).unwrap();
                write!(out, " off=#{:02X}{:02X}{:02X}", p.off.r, p.off.g, p.off.b).unwrap();

                if self.settings.binaural {
                    out.push_str(" binaural");
                }
                if self.settings.headless {
                    out.push_str(" headless");
                }
            } else {
                // Subsequent keyframes: only write changed parameters
                let prev = &self.keyframes[i - 1].params;

                if (p.freq - prev.freq).abs() > 0.001 {
                    write!(out, " freq={:.2}", p.freq).unwrap();
                }
                if (p.tone - prev.tone).abs() > 0.1 {
                    write!(out, " tone={:.0}", p.tone).unwrap();
                }
                if (p.vol - prev.vol).abs() > 0.001 {
                    write!(out, " vol={:.2}", p.vol).unwrap();
                }
                if (p.duty - prev.duty).abs() > 0.001 {
                    write!(out, " duty={:.2}", p.duty).unwrap();
                }
                if p.on != prev.on {
                    write!(out, " on=#{:02X}{:02X}{:02X}", p.on.r, p.on.g, p.on.b).unwrap();
                }
                if p.off != prev.off {
                    write!(out, " off=#{:02X}{:02X}{:02X}", p.off.r, p.off.g, p.off.b).unwrap();
                }

                if let Some(curve_str) = kf.curve.to_str() {
                    write!(out, " >{curve_str}").unwrap();
                }
            }

            out.push('\n');
        }

        out
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Parsing Utilities
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Parse a timestamp in MM:SS or HH:MM:SS format.
fn parse_timestamp(s: &str) -> Result<f64> {
    let parts: Vec<&str> = s.split(':').collect();
    match parts.len() {
        2 => {
            let m: f64 = parts[0].parse().context("invalid minutes")?;
            let s: f64 = parts[1].parse().context("invalid seconds")?;
            Ok(m * 60.0 + s)
        }
        3 => {
            let h: f64 = parts[0].parse().context("invalid hours")?;
            let m: f64 = parts[1].parse().context("invalid minutes")?;
            let s: f64 = parts[2].parse().context("invalid seconds")?;
            Ok(h * 3600.0 + m * 60.0 + s)
        }
        _ => bail!("invalid time format (expected MM:SS or HH:MM:SS)"),
    }
}

/// Format seconds as a timestamp string.
fn format_timestamp(secs: f64) -> String {
    let total_secs = secs.floor() as u64;
    let m = total_secs / 60;
    let s = total_secs % 60;
    let frac = secs.fract();

    if frac.abs() < 0.001 {
        format!("{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}.{:02}", (frac * 100.0).round() as u32)
    }
}

/// Parse a single program line into a keyframe.
fn parse_line(
    line: &str,
    current: &mut Params,
    settings: &mut Settings,
    is_first: bool,
) -> Result<Keyframe> {
    let mut tokens = line.split_whitespace();

    let timestamp = tokens.next().context("missing timestamp")?;
    let time = parse_timestamp(timestamp)?;
    let mut curve = Curve::Step;

    for token in tokens {
        // Curve directive: >curve
        if let Some(curve_name) = token.strip_prefix('>') {
            curve = Curve::parse(curve_name)?;
            continue;
        }

        // Key=value pairs
        if let Some((key, val)) = token.split_once('=') {
            match key {
                "freq" => {
                    current.freq = val.parse().context("invalid freq value")?;
                    if current.freq <= 0.0 {
                        bail!("freq must be positive");
                    }
                }
                "tone" => {
                    current.tone = val.parse().context("invalid tone value")?;
                    if current.tone <= 0.0 {
                        bail!("tone must be positive");
                    }
                }
                "vol" => {
                    current.vol = val
                        .parse::<f32>()
                        .context("invalid vol value")?
                        .clamp(0.0, 1.0);
                }
                "duty" => {
                    current.duty = val
                        .parse::<f32>()
                        .context("invalid duty value")?
                        .clamp(0.01, 0.99);
                }
                "on" => {
                    current.on = val
                        .parse()
                        .map_err(|e| anyhow::anyhow!("{e}"))
                        .context("invalid 'on' color")?;
                }
                "off" => {
                    current.off = val
                        .parse()
                        .map_err(|e| anyhow::anyhow!("{e}"))
                        .context("invalid 'off' color")?;
                }
                _ => bail!("unknown parameter '{key}'"),
            }
        } else {
            // Flags (only allowed on first line)
            if !is_first {
                bail!("setting '{token}' can only appear on the first line");
            }
            match token {
                "binaural" => settings.binaural = true,
                "headless" => settings.headless = true,
                _ => bail!("unknown setting '{token}'"),
            }
        }
    }

    Ok(Keyframe {
        time,
        params: *current,
        curve,
    })
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Tests
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_program() {
        let program = Program::parse("00:00 freq=20 vol=0\n00:10 vol=1 >linear").unwrap();
        assert_eq!(program.keyframes.len(), 2);
        assert!((program.duration - 10.0).abs() < 0.001);
    }

    #[test]
    fn linear_interpolation() {
        let program = Program::parse("00:00 freq=20 vol=0\n00:10 vol=1 >linear").unwrap();

        assert!((program.params_at(0.0).vol - 0.0).abs() < 0.001);
        assert!((program.params_at(5.0).vol - 0.5).abs() < 0.001);
        assert!((program.params_at(10.0).vol - 1.0).abs() < 0.001);
        assert!((program.params_at(15.0).vol - 1.0).abs() < 0.001); // past end
    }

    #[test]
    fn smooth_interpolation() {
        let program = Program::parse("00:00 freq=0\n00:10 freq=100 >smooth").unwrap();

        // Smoothstep should be 0.5 at t=0.5
        let mid = program.params_at(5.0).freq;
        assert!((mid - 50.0).abs() < 0.1);

        // Should be slower at edges
        let early = program.params_at(1.0).freq;
        assert!(early < 10.0); // Less than linear would give
    }

    #[test]
    fn settings_only_at_start() {
        assert!(Program::parse("00:00 freq=10\n00:10 binaural").is_err());
    }

    #[test]
    fn first_keyframe_must_be_zero() {
        assert!(Program::parse("00:05 freq=10").is_err());
    }

    #[test]
    fn timestamps_must_increase() {
        assert!(Program::parse("00:00 freq=10\n00:05 vol=1\n00:03 vol=0").is_err());
    }

    #[test]
    fn curve_functions() {
        // Step curve
        assert!((Curve::Step.apply(0.0) - 0.0).abs() < 0.001);
        assert!((Curve::Step.apply(0.5) - 0.0).abs() < 0.001);
        assert!((Curve::Step.apply(1.0) - 1.0).abs() < 0.001);

        // Linear curve
        assert!((Curve::Linear.apply(0.0) - 0.0).abs() < 0.001);
        assert!((Curve::Linear.apply(0.5) - 0.5).abs() < 0.001);
        assert!((Curve::Linear.apply(1.0) - 1.0).abs() < 0.001);

        // Smooth curve (smoothstep)
        assert!((Curve::Smooth.apply(0.0) - 0.0).abs() < 0.001);
        assert!((Curve::Smooth.apply(0.5) - 0.5).abs() < 0.001);
        assert!((Curve::Smooth.apply(1.0) - 1.0).abs() < 0.001);
    }

    #[test]
    fn color_parsing() {
        assert_eq!("#FF0000".parse::<Color>().unwrap(), Color { r: 255, g: 0, b: 0, a: 255 });
        assert_eq!("00FF00".parse::<Color>().unwrap(), Color { r: 0, g: 255, b: 0, a: 255 });
        assert!("invalid".parse::<Color>().is_err());
        assert!("#FFF".parse::<Color>().is_err());
    }

    #[test]
    fn roundtrip_to_source() {
        let original = "00:00 freq=10.00 tone=200 vol=0.50 duty=0.50 on=#FFFFFF off=#000000\n\
                        00:10 vol=1.00 >linear\n";
        let program = Program::parse(original).unwrap();
        let exported = program.to_source();

        // Parse the exported source and verify it produces the same params
        let reparsed = Program::parse(&exported).unwrap();
        let p1 = program.params_at(5.0);
        let p2 = reparsed.params_at(5.0);

        assert!((p1.freq - p2.freq).abs() < 0.01);
        assert!((p1.vol - p2.vol).abs() < 0.01);
    }
}