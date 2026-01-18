use crate::program::Program;
use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::StreamConfig;
use log::{error, info};
use std::f64::consts::TAU;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Sync State
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Shared state for audio-visual synchronization.
///
/// The audio thread writes to these atomics; the visual thread reads them
/// to maintain perfect sync with audio playback.
pub struct SyncState {
    /// Total frames written to the audio buffer (monotonically increasing).
    pub frames_written: AtomicU64,

    /// Current pulse phase [0, 1) as f64 bits.
    /// For isochronic mode, this is the amplitude envelope phase.
    /// For binaural mode, this is derived from the beat frequency.
    pub phase_bits: AtomicU64,

    /// Actual audio buffer size in frames (measured from first callback).
    pub buffer_frames: AtomicU32,

    /// Audio sample rate in Hz.
    pub sample_rate: AtomicU32,
}

impl SyncState {
    pub fn new() -> Self {
        Self {
            frames_written: AtomicU64::new(0),
            phase_bits: AtomicU64::new(0),
            buffer_frames: AtomicU32::new(0),
            sample_rate: AtomicU32::new(0),
        }
    }

    /// Get the current playback time in seconds, accounting for buffer latency.
    #[inline]
    pub fn playback_time(&self) -> f64 {
        let written = self.frames_written.load(Ordering::Acquire);
        let buffer = self.buffer_frames.load(Ordering::Acquire) as u64;
        let rate = self.sample_rate.load(Ordering::Acquire);

        if rate == 0 {
            return 0.0;
        }

        let played = written.saturating_sub(buffer);
        played as f64 / f64::from(rate)
    }

    /// Get the current pulse phase, compensated for buffer latency.
    #[inline]
    pub fn visual_phase(&self, freq: f64) -> f64 {
        let raw_phase = f64::from_bits(self.phase_bits.load(Ordering::Acquire));
        let buffer = self.buffer_frames.load(Ordering::Acquire);
        let rate = self.sample_rate.load(Ordering::Acquire);

        if rate == 0 {
            return 0.0;
        }

        // Rewind phase by buffer latency
        let latency_secs = f64::from(buffer) / f64::from(rate);
        let phase_offset = freq * latency_secs;

        (raw_phase - phase_offset).rem_euclid(1.0)
    }
}

