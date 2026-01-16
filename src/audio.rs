use std::f32::consts::{PI, TAU};
use std::sync::Arc;
use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use log::{error, info};
use crate::{AppConfig, TimingState};

pub const PULSE_WIDTH: f64 = 0.5;

pub struct AudioEngine {
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
    pub fn new(sample_rate: f32, timing: &TimingState, config: &AppConfig) -> Self {
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

    pub fn next_sample(&mut self) -> (f32, f32) {
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
        self.pulse_phase %= 1.0;
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

fn build_audio_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    timing_state: Arc<TimingState>,
    app_config: &AppConfig,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    let channels = config.channels as usize;
    let sample_rate = config.sample_rate as f32;
    let mut engine = AudioEngine::new(sample_rate, &timing_state, app_config);

    device.build_output_stream(config, move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
        for frame in data.chunks_mut(channels) {
            let (left, right) = engine.next_sample();
            if channels >= 2 {
                frame[0] = left;
                frame[1] = right;
            } else {
                frame[0] = (left + right) * 0.5;
            }
        }
    }, |err| error!("Audio stream error: {err}"), None)
}

pub fn setup_audio(timing_state: Arc<TimingState>, config: &AppConfig) -> Result<cpal::Stream> {
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or_else(|| anyhow::anyhow!("No default audio device"))?;
    info!("Audio output device: {}", device.description()?.name());
    let stream_config = device.default_output_config()?.into();
    let stream = build_audio_stream(&device, &stream_config, timing_state, config)?;
    stream.play()?;
    Ok(stream)
}