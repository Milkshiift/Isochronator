use crate::{AppConfig, TimingState};
use anyhow::Result;
use cpal::StreamConfig;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use log::{error, info};
use std::f32::consts::TAU;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy)]
struct Oscillator {
    phase: f32,
    inc: f32,
}

impl Oscillator {
    fn new(freq: f32, sample_rate: f32) -> Self {
        Self {
            phase: 0.0,
            inc: freq / sample_rate,
        }
    }
}

pub struct AudioEngine {
    amplitude: f32,
    binaural: bool,

    left: Oscillator,
    right: Oscillator,
    pulse: Oscillator,

    pulse_width: f32,
    inv_ramp: f32,

    // Shared counter for perfect A/V sync
    frames_written: Arc<AtomicU64>,
}

impl AudioEngine {
    pub fn new(
        sample_rate: f32,
        frequency: f64,
        period_secs: f64,
        config: &AppConfig,
        frames_written: Arc<AtomicU64>,
    ) -> Self {
        let left_freq = config.tone_hz;
        // Pre-calculate right frequency for Binaural mode
        let right_freq_bin = config.tone_hz + frequency as f32;

        let ramp_duration_ratio = (config.ramp_duration / period_secs as f32).min(0.5);

        Self {
            amplitude: config.amplitude,
            binaural: config.binaural,

            left: Oscillator::new(left_freq, sample_rate),
            right: Oscillator::new(right_freq_bin, sample_rate),
            pulse: Oscillator::new(frequency as f32, sample_rate),

            pulse_width: config.duty_cycle,
            inv_ramp: if ramp_duration_ratio > 0.0 {
                1.0 / ramp_duration_ratio
            } else {
                0.0
            },
            frames_written,
        }
    }

    pub fn process_buffer(&mut self, output: &mut [f32], channels: usize) {
        if self.binaural {
            self.process_binaural(output, channels);
        } else {
            self.process_isochronic(output, channels);
        }

        // Update the atomic counter so the visual thread knows exactly where we are.
        // Relaxed ordering is sufficient; we just need a monotonic approximation for visuals.
        let frames = output.len() / channels;
        self.frames_written
            .fetch_add(frames as u64, Ordering::Relaxed);
    }

    #[inline(always)]
    fn process_binaural(&mut self, output: &mut [f32], channels: usize) {
        // Load state into local variables (CPU registers)
        let mut l_phase = self.left.phase;
        let l_inc = self.left.inc;
        let mut r_phase = self.right.phase;
        let r_inc = self.right.inc;
        let amp = self.amplitude;

        // Process directly on the slice.
        // No "if" checks inside here. Just math.
        for frame in output.chunks_exact_mut(channels) {
            let l_val = (l_phase * TAU).sin();
            let r_val = (r_phase * TAU).sin();

            // Interleave Output
            if channels >= 2 {
                frame[0] = l_val * amp;
                frame[1] = r_val * amp;
            } else {
                frame[0] = ((l_val + r_val) * 0.5) * amp;
            }

            // Phase Updates
            l_phase += l_inc;
            if l_phase >= 1.0 {
                l_phase -= 1.0;
            }
            r_phase += r_inc;
            if r_phase >= 1.0 {
                r_phase -= 1.0;
            }
        }

        // Save state back
        self.left.phase = l_phase;
        self.right.phase = r_phase;
    }

    #[inline(always)]
    fn process_isochronic(&mut self, output: &mut [f32], channels: usize) {
        let mut l_phase = self.left.phase;
        let l_inc = self.left.inc;
        let mut p_phase = self.pulse.phase;
        let p_inc = self.pulse.inc;

        let p_width = self.pulse_width;
        let inv_ramp = self.inv_ramp;
        let amp = self.amplitude;

        for frame in output.chunks_exact_mut(channels) {
            // 1. Calculate Carrier Tone (Left only, saves 50% trig calls)
            let raw_tone = (l_phase * TAU).sin();

            // 2. Calculate Envelope (Branchless-ish)
            let envelope = if p_phase > p_width {
                0.0
            } else {
                let up = p_phase * inv_ramp;
                let down = (p_width - p_phase) * inv_ramp;
                // Branchless min
                let linear = if up < down { up } else { down };

                if linear >= 1.0 {
                    1.0
                } else {
                    // Hermite SmoothStep
                    linear * linear * (3.0 - 2.0 * linear)
                }
            };

            let final_val = raw_tone * envelope * amp;

            if channels >= 2 {
                frame[0] = final_val;
                frame[1] = final_val;
            } else {
                frame[0] = final_val;
            }

            // Phase Updates
            l_phase += l_inc;
            if l_phase >= 1.0 {
                l_phase -= 1.0;
            }
            p_phase += p_inc;
            if p_phase >= 1.0 {
                p_phase -= 1.0;
            }
        }

        self.left.phase = l_phase;
        self.pulse.phase = p_phase;
    }
}

fn build_audio_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    timing_state: Arc<TimingState>,
    app_config: &AppConfig,
    frames_written: Arc<AtomicU64>,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    let channels = config.channels as usize;
    let sample_rate = config.sample_rate as f32;

    let mut engine = AudioEngine::new(
        sample_rate,
        timing_state.frequency,
        timing_state.period_secs,
        app_config,
        frames_written,
    );

    device.build_output_stream(
        config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            engine.process_buffer(data, channels);
        },
        |err| error!("Audio stream error: {err}"),
        None,
    )
}

pub fn setup_audio(
    timing_state: Arc<TimingState>,
    config: &AppConfig,
    frames_written: Arc<AtomicU64>,
) -> Result<(cpal::Stream, u32)> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("No default audio device found"))?;
    info!("Audio output device: {}", device.description()?.name());
    let stream_config: StreamConfig = device.default_output_config()?.into();
    let sample_rate = stream_config.sample_rate;

    let stream = build_audio_stream(
        &device,
        &stream_config,
        timing_state,
        config,
        frames_written,
    )?;
    stream.play()?;

    Ok((stream, sample_rate))
}