impl Default for SyncState {
    fn default() -> Self {
        Self::new()
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Audio Engine
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Audio synthesis engine.
///
/// Processes audio buffers and maintains oscillator state.
pub struct AudioEngine {
    sample_rate: f64,
    program: Arc<Program>,
    sync: Arc<SyncState>,

    // Oscillator phases (f64 for long-session precision)
    left_phase: f64,
    right_phase: f64,
    pulse_phase: f64,

    // Frame counter for time calculation
    frame_count: u64,
}

impl AudioEngine {
    pub fn new(sample_rate: f64, program: Arc<Program>, sync: Arc<SyncState>) -> Self {
        Self {
            sample_rate,
            program,
            sync,
            left_phase: 0.0,
            right_phase: 0.0,
            pulse_phase: 0.0,
            frame_count: 0,
        }
    }

    /// Process an audio buffer. Called from the audio thread.
    pub fn process(&mut self, output: &mut [f32], channels: usize) {
        let frame_count = output.len() / channels;
        if frame_count == 0 {
            return;
        }

        // Update buffer size on first call (for latency compensation)
        if self.sync.buffer_frames.load(Ordering::Relaxed) == 0 {
            self.sync.buffer_frames.store(frame_count as u32, Ordering::Release);
        }

        // Calculate time range for this buffer
        let t_start = self.frame_count as f64 / self.sample_rate;
        let t_end = (self.frame_count + frame_count as u64) as f64 / self.sample_rate;

        // Get interpolated parameters at buffer boundaries
        let p_start = self.program.params_at(t_start);
        let p_end = self.program.params_at(t_end);

        // Dispatch to appropriate synthesis method
        if self.program.settings.binaural {
            self.process_binaural(output, channels, &p_start, &p_end);
        } else {
            self.process_isochronic(output, channels, &p_start, &p_end);
        }

        // Update frame counter
        self.frame_count += frame_count as u64;

        // Publish sync state
        self.sync.frames_written.store(self.frame_count, Ordering::Release);
        self.sync.phase_bits.store(self.pulse_phase.to_bits(), Ordering::Release);
    }

    /// Generate binaural beats (stereo frequency difference).
    fn process_binaural(
        &mut self,
        output: &mut [f32],
        channels: usize,
        p_start: &crate::program::Params,
        p_end: &crate::program::Params,
    ) {
        let frame_count = output.len() / channels;
        let inv_len = 1.0 / frame_count as f64;
        let inv_sr = 1.0 / self.sample_rate;

        let mut l_phase = self.left_phase;
        let mut r_phase = self.right_phase;

        for (i, frame) in output.chunks_exact_mut(channels).enumerate() {
            // Linear parameter interpolation within buffer
            let t = i as f64 * inv_len;

            let vol = f64::from(p_start.vol) + f64::from(p_end.vol - p_start.vol) * t;
            let tone = f64::from(p_start.tone) + f64::from(p_end.tone - p_start.tone) * t;
            let freq = p_start.freq + (p_end.freq - p_start.freq) * t;

            // Left channel: base tone, Right channel: base + beat frequency
            let l_inc = tone * inv_sr;
            let r_inc = (tone + freq) * inv_sr;

            let l_sample = (l_phase * TAU).sin() * vol;
            let r_sample = (r_phase * TAU).sin() * vol;

            frame[0] = l_sample as f32;
            if channels >= 2 {
                frame[1] = r_sample as f32;
            }

            // Advance phases (keep in [0, 1) for numerical stability)
            l_phase = (l_phase + l_inc).fract();
            r_phase = (r_phase + r_inc).fract();
        }

        self.left_phase = l_phase;
        self.right_phase = r_phase;

        // For binaural, pulse_phase tracks the beat phase for visual sync
        let avg_freq = (p_start.freq + p_end.freq) * 0.5;
        let phase_inc = avg_freq * (frame_count as f64 / self.sample_rate);
        self.pulse_phase = (self.pulse_phase + phase_inc).fract();
    }

    /// Generate isochronic tones (amplitude-modulated carrier).
    fn process_isochronic(
        &mut self,
        output: &mut [f32],
        channels: usize,
        p_start: &crate::program::Params,
        p_end: &crate::program::Params,
    ) {
        let frame_count = output.len() / channels;
        let inv_len = 1.0 / frame_count as f64;
        let inv_sr = 1.0 / self.sample_rate;

        let mut tone_phase = self.left_phase;
        let mut pulse_phase = self.pulse_phase;

        for (i, frame) in output.chunks_exact_mut(channels).enumerate() {
            // Linear parameter interpolation within buffer
            let t = i as f64 * inv_len;

            let vol = f64::from(p_start.vol) + f64::from(p_end.vol - p_start.vol) * t;
            let tone = f64::from(p_start.tone) + f64::from(p_end.tone - p_start.tone) * t;
            let freq = p_start.freq + (p_end.freq - p_start.freq) * t;
            let duty = f64::from(p_start.duty) + f64::from(p_end.duty - p_start.duty) * t;

            // Phase increments
            let tone_inc = tone * inv_sr;
            let pulse_inc = freq * inv_sr;

            // Generate carrier tone
            let carrier = (tone_phase * TAU).sin();

            // Generate smooth envelope to avoid clicks
            // Ramp duration is 10% of period or half the duty cycle, whichever is smaller
            let ramp = 0.1_f64.min(duty * 0.5);
            let inv_ramp = if ramp > 1e-9 { 1.0 / ramp } else { 1e9 };

            let envelope = if pulse_phase >= duty {
                0.0
            } else {
                // Trapezoidal envelope with smooth edges
                let attack = (pulse_phase * inv_ramp).min(1.0);
                let release = ((duty - pulse_phase) * inv_ramp).min(1.0);
                let linear = attack.min(release);
                // Apply smoothstep for softer transitions
                linear * linear * (3.0 - 2.0 * linear)
            };

            let sample = (carrier * envelope * vol) as f32;

            frame[0] = sample;
            if channels >= 2 {
                frame[1] = sample;
            }

            // Advance phases
            tone_phase = (tone_phase + tone_inc).fract();
            pulse_phase = (pulse_phase + pulse_inc).fract();
        }

        self.left_phase = tone_phase;
        self.pulse_phase = pulse_phase;
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Audio Setup
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Initialize audio output and start playback.
///
/// Returns the stream handle (must be kept alive) and initializes the sync state.
pub fn start(program: Arc<Program>, sync: Arc<SyncState>) -> Result<cpal::Stream> {
    let host = cpal::default_host();

    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("No audio output device available"))?;

    let device_name = device.description().map(|d| d.name().to_owned())?;
    info!("Audio device: {device_name}");

    let config: StreamConfig = device.default_output_config()?.into();
    let sample_rate = config.sample_rate;
    let channels = config.channels as usize;

    info!("Audio config: {sample_rate} Hz, {channels} channels");

    // Store sample rate in sync state
    sync.sample_rate.store(sample_rate, Ordering::Release);

    // Create engine
    let mut engine = AudioEngine::new(f64::from(sample_rate), program, sync);

    // Build and start stream
    let stream = device.build_output_stream(
        &config,
        move |data: &mut [f32], _info| {
            engine.process(data, channels);
        },
        |err| error!("Audio stream error: {err}"),
        None,
    )?;

    stream.play()?;

    Ok(stream)
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Tests
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program::{Params, Settings};

    fn test_program() -> Arc<Program> {
        Arc::new(Program::constant(Params::default(), Settings::default()))
    }

    #[test]
    fn engine_produces_output() {
        let sync = Arc::new(SyncState::new());
        let mut engine = AudioEngine::new(48000.0, test_program(), sync.clone());

        let mut buffer = vec![0.0f32; 1024];
        engine.process(&mut buffer, 2);

        // Should have written some non-zero samples
        assert!(buffer.iter().any(|&s| s.abs() > 0.001));

        // Sync state should be updated
        assert!(sync.frames_written.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn phase_wraps_correctly() {
        let sync = Arc::new(SyncState::new());
        let mut engine = AudioEngine::new(48000.0, test_program(), sync);

        let mut buffer = vec![0.0f32; 48000]; // 1 second of audio

        // Process many buffers to accumulate phase
        for _ in 0..100 {
            engine.process(&mut buffer, 2);
        }

        // Phases should remain in [0, 1)
        assert!(engine.left_phase >= 0.0 && engine.left_phase < 1.0);
        assert!(engine.pulse_phase >= 0.0 && engine.pulse_phase < 1.0);
    }

    #[test]
    fn sync_state_latency_compensation() {
        let sync = SyncState::new();
        sync.sample_rate.store(48000, Ordering::Relaxed);
        sync.buffer_frames.store(1024, Ordering::Relaxed);
        sync.frames_written.store(48000, Ordering::Relaxed); // 1 second written
        sync.phase_bits.store(0.5_f64.to_bits(), Ordering::Relaxed);

        // Playback time should be ~1 second minus buffer latency
        let time = sync.playback_time();
        let expected = 1.0 - (1024.0 / 48000.0);
        assert!((time - expected).abs() < 0.001);

        // Visual phase at 10 Hz should be rewound by latency
        let phase = sync.visual_phase(10.0);
        // 10 Hz * (1024/48000) seconds = ~0.213 cycles offset
        assert!(phase >= 0.0 && phase < 1.0);
    }
}