use anyhow::{Context, Result};
use log::{debug, error, info, warn};
use rubato::{SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::audio_processing::{
    audio_to_mono, HighPassFilter, LoudnessNormalizer, NoiseSuppressionProcessor,
};
use super::capture_resampler::CaptureResampler;
use super::devices::AudioDevice;
use super::diarization_stems::StemWindow;
use super::recording_state::{AudioChunk, AudioError, DeviceType, RecordingState, SourceActivity};
use super::source_gate::SourceGatedInput;
use super::vad::{ContinuousVadProcessor, SpeechSegment};

/// How long a silence must last before the VAD closes a speech segment, and
/// therefore how long the audio clips handed to the ASR engine are.
///
/// Live-path policy: 500ms (matches the established Meetily Pro live policy).
/// The batch paths (`import.rs` / `retranscription.rs`) use 2000ms instead —
/// they have no latency requirement, so they optimize purely for ASR request
/// length. The live path cannot: with continuous audio (e.g. a podcast played
/// as system audio) a 2000ms redemption keeps one VAD segment open
/// indefinitely, withholds live transcript emission, and overruns the
/// accumulated-speech-buffer warning threshold. Bounded live segmentation
/// during continuous speech is tracked separately in #756.
const VAD_REDEMPTION_TIME_MS: u32 = 500;

/// Send final speech segments and preserve flush or delivery errors.
fn forward_final_segments(
    final_segments: Result<Vec<SpeechSegment>>,
    mut send: impl FnMut(SpeechSegment) -> Result<()>,
) -> Result<()> {
    for segment in final_segments.context("Failed to flush final VAD audio")? {
        // Use the same 50 ms minimum as the live pipeline.
        if segment.samples.len() >= 800 {
            info!(
                "Sending final VAD segment: {:.1} ms, {} samples",
                segment.end_timestamp_ms - segment.start_timestamp_ms,
                segment.samples.len()
            );
            send(segment).context("Failed to send final VAD segment")?;
        } else {
            debug!(
                "Skipping short final VAD segment: {} samples < 800",
                segment.samples.len()
            );
        }
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod linux_mixing_tests {
    use super::*;

    #[test]
    fn unequal_delivery_boundaries_do_not_expand_the_timeline() {
        let mut ring = AudioMixerRingBuffer::new(48000);
        let mut positions = [0u64; 2];
        let mut output = [Vec::new(), Vec::new()];
        for _ in 0..6 {
            for (role, index, count) in [(DeviceType::Microphone, 0, 16384), (DeviceType::System, 1, 8192), (DeviceType::System, 1, 8192)] {
                let samples = (positions[index]..positions[index] + count).map(|n| n as f32).collect();
                ring.add_samples_at(role, samples, positions[index]);
                positions[index] += count;
                while let Some((mic, system)) = ring.extract_aligned(false) {
                    output[0].extend(mic); output[1].extend(system);
                }
            }
        }
        while let Some((mic, system)) = ring.extract_aligned(true) {
            output[0].extend(mic); output[1].extend(system);
        }
        let expected: Vec<_> = (0..98304).map(|n| n as f32).collect();
        assert_eq!(output[0], expected);
        assert_eq!(output[1], expected);
    }

    #[test]
    fn counterpart_within_lead_is_kept_and_beyond_lead_is_padded_once() {
        let mut ring = AudioMixerRingBuffer::new(48000);
        // Mic leads by 330 ms, the delivery spread measured between PipeWire sources.
        ring.add_samples_at(DeviceType::Microphone, vec![1.0; 28800 + 15840], 0);
        assert!(ring.extract_aligned(false).is_none());
        ring.add_samples_at(DeviceType::System, vec![3.0; 28800], 0);
        let (mic, system) = ring.extract_aligned(false).unwrap();
        assert_eq!(mic, vec![1.0; 28800]);
        assert_eq!(system, vec![3.0; 28800]);
        // A silent counterpart is padded only once the leader is a full second past the window.
        ring.add_samples_at(DeviceType::Microphone, vec![2.0; 105599 - 44640], 44640);
        assert!(ring.extract_aligned(false).is_none());
        ring.add_samples_at(DeviceType::Microphone, vec![2.0], 105599);
        let (mic, system) = ring.extract_aligned(false).unwrap();
        assert_eq!(&mic[..15840], &[1.0; 15840]);
        assert_eq!(system, vec![0.0; 28800]);
        // Its late arrival is not replayed into later windows.
        ring.add_samples_at(DeviceType::System, vec![3.0; 28800], 28800);
        while let Some((_, system)) = ring.extract_aligned(true) {
            assert!(system.iter().all(|&sample| sample == 0.0));
        }
    }

    #[test]
    fn earlier_counterpart_is_retained_before_the_first_window_is_emitted() {
        let mut ring = AudioMixerRingBuffer::new(48000);
        ring.add_samples_at(DeviceType::System, vec![2.0; 3], 2);
        ring.add_samples_at(DeviceType::Microphone, vec![1.0; 5], 0);
        let (mic, system) = ring.extract_aligned(true).unwrap();
        assert_eq!(mic, vec![1.0; 5]);
        assert_eq!(system, vec![0.0, 0.0, 2.0, 2.0, 2.0]);
    }

    #[test]
    fn single_role_flush_preserves_exact_partial_window_and_gaps() {
        let mut ring = AudioMixerRingBuffer::new(48000);
        ring.add_samples_at(DeviceType::System, vec![1.0; 3], 10);
        ring.add_samples_at(DeviceType::System, vec![2.0; 2], 15);
        let (mic, system) = ring.extract_aligned(true).unwrap();
        assert_eq!(mic, vec![0.0; 7]);
        assert_eq!(system, vec![1.0, 1.0, 1.0, 0.0, 0.0, 2.0, 2.0]);
    }

    #[test]
    fn recovery_gap_preserves_the_tail_and_every_recovered_sample() {
        for role in [DeviceType::Microphone, DeviceType::System] {
            for gap in [230401, 6 * 48000] {
                let mut ring = AudioMixerRingBuffer::new(48000);
                ring.add_samples_at(role.clone(), vec![1.0; 28811], 0);
                ring.extract_aligned(true).unwrap();
                ring.add_samples_at(role.clone(), vec![0.75; 960], 28811 + gap);
                let mut output = Vec::new();
                while let Some((mic, system)) = ring.extract_aligned(true) {
                    output.extend(if role == DeviceType::Microphone { mic } else { system });
                }
                let mut expected = vec![1.0; 11];
                expected.resize(11 + gap as usize, 0.0);
                expected.extend(vec![0.75; 960]);
                assert_eq!(output.len(), expected.len());
                assert_eq!(output, expected);
                assert_eq!(ring.take_dropped_samples(), (0, 0));
            }
        }
    }

    #[test]
    fn overlapping_packets_keep_first_arrival_and_partial_tails() {
        let mut ring = AudioMixerRingBuffer::new(48000);
        ring.window_size_samples = 4;
        ring.add_samples_at(DeviceType::Microphone, vec![1.0, 2.0, 3.0, 4.0, 5.0], 10);
        ring.add_samples_at(DeviceType::Microphone, vec![9.0, 9.0, 6.0, 7.0], 13);
        let (first, _) = ring.extract_aligned(true).unwrap();
        let (tail, _) = ring.extract_aligned(true).unwrap();
        assert_eq!(first, vec![1.0, 2.0, 3.0, 4.0]);
        assert_eq!(tail, vec![5.0, 6.0, 7.0]);
        assert!(ring.extract_aligned(true).is_none());
    }

    #[test]
    fn capacity_counts_real_samples_and_drops_only_the_newest_overflow() {
        let mut ring = AudioMixerRingBuffer::new(48000);
        ring.window_size_samples = 4;
        ring.max_buffer_size = 8;
        ring.add_samples_at(DeviceType::Microphone, vec![1.0; 3], 0);
        ring.add_samples_at(DeviceType::Microphone, vec![2.0; 6], 100);
        assert_eq!(ring.take_dropped_samples(), (1, 0));
        let mut output = Vec::new();
        while let Some((mic, _)) = ring.extract_aligned(true) {
            output.extend(mic);
        }
        assert_eq!(output.len(), 105);
        assert_eq!(&output[..3], &[1.0; 3]);
        assert!(output[3..100].iter().all(|&sample| sample == 0.0));
        assert_eq!(&output[100..], &[2.0; 5]);
    }
}

/// Ring buffer for synchronized audio mixing
/// Accumulates samples from mic and system streams until we have aligned windows
struct AudioMixerRingBuffer {
    #[cfg(not(target_os = "linux"))]
    mic_buffer: VecDeque<f32>,
    #[cfg(not(target_os = "linux"))]
    system_buffer: VecDeque<f32>,
    #[cfg(target_os = "linux")]
    mic_buffer: VecDeque<PositionedAudio>,
    #[cfg(target_os = "linux")]
    system_buffer: VecDeque<PositionedAudio>,
    window_size_samples: usize, // Fixed mixing window (e.g., 50ms)
    max_buffer_size: usize,     // Safety limit (e.g., 400ms)
    mic_dropped_samples: u64,
    system_dropped_samples: u64,
    #[cfg(target_os = "linux")]
    position: Option<u64>,
    #[cfg(target_os = "linux")]
    emitted: bool,
}

/// How far one role may run ahead before the other is padded with silence (1 s at 48 kHz).
#[cfg(target_os = "linux")]
const LINUX_MAX_STREAM_LEAD_SAMPLES: u64 = 48_000;

#[cfg(target_os = "linux")]
struct PositionedAudio {
    start: u64,
    samples: VecDeque<f32>,
}

#[derive(Debug)]
struct SourceActivityWindow {
    start: f64,
    end: f64,
    microphone_active: bool,
    system_active: bool,
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32).sqrt()
}

impl AudioMixerRingBuffer {
    fn new(sample_rate: u32) -> Self {
        // Use 50ms windows for mixing
        let window_ms = super::constants::MIXING_WINDOW_MS as f32;
        let window_size_samples = (sample_rate as f32 * window_ms / 1000.0) as usize;

        // CRITICAL FIX: Increase max buffer to 400ms for system audio stability
        // System audio (especially Core Audio on macOS) can have significant jitter
        // due to sample-by-sample streaming → batching → channel transmission
        // Accounts for: RNNoise buffering + Core Audio jitter + processing delays
        let max_buffer_size = window_size_samples * 8; // 400ms (was 200ms)

        info!(
            "🔊 Ring buffer initialized: window={}ms ({} samples), max={}ms ({} samples)",
            window_ms,
            window_size_samples,
            window_ms * 8.0,
            max_buffer_size
        );

        Self {
            #[cfg(not(target_os = "linux"))]
            mic_buffer: VecDeque::with_capacity(max_buffer_size),
            #[cfg(not(target_os = "linux"))]
            system_buffer: VecDeque::with_capacity(max_buffer_size),
            #[cfg(target_os = "linux")]
            mic_buffer: VecDeque::new(),
            #[cfg(target_os = "linux")]
            system_buffer: VecDeque::new(),
            window_size_samples,
            max_buffer_size,
            mic_dropped_samples: 0,
            system_dropped_samples: 0,
            #[cfg(target_os = "linux")]
            position: None,
            #[cfg(target_os = "linux")]
            emitted: false,
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn add_samples(&mut self, device_type: DeviceType, samples: Vec<f32>) {
        match device_type {
            DeviceType::Microphone => self.mic_buffer.extend(samples),
            DeviceType::System => self.system_buffer.extend(samples),
        }

        if self.mic_buffer.len() > self.max_buffer_size {
            self.mic_dropped_samples += (self.mic_buffer.len() - self.max_buffer_size) as u64;
        }
        if self.system_buffer.len() > self.max_buffer_size {
            self.system_dropped_samples += (self.system_buffer.len() - self.max_buffer_size) as u64;
        }
        while self.mic_buffer.len() > self.max_buffer_size {
            self.mic_buffer.pop_front();
        }
        while self.system_buffer.len() > self.max_buffer_size {
            self.system_buffer.pop_front();
        }
    }

    #[cfg(target_os = "linux")]
    fn add_samples_at(&mut self, role: DeviceType, samples: Vec<f32>, position: u64) {
        if samples.is_empty() { return; }
        let mut cursor = *self.position.get_or_insert(position);
        if !self.emitted && position < cursor {
            cursor = position;
            self.position = Some(position);
        }
        let (buffer, dropped) = match role {
            DeviceType::Microphone => (&mut self.mic_buffer, &mut self.mic_dropped_samples),
            DeviceType::System => (&mut self.system_buffer, &mut self.system_dropped_samples),
        };
        let end = buffer.back().map_or(cursor, |packet| packet.start + packet.samples.len() as u64).max(cursor);
        let skip = end.saturating_sub(position).min(samples.len() as u64) as usize;
        if skip == samples.len() { return; }
        // Gaps consume timeline, not storage that recovered speech needs.
        let buffered: usize = buffer.iter().map(|packet| packet.samples.len()).sum();
        let accepted = (samples.len() - skip).min(self.max_buffer_size - buffered);
        *dropped += (samples.len() - skip - accepted) as u64;
        if accepted > 0 {
            buffer.push_back(PositionedAudio {
                start: position + skip as u64,
                samples: samples.into_iter().skip(skip).take(accepted).collect(),
            });
        }
    }

    #[cfg(target_os = "linux")]
    fn extract_aligned(&mut self, flush: bool) -> Option<(Vec<f32>, Vec<f32>)> {
        let cursor = self.position?;
        let ends = [&self.mic_buffer, &self.system_buffer].map(|buffer|
            buffer.back().map_or(cursor, |packet| packet.start + packet.samples.len() as u64));
        let available = ends[0].max(ends[1]) - cursor;
        if available == 0 || (!flush && available < self.window_size_samples as u64) { return None; }
        let count = available.min(self.window_size_samples as u64) as usize;
        let end = cursor + count as u64;
        // Pad a lagging role only once the other is a full second past this window. Capture
        // sources differ in delivery latency (150-330 ms measured on PipeWire), and padding sooner
        // zero-filled the window and then discarded the real samples when they arrived.
        if !flush && ends[0].min(ends[1]) < end && ends[0].max(ends[1]) < end + LINUX_MAX_STREAM_LEAD_SAMPLES {
            return None;
        }
        let take = |buffer: &mut VecDeque<PositionedAudio>| {
            let mut samples = vec![0.0; count];
            while let Some(packet) = buffer.front_mut() {
                if packet.start >= end { break; }
                let offset = (packet.start - cursor) as usize;
                let take = packet.samples.len().min(count - offset);
                for (output, input) in samples[offset..offset + take].iter_mut().zip(packet.samples.drain(..take)) {
                    *output = input;
                }
                packet.start += take as u64;
                if packet.samples.is_empty() { buffer.pop_front(); }
                else { break; }
            }
            samples
        };
        let result = (take(&mut self.mic_buffer), take(&mut self.system_buffer));
        self.emitted = true;
        *self.position.as_mut().unwrap() += count as u64;
        Some(result)
    }

    fn take_dropped_samples(&mut self) -> (u64, u64) {
        let dropped = (self.mic_dropped_samples, self.system_dropped_samples);
        self.mic_dropped_samples = 0;
        self.system_dropped_samples = 0;
        dropped
    }

    #[cfg(not(target_os = "linux"))]
    fn can_mix(&self) -> bool {
        self.mic_buffer.len() >= self.window_size_samples
            || self.system_buffer.len() >= self.window_size_samples
    }

    #[cfg(not(target_os = "linux"))]
    fn extract_remaining(&mut self) -> Option<(Vec<f32>, Vec<f32>)> {
        let length = self.mic_buffer.len().max(self.system_buffer.len());
        if length == 0 {
            return None;
        }
        let mut mic: Vec<f32> = self.mic_buffer.drain(..).collect();
        let mut system: Vec<f32> = self.system_buffer.drain(..).collect();
        mic.resize(length, 0.0);
        system.resize(length, 0.0);
        Some((mic, system))
    }

    #[cfg(not(target_os = "linux"))]
    fn extract_window(&mut self) -> Option<(Vec<f32>, Vec<f32>)> {
        if !self.can_mix() {
            return None;
        }

        // Extract mic window with zero-padding for incomplete buffers
        // Zero-padding (silence) is preferred over last-sample-hold to prevent artifacts

        // Extract mic window (or pad with zeros if insufficient data)
        let mic_window = if self.mic_buffer.len() >= self.window_size_samples {
            // Enough mic data - drain window
            self.mic_buffer.drain(0..self.window_size_samples).collect()
        } else if !self.mic_buffer.is_empty() {
            // Some mic data but not enough - consume all + pad with zeros
            let available: Vec<f32> = self.mic_buffer.drain(..).collect();
            let mut padded = Vec::with_capacity(self.window_size_samples);
            padded.extend_from_slice(&available);

            // Use zero-padding (silence) to prevent repetition artifacts
            // Zero-padding is inaudible at 48kHz sample rate
            padded.resize(self.window_size_samples, 0.0);

            padded
        } else {
            // No mic data - return silence
            vec![0.0; self.window_size_samples]
        };

        // Extract system window (or pad with zeros if insufficient data)
        let sys_window = if self.system_buffer.len() >= self.window_size_samples {
            // Enough system data - drain window
            self.system_buffer
                .drain(0..self.window_size_samples)
                .collect()
        } else if !self.system_buffer.is_empty() {
            // Some system data but not enough - consume all + pad with zeros
            let available: Vec<f32> = self.system_buffer.drain(..).collect();
            let mut padded = Vec::with_capacity(self.window_size_samples);
            padded.extend_from_slice(&available);

            // Use zero-padding (silence) to prevent repetition artifacts
            // Zero-padding is inaudible at 48kHz sample rate
            padded.resize(self.window_size_samples, 0.0);

            padded
        } else {
            // No system data - return silence
            vec![0.0; self.window_size_samples]
        };

        Some((mic_window, sys_window))
    }
}

/// Simple audio mixer without aggressive ducking
/// Combines mic + system audio with basic clipping prevention
struct ProfessionalAudioMixer;

impl ProfessionalAudioMixer {
    fn new(_sample_rate: u32) -> Self {
        Self
    }

    fn mix_window(&mut self, mic_window: &[f32], sys_window: &[f32]) -> Vec<f32> {
        // Handle different lengths (already padded by extract_window, but defensive)
        let max_len = mic_window.len().max(sys_window.len());
        let mut mixed = Vec::with_capacity(max_len);

        // Professional mixing with soft scaling to prevent distortion
        // Uses proportional scaling instead of hard clamping to avoid artifacts
        for i in 0..max_len {
            let mic = mic_window.get(i).copied().unwrap_or(0.0);
            let sys = sys_window.get(i).copied().unwrap_or(0.0);

            // Pre-scale system audio to 70% to leave headroom
            // This prevents constant soft scaling which can cause pumping artifacts
            // Mic is normalized to -23 LUFS (already optimal), system needs reduction
            let sys_scaled = sys * 1.0;
            let _mic_scaled = mic * 0.8; // Reserved for future mic scaling

            // Sum without ducking - mic stays at full volume, system slightly reduced
            let sum = mic + sys_scaled;

            // CRITICAL FIX: Soft scaling prevents distortion artifacts
            // If the sum would exceed ±1.0, scale down PROPORTIONALLY
            // This avoids hard clipping distortion that sounds like "radio breaks"
            let sum_abs = sum.abs();
            let mixed_sample = if sum_abs > 1.0 {
                // Scale down to fit within ±1.0
                sum / sum_abs
            } else {
                sum
            };

            mixed.push(mixed_sample);
        }

        mixed
    }
}

/// Lock a mutex even if a callback panicked while holding it, so stop can still drain.
pub(super) fn lock_unpoisoned<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Simplified audio capture without broadcast channels
#[derive(Clone)]
pub struct AudioCapture {
    device: Arc<AudioDevice>,
    state: Arc<RecordingState>,
    sample_rate: u32, // Original device sample rate
    channels: u16,
    chunk_counter: Arc<std::sync::atomic::AtomicU64>,
    device_type: DeviceType,
    recording_sender: Option<mpsc::UnboundedSender<AudioChunk>>,
    needs_resampling: bool, // Flag if resampling is required
    resampler: Arc<std::sync::Mutex<Option<CaptureResampler>>>,
    // One lock orders callbacks, Core Audio batching, and the terminal drain.
    processing: Arc<std::sync::Mutex<CaptureProcessing>>,
    // Audio enhancement processors (microphone only)
    noise_suppressor: Arc<std::sync::Mutex<Option<NoiseSuppressionProcessor>>>,
    high_pass_filter: Arc<std::sync::Mutex<Option<HighPassFilter>>>,
    // EBU R128 normalizer for microphone audio (per-device, stateful)
    normalizer: Arc<std::sync::Mutex<Option<LoudnessNormalizer>>>,
    #[cfg(target_os = "linux")]
    fallback_clock: Arc<std::sync::Mutex<(Option<f64>, u64)>>,
}

struct CaptureProcessing {
    finished: bool,
    pending_samples: Vec<f32>,
}

impl AudioCapture {
    pub fn new(
        device: Arc<AudioDevice>,
        state: Arc<RecordingState>,
        sample_rate: u32,
        channels: u16,
        device_type: DeviceType,
        recording_sender: Option<mpsc::UnboundedSender<AudioChunk>>,
    ) -> Self {
        // CRITICAL FIX: Detect if resampling is needed
        // Pipeline expects 48kHz, but Bluetooth devices often report 8kHz, 16kHz, or 44.1kHz
        const TARGET_SAMPLE_RATE: u32 = 48000;
        let needs_resampling = sample_rate != TARGET_SAMPLE_RATE;

        // Detect device kind (Bluetooth vs Wired) for adaptive processing
        // Use reasonable defaults for buffer size (512 samples is typical)
        let device_kind =
            super::device_detection::InputDeviceKind::detect(&device.name, 512, sample_rate);

        if needs_resampling {
            warn!("⚠️ SAMPLE RATE MISMATCH DETECTED ⚠️");
            warn!(
                "🔄 [{:?}] Audio device '{}' ({:?}) reports {} Hz (pipeline expects {} Hz)",
                device_type, device.name, device_kind, sample_rate, TARGET_SAMPLE_RATE
            );
            warn!(
                "🔄 Automatic resampling will be applied: {} Hz → {} Hz",
                sample_rate, TARGET_SAMPLE_RATE
            );

            // Log which resampling strategy will be used
            let ratio = TARGET_SAMPLE_RATE as f64 / sample_rate as f64;
            let strategy = if ratio >= 2.0 {
                "High-quality upsampling (sinc_len=512, Cubic interpolation)"
            } else if ratio >= 1.5 {
                "Moderate upsampling (sinc_len=384, Cubic)"
            } else if ratio > 1.0 {
                "Small upsampling (sinc_len=256, Linear)"
            } else if ratio <= 0.5 {
                "Anti-aliased downsampling (sinc_len=512, Cubic)"
            } else {
                "Moderate downsampling (sinc_len=384, Linear)"
            };
            info!("   Resampling strategy: {}", strategy);
        } else {
            info!(
                "✅ [{:?}] Audio device '{}' ({:?}) uses {} Hz (matches pipeline)",
                device_type, device.name, device_kind, sample_rate
            );
        }

        // Initialize audio enhancement processors for MICROPHONE ONLY
        // System audio doesn't need enhancement (already clean)
        let (noise_suppressor, high_pass_filter, normalizer) = if matches!(
            device_type,
            DeviceType::Microphone
        ) {
            // Initialize noise suppression (RNNoise) at 48kHz - CONDITIONAL based on flag
            let ns = if super::ffmpeg_mixer::RNNOISE_APPLY_ENABLED {
                match NoiseSuppressionProcessor::new(TARGET_SAMPLE_RATE) {
                    Ok(processor) => {
                        info!("✅ RNNoise noise suppression ENABLED for microphone '{}' (10-15 dB reduction)", device.name);
                        Some(processor)
                    }
                    Err(e) => {
                        warn!("⚠️ Failed to create noise suppressor: {}, continuing without noise suppression", e);
                        None
                    }
                }
            } else {
                info!("ℹ️ RNNoise noise suppression DISABLED for microphone '{}' (flag: RNNOISE_APPLY_ENABLED=false)", device.name);
                info!("   Whisper handles noise well internally - RNNoise is optional");
                None
            };

            // Initialize high-pass filter (removes rumble below 80 Hz)
            let hpf = {
                let filter = HighPassFilter::new(TARGET_SAMPLE_RATE, 80.0);
                info!(
                    "✅ High-pass filter initialized for microphone '{}' (cutoff: 80 Hz)",
                    device.name
                );
                Some(filter)
            };

            // Initialize EBU R128 normalizer (professional loudness standard)
            let norm = match LoudnessNormalizer::new(1, TARGET_SAMPLE_RATE) {
                Ok(normalizer) => {
                    info!(
                        "✅ EBU R128 normalizer initialized for microphone '{}' (target: -23 LUFS)",
                        device.name
                    );
                    Some(normalizer)
                }
                Err(e) => {
                    warn!(
                        "⚠️ Failed to create normalizer for microphone: {}, normalization disabled",
                        e
                    );
                    None
                }
            };

            (ns, hpf, norm)
        } else {
            // System audio: no enhancement needed
            info!(
                "ℹ️ System audio '{}' captured raw (no enhancement)",
                device.name
            );
            (None, None, None)
        };

        // CRITICAL FIX: Initialize persistent resampler to preserve energy across chunks
        // Creating a new resampler per chunk causes energy amplification and incorrect output sizes
        // Use fixed chunk size of 512 samples with buffering for variable-size input
        const RESAMPLER_CHUNK_SIZE: usize = 512;

        let resampler = if needs_resampling {
            let ratio = TARGET_SAMPLE_RATE as f64 / sample_rate as f64;

            // Adaptive parameters based on sample rate ratio (same logic as resample_audio)
            let (sinc_len, interpolation_type, oversampling) = if ratio >= 2.0 {
                (512, SincInterpolationType::Cubic, 512)
            } else if ratio >= 1.5 {
                (384, SincInterpolationType::Cubic, 384)
            } else if ratio > 1.0 {
                (256, SincInterpolationType::Linear, 256)
            } else if ratio <= 0.5 {
                (512, SincInterpolationType::Cubic, 512)
            } else {
                (384, SincInterpolationType::Linear, 384)
            };

            let params = SincInterpolationParameters {
                sinc_len,
                f_cutoff: 0.95,
                interpolation: interpolation_type,
                oversampling_factor: oversampling,
                window: WindowFunction::BlackmanHarris2,
            };

            match SincFixedIn::<f32>::new(
                ratio,
                2.0, // Maximum relative deviation
                params,
                RESAMPLER_CHUNK_SIZE,
                1, // Mono
            ) {
                Ok(resampler) => {
                    info!(
                        "✅ Persistent resampler initialized for '{}' ({}Hz → {}Hz, chunk_size={})",
                        device.name, sample_rate, TARGET_SAMPLE_RATE, RESAMPLER_CHUNK_SIZE
                    );
                    info!("   Buffering enabled for variable-size chunks (e.g., 320, 512, 1024, etc.)");
                    Some(CaptureResampler::new(
                        resampler,
                        sample_rate,
                        TARGET_SAMPLE_RATE,
                    ))
                }
                Err(e) => {
                    warn!(
                        "⚠️ Failed to create persistent resampler: {}, will use fallback",
                        e
                    );
                    None
                }
            }
        } else {
            None
        };

        Self {
            #[cfg(target_os = "linux")]
            fallback_clock: Arc::new(std::sync::Mutex::new((None, 0))),
            device,
            state,
            sample_rate,
            channels,
            chunk_counter: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            device_type,
            recording_sender,
            needs_resampling,
            resampler: Arc::new(std::sync::Mutex::new(resampler)),
            processing: Arc::new(std::sync::Mutex::new(CaptureProcessing {
                finished: false,
                pending_samples: Vec::with_capacity(1024),
            })),
            noise_suppressor: Arc::new(std::sync::Mutex::new(noise_suppressor)),
            high_pass_filter: Arc::new(std::sync::Mutex::new(high_pass_filter)),
            normalizer: Arc::new(std::sync::Mutex::new(normalizer)),
            // Using global recording time for sync
        }
    }

    /// Process audio data directly from callback
    pub fn process_audio_data(&self, data: &[f32]) {
        self.process_audio_data_with_timestamp(data, None);
    }

    /// Process samples at the capture position the native stream reports.
    #[cfg(target_os = "linux")]
    pub(super) fn process_audio_data_at(&self, data: &[f32], timestamp: f64) {
        self.process_audio_data_with_timestamp(data, Some(timestamp));
    }

    fn process_audio_data_with_timestamp(&self, data: &[f32], timestamp: Option<f64>) {
        let processing = lock_unpoisoned(&self.processing);
        if processing.finished || !self.state.is_active() {
            return;
        }
        self.process_input(data, timestamp);
    }

    /// The native callback admits these samples before it puts them in its queue.
    #[cfg(target_os = "macos")]
    pub(super) fn process_captured_samples(&self, samples: &[f32]) {
        let mut processing = lock_unpoisoned(&self.processing);
        if processing.finished {
            return;
        }
        for chunk in samples.chunks(1024) {
            processing.pending_samples.extend_from_slice(chunk);
            if processing.pending_samples.len() >= 1024 {
                self.process_input(&processing.pending_samples, None);
                processing.pending_samples.clear();
            }
        }
    }

    /// Send all accepted samples, including filter and noise suppression tails.
    pub fn finish(&self) -> Result<()> {
        let mut processing = lock_unpoisoned(&self.processing);
        if processing.finished {
            return Ok(());
        }
        processing.finished = true;
        let samples = std::mem::take(&mut processing.pending_samples);
        self.process_input(&samples, None);
        // A resampler failure must not skip the noise suppression tail.
        let mut result = Ok(());
        if let Some(resampler) = lock_unpoisoned(&self.resampler).as_mut() {
            match resampler.finish() {
                Ok(tail) => self.process_output(tail, None),
                Err(e) => result = Err(e),
            }
        }
        let noise_tail = lock_unpoisoned(&self.noise_suppressor)
            .as_mut()
            .map(|suppressor| suppressor.flush())
            .unwrap_or_default();
        self.normalize_and_send(noise_tail, None);
        result
    }

    fn process_input(&self, data: &[f32], timestamp: Option<f64>) {
        if data.is_empty() {
            return;
        }
        // Convert to mono if needed
        let mut mono_data = if self.channels > 1 {
            audio_to_mono(data, self.channels)
        } else {
            data.to_vec()
        };

        if self.needs_resampling {
            let mut resampler = lock_unpoisoned(&self.resampler);
            mono_data = if let Some(resampler) = resampler.as_mut() {
                match resampler.process(&mono_data) {
                    Ok(output) => output,
                    Err(e) => {
                        warn!("Capture resampling failed: {}", e);
                        self.state.report_error(AudioError::ProcessingFailed);
                        return;
                    }
                }
            } else {
                super::audio_processing::resample_audio(&mono_data, self.sample_rate, 48000)
            };
        }
        self.process_output(mono_data, timestamp);
    }

    fn process_output(&self, mut mono_data: Vec<f32>, timestamp: Option<f64>) {
        if mono_data.is_empty() {
            return;
        }
        // AUDIO ENHANCEMENT PIPELINE (Microphone Only)
        // Processing order is critical: high-pass → noise suppression → normalization
        // This ensures noise is removed before being amplified by the normalizer
        if matches!(self.device_type, DeviceType::Microphone) {
            // STEP 1: Apply high-pass filter to remove low-frequency rumble (< 80 Hz)
            if let Ok(mut hpf_lock) = self.high_pass_filter.lock() {
                if let Some(ref mut filter) = *hpf_lock {
                    mono_data = filter.process(&mono_data);
                }
            }

            // STEP 2: Apply RNNoise noise suppression (10-15 dB reduction) - CONDITIONAL
            if let Ok(mut ns_lock) = self.noise_suppressor.lock() {
                if let Some(ref mut suppressor) = *ns_lock {
                    let before_len = mono_data.len();
                    mono_data = suppressor.process(&mono_data);
                    let after_len = mono_data.len();

                    // CRITICAL MONITORING: Track buffer health
                    let chunk_id = self.chunk_counter.load(std::sync::atomic::Ordering::SeqCst);
                    if chunk_id % 100 == 0 {
                        let buffered = suppressor.buffered_samples();
                        let length_delta = (before_len as i32 - after_len as i32).abs();

                        debug!("🔇 Noise suppression health: in={}, out={}, delta={}, buffered={}, RMS={:.4}",
                                   before_len, after_len, length_delta, buffered,
                                   rms(&mono_data));

                        // WARN if accumulating samples (potential latency buildup)
                        if buffered > 1000 {
                            warn!("⚠️ RNNoise accumulating samples: {} buffered (potential latency issue!)",
                                      buffered);
                        }

                        // WARN if significant length mismatch
                        if length_delta > 50 {
                            warn!(
                                "⚠️ RNNoise length mismatch: input={} output={} (delta={})",
                                before_len, after_len, length_delta
                            );
                        }
                    }
                }
            }
        }
        self.normalize_and_send(mono_data, timestamp);
    }

    fn normalize_and_send(&self, mut mono_data: Vec<f32>, timestamp: Option<f64>) {
        if matches!(self.device_type, DeviceType::Microphone) {
            // STEP 3: Apply EBU R128 normalization (professional loudness standard)
            if let Ok(mut normalizer_lock) = self.normalizer.lock() {
                if let Some(ref mut normalizer) = *normalizer_lock {
                    mono_data = normalizer.normalize_loudness(&mono_data);

                    // Log normalization occasionally for debugging
                    let chunk_id = self.chunk_counter.load(std::sync::atomic::Ordering::SeqCst);
                    if chunk_id % 200 == 0 && !mono_data.is_empty() {
                        let rms = rms(&mono_data);
                        let peak = mono_data.iter().map(|&x| x.abs()).fold(0.0f32, f32::max);
                        debug!(
                            "🎤 After normalization chunk {}: RMS={:.4}, Peak={:.4}",
                            chunk_id, rms, peak
                        );
                    }
                }
            }
        }

        self.send_output(mono_data, timestamp);
    }

    fn send_output(&self, mono_data: Vec<f32>, timestamp: Option<f64>) {
        if mono_data.is_empty() {
            return;
        }
        // Create audio chunk with stream-specific timestamp (get ID first for logging)
        let chunk_id = self
            .chunk_counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        // RAW AUDIO: No gain applied here - will be applied AFTER mixing
        // This prevents amplifying system audio bleed-through in the microphone

        // DIAGNOSTIC: Log audio levels for debugging (especially mic issues)
        // if chunk_id % 100 == 0 && !mono_data.is_empty() {
        //     let raw_rms = (mono_data.iter().map(|&x| x * x).sum::<f32>() / mono_data.len() as f32).sqrt();
        //     let raw_peak = mono_data.iter().map(|&x| x.abs()).fold(0.0f32, f32::max);

        //         info!("🎙️ [{:?}] Chunk {} - Raw: RMS={:.6}, Peak={:.6}",
        //               self.device_type, chunk_id, raw_rms, raw_peak);

        //     // Warn if microphone is completely silent
        //     if matches!(self.device_type, DeviceType::Microphone) && raw_rms == 0.0 && raw_peak == 0.0 {
        //         warn!("⚠️ Microphone producing ZERO audio - check permissions or hardware!");
        //     }
        // }
        // else if chunk_id % 100 == 0 && matches!(self.device_type, DeviceType::System) {
        //     let raw_rms = (mono_data.iter().map(|&x| x * x).sum::<f32>() / mono_data.len() as f32).sqrt();
        //     let raw_peak = mono_data.iter().map(|&x| x.abs()).fold(0.0f32, f32::max);
        //     info!("🔊 [{:?}] Chunk {} - Raw: RMS={:.6}, Peak={:.6}",
        //       self.device_type, chunk_id, raw_rms, raw_peak);

        //     // Warn if system audio is completely silent
        //     if raw_rms == 0.0 && raw_peak == 0.0 {
        //         warn!("⚠️ System audio producing ZERO audio - check permissions or hardware!");
        //     }
        // }

        // Use global recording timestamp for proper synchronization
        #[cfg(not(target_os = "linux"))]
        let timestamp = timestamp.unwrap_or_else(|| self.state.get_recording_duration().unwrap_or(0.0));
        #[cfg(target_os = "linux")]
        let timestamp = timestamp.unwrap_or_else(|| {
            // ALSA callback jitter must not become gaps in its single-role recording.
            let mut clock = self.fallback_clock.lock().unwrap();
            let origin = *clock.0.get_or_insert_with(||
                (self.state.get_active_recording_duration().unwrap_or(0.0) - mono_data.len() as f64 / 48000.0).max(0.0));
            let timestamp = origin + clock.1 as f64 / 48000.0;
            clock.1 += mono_data.len() as u64;
            timestamp
        });

        // RAW AUDIO CHUNK: No gain applied - will be mixed and gained downstream
        // Use 48kHz if we resampled, otherwise use original rate
        let audio_chunk = AudioChunk {
            data: mono_data, // Raw audio (resampled if needed), no gain yet
            sample_rate: if self.needs_resampling {
                48000
            } else {
                self.sample_rate
            },
            timestamp,
            end_timestamp: None,
            chunk_id,
            device_type: self.device_type.clone(),
            source_activity: None,
        };

        // NOTE: Raw audio is NOT sent to recording saver to prevent echo
        // Only the mixed audio (from AudioPipeline) is saved to file (see pipeline.rs:726-736)
        // This ensures we only record once: mic + system properly mixed
        // Individual raw streams go only to the transcription pipeline below

        // The state keeps its sender open after recording stops, so finish() can drain.
        if let Err(e) = self.state.send_captured_chunk(audio_chunk) {
            // Expected during initialization and after the pipeline closes
            if e.to_string().contains("Audio pipeline not ready") {
                debug!("Audio pipeline not ready yet, skipping chunk {}", chunk_id);
                return;
            }
            warn!("Failed to send audio chunk: {}", e);
            self.state.report_error(AudioError::ChannelClosed);
        }
    }

    /// Handle stream errors with enhanced disconnect detection
    pub fn handle_stream_error(&self, error: cpal::StreamError) {
        error!("Audio stream error for {}: {}", self.device.name, error);

        let error_str = error.to_string().to_lowercase();

        // Enhanced error detection for device disconnection
        let audio_error = if error_str.contains("device is no longer available")
            || error_str.contains("device not found")
            || error_str.contains("device disconnected")
            || error_str.contains("no such device")
            || error_str.contains("device unavailable")
            || error_str.contains("device removed")
        {
            warn!("🔌 Device disconnect detected for: {}", self.device.name);
            AudioError::DeviceDisconnected
        } else if error_str.contains("permission") || error_str.contains("access denied") {
            AudioError::PermissionDenied
        } else if error_str.contains("channel closed") {
            AudioError::ChannelClosed
        } else if error_str.contains("stream") && error_str.contains("failed") {
            AudioError::StreamFailed
        } else {
            warn!("Unknown audio error: {}", error);
            AudioError::StreamFailed
        };

        self.state.report_error(audio_error);
    }
}

#[derive(Clone, Copy)]
enum VadOperation {
    Process,
    Flush,
}

#[derive(Default)]
struct PipelineLogStats {
    emitted: u64,
    short: u64,
    vad_process_invalid_data_failures: u64,
    vad_process_inference_failures: u64,
    vad_flush_invalid_data_failures: u64,
    vad_flush_inference_failures: u64,
    send_failures: u64,
    duration_sum_ms: f64,
    duration_min_ms: Option<f64>,
    duration_max_ms: f64,
}

impl PipelineLogStats {
    fn record_segment(&mut self, duration_ms: f64) {
        self.emitted += 1;
        self.duration_sum_ms += duration_ms;
        self.duration_min_ms = Some(self.duration_min_ms.map_or(duration_ms, |min| min.min(duration_ms)));
        self.duration_max_ms = self.duration_max_ms.max(duration_ms);
    }

    fn record_vad_failure(&mut self, operation: VadOperation, error: &anyhow::Error) {
        let invalid_data = error.chain().any(|cause| {
            matches!(
                cause.downcast_ref::<silero_rs::VadError>(),
                Some(silero_rs::VadError::InvalidData)
            )
        });
        match (operation, invalid_data) {
            (VadOperation::Process, true) => self.vad_process_invalid_data_failures += 1,
            (VadOperation::Process, false) => self.vad_process_inference_failures += 1,
            (VadOperation::Flush, true) => self.vad_flush_invalid_data_failures += 1,
            (VadOperation::Flush, false) => self.vad_flush_inference_failures += 1,
        }
    }

    fn snapshot_and_reset(&mut self) -> Self {
        std::mem::take(self)
    }
}

/// VAD-driven audio processing pipeline
/// Uses Voice Activity Detection to segment speech in real-time and send only speech to Whisper
pub struct AudioPipeline {
    receiver: mpsc::UnboundedReceiver<AudioChunk>,
    transcription_sender: mpsc::UnboundedSender<AudioChunk>,
    state: Arc<RecordingState>,
    vad_processor: ContinuousVadProcessor,
    /// Silences each source outside its own speech before the transcription mix.
    source_gate: SourceGatedInput,
    sample_rate: u32,
    chunk_id_counter: u64,
    last_summary_time: std::time::Instant,
    log_stats: PipelineLogStats,
    // PROFESSIONAL AUDIO MIXING: Ring buffer + RMS-based mixer
    ring_buffer: AudioMixerRingBuffer,
    mixer: ProfessionalAudioMixer,
    // Recording sender for pre-mixed audio
    recording_sender_for_mixed: Option<mpsc::UnboundedSender<AudioChunk>>,
    stem_sender: Option<mpsc::UnboundedSender<StemWindow>>,
    source_activity: VecDeque<SourceActivityWindow>,
    source_timeline_seconds: f64,
    system_available: bool,
}

impl AudioPipeline {
    pub fn new(
        receiver: mpsc::UnboundedReceiver<AudioChunk>,
        transcription_sender: mpsc::UnboundedSender<AudioChunk>,
        state: Arc<RecordingState>,
        target_chunk_duration_ms: u32,
        sample_rate: u32,
        mic_device_name: String,
        mic_device_kind: super::device_detection::InputDeviceKind,
        system_device_name: String,
        system_device_kind: super::device_detection::InputDeviceKind,
    ) -> Result<Self> {
        // Log device characteristics for adaptive buffering
        info!("🎛️ AudioPipeline initializing with device characteristics:");
        info!(
            "   Mic: '{}' ({:?}) - Buffer: {:?}",
            mic_device_name,
            mic_device_kind,
            mic_device_kind.buffer_timeout()
        );
        info!(
            "   System: '{}' ({:?}) - Buffer: {:?}",
            system_device_name,
            system_device_kind,
            system_device_kind.buffer_timeout()
        );

        // Device kind information can be used for adaptive buffering in the future
        // For now, we log it for monitoring and potential optimization
        let system_available = system_device_name != "No System Audio";
        let _ = (
            mic_device_name,
            mic_device_kind,
            system_device_name,
            system_device_kind,
        );

        // Create VAD processor. The VAD processor handles 48kHz->16kHz resampling
        // internally.
        //
        // Redemption time is how long a silence must last before the VAD closes a
        // speech segment, so it decides how long the audio clips handed to the ASR
        // engine are. Conversational speech pauses constantly for breath and
        // mid-sentence thought, and every pause longer than this becomes a segment
        // boundary and therefore a separate transcription request.
        //
        // This was 400ms, which fragmented a 26-minute meeting into 322 requests with
        // a median length of 3.5s. Whisper is a fixed 30-second-window model: below
        // that it zero-pads the window and leans on its language-model prior, which
        // was trained on web subtitles, so short clips come back as memorised
        // boilerplate ("subscribe to the channel", "thank you") instead of speech.
        // Measured on a real recording, 47% of segment boundaries sat in the
        // 0.42-0.75s range that a longer redemption bridges.
        //
        // 500ms is the live-path policy (see the constant's doc comment). The
        // batch value (2000ms, `import.rs`/`retranscription.rs`) was tried here
        // first, but under continuous system audio it kept a VAD segment open
        // indefinitely and withheld live transcript emission, so live and batch
        // deliberately diverge. Bounded live segments under continuous speech
        // are tracked in #756.
        let vad_processor = ContinuousVadProcessor::new(sample_rate, VAD_REDEMPTION_TIME_MS)?;
        info!(
            "VAD-driven pipeline: segments dispatched per speech burst (redemption_time={}ms)",
            VAD_REDEMPTION_TIME_MS
        );

        let source_gate = SourceGatedInput::new(sample_rate, VAD_REDEMPTION_TIME_MS)?;

        // Initialize professional audio mixing components
        let ring_buffer = AudioMixerRingBuffer::new(sample_rate);
        let mixer = ProfessionalAudioMixer::new(sample_rate);

        // Note: target_chunk_duration_ms is ignored - VAD controls segmentation now
        let _ = target_chunk_duration_ms;

        Ok(Self {
            receiver,
            transcription_sender,
            state,
            vad_processor,
            source_gate,
            sample_rate,
            chunk_id_counter: 0,
            last_summary_time: std::time::Instant::now(),
            log_stats: PipelineLogStats::default(),
            // Initialize professional audio mixing
            ring_buffer,
            mixer,
            recording_sender_for_mixed: None, // Will be set by manager
            stem_sender: None,
            source_activity: VecDeque::new(),
            source_timeline_seconds: 0.0,
            system_available,
        })
    }

    /// Run the VAD-driven audio processing pipeline
    pub async fn run(self) -> Result<()> {
        self.run_with_flush(Self::flush_remaining_audio).await
    }

    /// Flush only after all queued audio has been processed.
    async fn run_with_flush(
        mut self,
        mut flush: impl FnMut(&mut Self) -> Result<()> + Send,
    ) -> Result<()> {
        info!("VAD-driven audio pipeline started - segments sent in real-time based on speech detection");

        // CRITICAL FIX: Continue processing until channel is closed, not based on recording state
        // This ensures ALL chunks are processed during shutdown, fixing premature meeting completion
        // Previous bug: Loop checked `while self.state.is_recording()` which caused early exit when
        // stop_recording() was called, losing remaining chunks in the pipeline
        loop {
            let wait = std::time::Duration::from_millis(50);
            // Receive audio chunks with timeout
            match tokio::time::timeout(wait, self.receiver.recv()).await {
                Ok(Some(chunk)) => {
                    if self.last_summary_time.elapsed() >= std::time::Duration::from_secs(60) {
                        self.emit_log_summary(false);
                    }

                    // STEP 1: Add raw audio to ring buffer for mixing
                    // Microphone audio is already normalized at capture level (AudioCapture)
                    // System audio remains raw
                    #[cfg(target_os = "linux")]
                    self.ring_buffer.add_samples_at(
                        chunk.device_type,
                        chunk.data,
                        (chunk.timestamp * self.sample_rate as f64).round().max(0.0) as u64,
                    );
                    #[cfg(not(target_os = "linux"))]
                    self.ring_buffer.add_samples(chunk.device_type, chunk.data);

                    // STEP 2: Mix audio in fixed windows when both streams have sufficient data
                    self.mix_ready(false, chunk.timestamp);
                }
                Ok(None) => break,
                Err(_) => continue,
            }
        }

        // Flush the final mixer window and return any final transcript error.
        let flushed = flush(&mut self);
        self.emit_log_summary(true);
        flushed?;

        info!("VAD-driven audio pipeline ended");
        Ok(())
    }

    fn mix_ready(&mut self, flush: bool, timestamp: f64) {
        loop {
            #[cfg(target_os = "linux")]
            let window = self.ring_buffer.extract_aligned(flush);
            #[cfg(not(target_os = "linux"))]
            let window = {
                let _ = flush;
                self.ring_buffer.extract_window()
            };
            let Some((mic, system)) = window else { break; };
            #[cfg(target_os = "linux")]
            let timestamp = {
                let _ = timestamp;
                (self.ring_buffer.position.unwrap() - mic.len() as u64) as f64
                    / self.sample_rate as f64
            };
            self.process_window(mic, system, timestamp);
        }
    }

    fn process_window(&mut self, mic_window: Vec<f32>, sys_window: Vec<f32>, timestamp: f64) {
        if let Some(sender) = &self.stem_sender {
            let _ = sender.send(StemWindow {
                microphone: mic_window.clone(),
                system: sys_window.clone(),
            });
        }
        self.record_source_activity(&mic_window, &sys_window);
        // Simple mixing without aggressive ducking
        let mixed_clean = self.mixer.mix_window(&mic_window, &sys_window);

        // NO POST-GAIN NEEDED: Microphone already normalized by EBU R128 to -23 LUFS
        // This is broadcast-standard loudness (Netflix/YouTube/Spotify level)
        // System audio at natural levels
        // Previous 2x gain was causing excessive limiting/distortion
        let mixed_with_gain = mixed_clean;

        // STEP 3: Transcribe each source only inside its own speech. Steady
        // noise on one source must not hide speech on the other (see source_gate).
        let (gated_mic, gated_sys) = self.source_gate.push(&mic_window, &sys_window);
        self.transcribe_gated(&gated_mic, &gated_sys);

        // STEP 4: Send mixed audio for recording (WAV file)
        if let Some(ref sender) = self.recording_sender_for_mixed {
            let recording_chunk = AudioChunk {
                data: mixed_with_gain.clone(),
                sample_rate: self.sample_rate,
                timestamp,
                end_timestamp: None,
                chunk_id: self.chunk_id_counter,
                device_type: DeviceType::Microphone, // Mixed audio
                source_activity: None,
            };
            let _ = sender.send(recording_chunk);
        }
    }

    /// Mix gated source audio and send its speech segments for transcription.
    fn transcribe_gated(&mut self, microphone: &[f32], system: &[f32]) {
        if microphone.is_empty() && system.is_empty() {
            return;
        }
        let mixed = self.mixer.mix_window(microphone, system);
        match self.vad_processor.process_audio(&mixed) {
            Ok(speech_segments) => {
                for segment in speech_segments {
                    let duration_ms = segment.end_timestamp_ms - segment.start_timestamp_ms;
                    // Minimum 50ms at 16kHz - matches Parakeet capability
                    if segment.samples.len() >= 800 {
                        self.log_stats.record_segment(duration_ms);
                        let transcription_chunk = AudioChunk {
                            data: segment.samples,
                            sample_rate: 16000,
                            timestamp: segment.start_timestamp_ms / 1000.0,
                            end_timestamp: Some(segment.end_timestamp_ms / 1000.0),
                            chunk_id: self.chunk_id_counter,
                            device_type: DeviceType::Microphone, // Mixed audio
                            source_activity: Some(self.source_activity_for_interval(
                                segment.start_timestamp_ms / 1000.0,
                                segment.end_timestamp_ms / 1000.0,
                            )),
                        };
                        if self.transcription_sender.send(transcription_chunk).is_err() {
                            self.log_stats.send_failures += 1;
                        } else {
                            self.chunk_id_counter += 1;
                        }
                    } else {
                        self.log_stats.short += 1;
                    }
                }
            }
            Err(error) => self.log_stats.record_vad_failure(VadOperation::Process, &error),
        }
    }

    fn flush_remaining_audio(&mut self) -> Result<()> {
        info!("Flushing remaining audio from pipeline");

        #[cfg(target_os = "linux")]
        self.mix_ready(true, self.source_timeline_seconds);
        #[cfg(not(target_os = "linux"))]
        if let Some((mic_window, sys_window)) = self.ring_buffer.extract_remaining() {
            self.process_window(mic_window, sys_window, self.source_timeline_seconds);
        }
        // Release the audio still waiting for its source speech decision.
        let (gated_mic, gated_sys) = self.source_gate.finish();
        self.transcribe_gated(&gated_mic, &gated_sys);

        let final_segments = self.vad_processor.flush();
        if let Err(error) = &final_segments {
            self.log_stats.record_vad_failure(VadOperation::Flush, error);
        }
        forward_final_segments(final_segments, |segment| {
            self.log_stats
                .record_segment(segment.end_timestamp_ms - segment.start_timestamp_ms);
            let transcription_chunk = AudioChunk {
                data: segment.samples,
                sample_rate: 16000,
                timestamp: segment.start_timestamp_ms / 1000.0,
                end_timestamp: Some(segment.end_timestamp_ms / 1000.0),
                chunk_id: self.chunk_id_counter,
                device_type: DeviceType::Microphone,
                source_activity: Some(self.source_activity_for_interval(
                    segment.start_timestamp_ms / 1000.0,
                    segment.end_timestamp_ms / 1000.0,
                )),
            };
            self.transcription_sender
                .send(transcription_chunk)
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            self.chunk_id_counter += 1;
            Ok(())
        })
    }

    fn record_source_activity(&mut self, microphone: &[f32], system: &[f32]) {
        let duration = microphone.len().max(system.len()) as f64 / self.sample_rate as f64;
        let start = self.source_timeline_seconds;
        let end = start + duration;
        self.source_timeline_seconds = end;
        self.source_activity.push_back(SourceActivityWindow {
            start,
            end,
            microphone_active: rms(microphone) >= 0.003,
            system_active: rms(system) >= 0.003,
        });
        while self
            .source_activity
            .front()
            .is_some_and(|window| window.end < end - 120.0)
        {
            self.source_activity.pop_front();
        }
    }

    fn source_activity_for_interval(&self, start: f64, end: f64) -> SourceActivity {
        let mut microphone_active = false;
        let mut system_active = false;
        let mut overlap_detected = false;
        for window in &self.source_activity {
            if window.end > start && window.start < end {
                microphone_active |= window.microphone_active;
                system_active |= window.system_active;
                overlap_detected |= window.microphone_active && window.system_active;
            }
        }
        SourceActivity {
            microphone_active,
            system_active,
            system_available: self.system_available,
            overlap_detected,
        }
    }

    fn emit_log_summary(&mut self, final_summary: bool) {
        let stats = self.log_stats.snapshot_and_reset();
        let (mic_dropped, system_dropped) = self.ring_buffer.take_dropped_samples();
        let mean_duration_ms = if stats.emitted == 0 {
            0.0
        } else {
            stats.duration_sum_ms / stats.emitted as f64
        };
        if final_summary {
            info!("Audio pipeline summary; emitted={}, short={}, vad_process_invalid_data_failures={}, vad_process_inference_failures={}, vad_flush_invalid_data_failures={}, vad_flush_inference_failures={}, send_failures={}, mic_dropped_samples={}, system_dropped_samples={}, segment_min_ms={:.1}, segment_mean_ms={:.1}, segment_max_ms={:.1}", stats.emitted, stats.short, stats.vad_process_invalid_data_failures, stats.vad_process_inference_failures, stats.vad_flush_invalid_data_failures, stats.vad_flush_inference_failures, stats.send_failures, mic_dropped, system_dropped, stats.duration_min_ms.unwrap_or(0.0), mean_duration_ms, stats.duration_max_ms);
        } else if stats.vad_process_invalid_data_failures > 0 || stats.vad_process_inference_failures > 0 || stats.vad_flush_invalid_data_failures > 0 || stats.vad_flush_inference_failures > 0 || stats.send_failures > 0 || mic_dropped > 0 || system_dropped > 0 {
            warn!("Audio pipeline summary; emitted={}, short={}, vad_process_invalid_data_failures={}, vad_process_inference_failures={}, vad_flush_invalid_data_failures={}, vad_flush_inference_failures={}, send_failures={}, mic_dropped_samples={}, system_dropped_samples={}", stats.emitted, stats.short, stats.vad_process_invalid_data_failures, stats.vad_process_inference_failures, stats.vad_flush_invalid_data_failures, stats.vad_flush_inference_failures, stats.send_failures, mic_dropped, system_dropped);
        } else {
            debug!("Audio pipeline summary; emitted={}, short={}, segment_min_ms={:.1}, segment_mean_ms={:.1}, segment_max_ms={:.1}", stats.emitted, stats.short, stats.duration_min_ms.unwrap_or(0.0), mean_duration_ms, stats.duration_max_ms);
        }
        self.last_summary_time = std::time::Instant::now();
    }
}

/// Separate an early pipeline exit from a failed final transcript flush.
#[derive(Debug, thiserror::Error)]
pub(crate) enum PipelineStopError {
    /// The pipeline also feeds the recording, so saved audio may be incomplete.
    #[error("{0:#}")]
    Pipeline(anyhow::Error),
    /// Only the final transcript segments may be missing.
    #[error("{0:#}")]
    FinalTranscript(anyhow::Error),
}

/// Simple audio pipeline manager
pub struct AudioPipelineManager {
    pipeline_handle: Option<JoinHandle<Result<()>>>,
    audio_sender: Option<mpsc::UnboundedSender<AudioChunk>>,
    state: Option<Arc<RecordingState>>,
}

impl AudioPipelineManager {
    pub fn new() -> Self {
        Self {
            pipeline_handle: None,
            audio_sender: None,
            state: None,
        }
    }

    #[cfg(test)]
    pub(super) fn with_task_for_test(handle: JoinHandle<Result<()>>) -> Self {
        Self {
            pipeline_handle: Some(handle),
            audio_sender: None,
            state: None,
        }
    }

    /// Start the audio pipeline with device information for adaptive buffering
    pub fn start(
        &mut self,
        state: Arc<RecordingState>,
        transcription_sender: mpsc::UnboundedSender<AudioChunk>,
        target_chunk_duration_ms: u32,
        sample_rate: u32,
        recording_sender: Option<mpsc::UnboundedSender<AudioChunk>>,
        stem_sender: Option<mpsc::UnboundedSender<StemWindow>>,
        mic_device_name: String,
        mic_device_kind: super::device_detection::InputDeviceKind,
        system_device_name: String,
        system_device_kind: super::device_detection::InputDeviceKind,
    ) -> Result<()> {
        // Log device information for adaptive buffering
        info!("🎙️ Starting pipeline with device info:");
        info!(
            "   Microphone: '{}' ({:?})",
            mic_device_name, mic_device_kind
        );
        info!(
            "   System Audio: '{}' ({:?})",
            system_device_name, system_device_kind
        );

        // Create audio processing channel
        let (audio_sender, audio_receiver) = mpsc::unbounded_channel::<AudioChunk>();

        // Create and start pipeline with device information for adaptive mixing
        let mut pipeline = AudioPipeline::new(
            audio_receiver,
            transcription_sender,
            state.clone(),
            target_chunk_duration_ms,
            sample_rate,
            mic_device_name,
            mic_device_kind,
            system_device_name,
            system_device_kind,
        )?;
        state.set_audio_sender(audio_sender.clone());

        // CRITICAL FIX: Connect recording sender to receive pre-mixed audio
        // This ensures both mic AND system audio are captured in recordings
        pipeline.recording_sender_for_mixed = recording_sender;
        pipeline.stem_sender = stem_sender;

        let handle = tokio::spawn(async move { pipeline.run().await });

        self.pipeline_handle = Some(handle);
        self.audio_sender = Some(audio_sender);
        self.state = Some(state);

        info!("Audio pipeline manager started with mixed audio recording");
        Ok(())
    }

    /// Stop the audio pipeline
    ///
    /// Stop the capture streams first. Their final audio goes through the state's sender,
    /// and this call closes it. A capture that is still stopping cannot hold the pipeline open.
    pub(crate) async fn stop(&mut self) -> std::result::Result<(), PipelineStopError> {
        // Drop the senders to close the pipeline
        self.audio_sender = None;
        if let Some(state) = self.state.take() {
            state.close_audio_sender();
        }

        // Wait for pipeline to finish
        if let Some(handle) = self.pipeline_handle.take() {
            match handle.await {
                Ok(result) => result.map_err(PipelineStopError::FinalTranscript),
                Err(e) => Err(PipelineStopError::Pipeline(anyhow::anyhow!(
                    "Audio pipeline task failed: {e}"
                ))),
            }
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PipelineLogStats, VadOperation};

    #[test]
    fn logging_pipeline_diagnostics_aggregate_and_reset() {
        let mut stats = PipelineLogStats::default();
        stats.record_segment(10.0);
        stats.record_segment(30.0);
        stats.short = 1;
        for operation in [VadOperation::Process, VadOperation::Flush] {
            stats.record_vad_failure(
                operation,
                &anyhow::Error::from(silero_rs::VadError::InvalidData),
            );
            stats.record_vad_failure(operation, &anyhow::anyhow!("opaque VAD failure"));
        }
        let snapshot = stats.snapshot_and_reset();
        assert_eq!(snapshot.emitted, 2);
        assert_eq!(snapshot.short, 1);
        assert_eq!(snapshot.vad_process_invalid_data_failures, 1);
        assert_eq!(snapshot.vad_process_inference_failures, 1);
        assert_eq!(snapshot.vad_flush_invalid_data_failures, 1);
        assert_eq!(snapshot.vad_flush_inference_failures, 1);
        assert_eq!(snapshot.duration_min_ms, Some(10.0));
        assert_eq!(snapshot.duration_max_ms, 30.0);
        assert_eq!(snapshot.duration_sum_ms / snapshot.emitted as f64, 20.0);
        let reset = stats.snapshot_and_reset();
        assert_eq!(reset.vad_process_invalid_data_failures, 0);
        assert_eq!(reset.vad_process_inference_failures, 0);
        assert_eq!(reset.vad_flush_invalid_data_failures, 0);
        assert_eq!(reset.vad_flush_inference_failures, 0);
    }
}

impl Default for AudioPipelineManager {
    fn default() -> Self {
        Self::new()
    }
}
#[cfg(test)]
mod policy_tests {
    use super::*;

    fn final_segment(samples: usize) -> SpeechSegment {
        SpeechSegment {
            samples: vec![0.1; samples],
            start_timestamp_ms: 0.0,
            end_timestamp_ms: samples as f64 / 16.0,
            confidence: 1.0,
        }
    }

    #[tokio::test]
    async fn final_flush_failure_preserves_queued_audio_and_the_partial_window() {
        use super::super::device_detection::InputDeviceKind;
        let (audio_sender, audio_receiver) = mpsc::unbounded_channel();
        let (transcription_sender, _transcription_receiver) = mpsc::unbounded_channel();
        let (recording_sender, mut recording_receiver) = mpsc::unbounded_channel();
        let mut pipeline = AudioPipeline::new(
            audio_receiver,
            transcription_sender,
            RecordingState::new(),
            1000,
            16000,
            "Test microphone".to_string(),
            InputDeviceKind::Wired,
            "No System Audio".to_string(),
            InputDeviceKind::Wired,
        )
        .unwrap();
        pipeline.recording_sender_for_mixed = Some(recording_sender);
        let chunk = |chunk_id, timestamp, data| AudioChunk {
            data,
            chunk_id,
            sample_rate: 16000,
            timestamp,
            end_timestamp: None,
            device_type: DeviceType::Microphone,
            source_activity: None,
        };
        // Keep both a complete mixer window and a partial window before final flush.
        audio_sender.send(chunk(0, 0.0, vec![0.05; 9600])).unwrap();
        audio_sender.send(chunk(1, 0.6, vec![0.05; 127])).unwrap();
        drop(audio_sender);
        let mut flush_calls = 0;
        let error = pipeline
            .run_with_flush(|pipeline| {
                flush_calls += 1;
                pipeline.flush_remaining_audio()?;
                Err(anyhow::anyhow!("Injected final flush failure"))
            })
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "Injected final flush failure");
        assert_eq!(flush_calls, 1);
        assert_eq!(recording_receiver.recv().await.unwrap().data.len(), 9600);
        assert_eq!(recording_receiver.recv().await.unwrap().data.len(), 127);
        assert!(recording_receiver.recv().await.is_none());
    }

    #[tokio::test]
    async fn final_flush_error_reaches_stop() {
        let mut manager = AudioPipelineManager::new();
        manager.pipeline_handle = Some(tokio::spawn(async {
            forward_final_segments(Err(anyhow::anyhow!("Injected VAD failure")), |_| {
                panic!("A failed flush must not send segments")
            })
        }));
        let error = manager.stop().await.unwrap_err();
        assert!(matches!(error, PipelineStopError::FinalTranscript(_)));
        let message = error.to_string();
        assert!(message.contains("Failed to flush final VAD audio"));
        assert!(message.contains("Injected VAD failure"));
        assert!(manager.pipeline_handle.is_none());
    }

    #[tokio::test]
    async fn closed_final_transcript_receiver_reaches_stop() {
        let mut manager = AudioPipelineManager::new();
        let (sender, receiver) = mpsc::unbounded_channel::<SpeechSegment>();
        drop(receiver);
        manager.pipeline_handle = Some(tokio::spawn(async move {
            forward_final_segments(Ok(vec![final_segment(800)]), |segment| {
                sender
                    .send(segment)
                    .map_err(|error| anyhow::anyhow!("{error}"))
            })
        }));
        let error = manager.stop().await.unwrap_err();
        assert!(matches!(error, PipelineStopError::FinalTranscript(_)));
        assert!(error.to_string().contains("Failed to send final VAD segment"));
        assert!(manager.pipeline_handle.is_none());
    }

    #[tokio::test]
    async fn final_segments_keep_the_minimum_length_filter() {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        forward_final_segments(
            Ok(vec![final_segment(799), final_segment(800)]),
            |segment| {
                sender
                    .send(segment)
                    .map_err(|error| anyhow::anyhow!("{error}"))
            },
        )
        .unwrap();
        assert_eq!(receiver.try_recv().unwrap().samples.len(), 800);
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn pipeline_task_failure_is_an_audio_failure() {
        let mut manager = AudioPipelineManager::new();
        manager.pipeline_handle = Some(tokio::spawn(async {
            panic!("Injected pipeline panic");
        }));
        let error = manager.stop().await.unwrap_err();
        assert!(matches!(error, PipelineStopError::Pipeline(_)));
        assert!(error.to_string().contains("Audio pipeline task failed"));
        assert!(manager.pipeline_handle.is_none());
    }

    #[tokio::test]
    async fn stop_joins_a_finished_pipeline_after_its_receiver_closes() {
        let mut manager = AudioPipelineManager::new();
        let (sender, receiver) = mpsc::unbounded_channel();
        drop(receiver);
        manager.audio_sender = Some(sender);
        manager.pipeline_handle = Some(tokio::spawn(async { Ok(()) }));
        manager.stop().await.unwrap();
        assert!(manager.pipeline_handle.is_none());
        assert!(manager.audio_sender.is_none());
    }

    #[test]
    fn test_live_vad_redemption_matches_pro_policy() {
        // Live uses the established 500ms pause policy; it does not bound
        // uninterrupted speech. Batch import/retranscription use 2000ms.
        // See #679 and #756.
        assert_eq!(VAD_REDEMPTION_TIME_MS, 500);
    }
    fn capture_for_test(
        rate: u32,
        device_type: DeviceType,
    ) -> (
        AudioCapture,
        Arc<RecordingState>,
        mpsc::UnboundedReceiver<AudioChunk>,
    ) {
        let state = RecordingState::new();
        state.start_recording().unwrap();
        let (sender, receiver) = mpsc::unbounded_channel();
        state.set_audio_sender(sender);
        let device = Arc::new(AudioDevice::new(
            "Test capture".into(),
            super::super::devices::DeviceType::Output,
        ));
        let capture = AudioCapture::new(device, state.clone(), rate, 1, device_type, None);
        (capture, state, receiver)
    }

    #[test]
    fn capture_drains_after_state_stops_and_finishes_once() {
        for rate in [16000, 44100, 48000, 96000] {
            let (capture, state, mut receiver) = capture_for_test(rate, DeviceType::System);
            let input = vec![0.25; 128];
            capture.process_audio_data(&input);
            state.stop_recording();
            capture.finish().unwrap();
            capture.finish().unwrap();
            capture.process_audio_data(&input);
            state.close_audio_sender();
            let mut output = Vec::new();
            while let Ok(chunk) = receiver.try_recv() {
                assert_eq!(chunk.sample_rate, 48000);
                assert_eq!(chunk.device_type, DeviceType::System);
                output.extend(chunk.data);
            }
            assert_eq!(output.len(), (128usize * 48000).div_ceil(rate as usize));
            assert!(output.iter().any(|sample| sample.abs() > 0.01));
            assert!(matches!(
                receiver.try_recv(),
                Err(mpsc::error::TryRecvError::Disconnected)
            ));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn core_audio_batch_keeps_samples_accepted_before_pause_and_stop() {
        let (capture, state, mut receiver) = capture_for_test(48000, DeviceType::System);
        capture.process_captured_samples(&[0.25; 127]);
        state.pause_recording().unwrap();
        capture.process_audio_data(&[0.75; 128]);
        state.stop_recording();
        capture.finish().unwrap();
        state.close_audio_sender();
        assert_eq!(receiver.try_recv().unwrap().data, vec![0.25; 127]);
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        ));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn final_mixer_window_has_only_real_duration() {
        for (mic_len, sys_len) in [(0, 0), (127, 0), (0, 511), (511, 127), (28799, 28799)] {
            let mut buffer = AudioMixerRingBuffer::new(48000);
            buffer.add_samples(DeviceType::Microphone, vec![0.25; mic_len]);
            buffer.add_samples(DeviceType::System, vec![0.5; sys_len]);
            assert!(!buffer.can_mix());
            if mic_len.max(sys_len) == 0 {
                assert!(buffer.extract_remaining().is_none());
                continue;
            }
            let (mic, sys) = buffer.extract_remaining().unwrap();
            assert_eq!(mic.len(), mic_len.max(sys_len));
            assert_eq!(sys.len(), mic.len());
            assert_eq!(&mic[..mic_len], vec![0.25; mic_len]);
            assert_eq!(&sys[..sys_len], vec![0.5; sys_len]);
            assert!(mic[mic_len..].iter().all(|v| *v == 0.0));
            assert!(sys[sys_len..].iter().all(|v| *v == 0.0));
            assert!(buffer.extract_remaining().is_none());
        }
    }

    fn pipeline_for_test(
        sample_rate: u32,
    ) -> (
        AudioPipeline,
        mpsc::UnboundedSender<AudioChunk>,
        mpsc::UnboundedReceiver<AudioChunk>,
        mpsc::UnboundedReceiver<StemWindow>,
        mpsc::UnboundedReceiver<AudioChunk>,
    ) {
        use super::super::device_detection::InputDeviceKind;
        let (sender, receiver) = mpsc::unbounded_channel();
        let (transcription_sender, transcription_receiver) = mpsc::unbounded_channel();
        let (recording_sender, recording_receiver) = mpsc::unbounded_channel();
        let (stem_sender, stem_receiver) = mpsc::unbounded_channel();
        let mut pipeline = AudioPipeline::new(
            receiver,
            transcription_sender,
            RecordingState::new(),
            600,
            sample_rate,
            "Test microphone".into(),
            InputDeviceKind::Wired,
            "Test system".into(),
            InputDeviceKind::Wired,
        )
        .unwrap();
        pipeline.recording_sender_for_mixed = Some(recording_sender);
        pipeline.stem_sender = Some(stem_sender);
        (
            pipeline,
            sender,
            recording_receiver,
            stem_receiver,
            transcription_receiver,
        )
    }

    #[tokio::test]
    async fn channel_close_sends_final_window_to_recording_and_stems() {
        let (pipeline, sender, mut recording, mut stems, _transcription) = pipeline_for_test(48000);
        // One full window followed by a partial window, with no explicit flush marker.
        for (id, timestamp, length) in [(0, 0.0, 28800), (1, 0.6, 127)] {
            sender
                .send(AudioChunk {
                    data: vec![0.25; length],
                    sample_rate: 48000,
                    timestamp,
                    end_timestamp: None,
                    chunk_id: id,
                    device_type: DeviceType::System,
                    source_activity: None,
                })
                .unwrap();
        }
        drop(sender);
        tokio::time::timeout(std::time::Duration::from_secs(5), pipeline.run())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(recording.try_recv().unwrap().data, vec![0.25; 28800]);
        assert_eq!(recording.try_recv().unwrap().data, vec![0.25; 127]);
        assert_eq!(stems.try_recv().unwrap().system, vec![0.25; 28800]);
        let tail = stems.try_recv().unwrap();
        assert_eq!(tail.system, vec![0.25; 127]);
        assert_eq!(tail.microphone, vec![0.0; 127]);
        assert!(recording.try_recv().is_err());
        assert!(stems.try_recv().is_err());
    }

    #[tokio::test]
    async fn repeated_final_flush_does_not_send_audio_twice() {
        let (mut pipeline, _sender, mut recording, mut stems, _transcription) =
            pipeline_for_test(48000);
        #[cfg(target_os = "linux")]
        pipeline
            .ring_buffer
            .add_samples_at(DeviceType::System, vec![0.25; 127], 0);
        #[cfg(not(target_os = "linux"))]
        pipeline
            .ring_buffer
            .add_samples(DeviceType::System, vec![0.25; 127]);
        pipeline.flush_remaining_audio().unwrap();
        pipeline.flush_remaining_audio().unwrap();
        assert_eq!(recording.try_recv().unwrap().data, vec![0.25; 127]);
        assert_eq!(stems.try_recv().unwrap().system, vec![0.25; 127]);
        assert!(recording.try_recv().is_err());
        assert!(stems.try_recv().is_err());
    }
    #[tokio::test]
    async fn final_speech_reaches_vad_before_flush() {
        let (pipeline, sender, mut recording, _stems, mut transcription) = pipeline_for_test(16000);
        // Use the existing VAD signal and stop while speech is active under the live policy.
        let mut samples = vec![0.0; 320000];
        samples.extend((0..32000).map(|i| {
            let time = i as f32 / 16000.0;
            let freq1 = 200.0 + (time * 50.0).sin() * 100.0;
            let freq2 = freq1 * 2.0;
            let freq3 = freq1 * 3.0;
            let amplitude = 0.3 + 0.1 * (time * 5.0).sin();
            amplitude
                * (0.5 * (2.0 * std::f32::consts::PI * freq1 * time).sin()
                    + 0.3 * (2.0 * std::f32::consts::PI * freq2 * time).sin()
                    + 0.2 * (2.0 * std::f32::consts::PI * freq3 * time).sin())
        }));
        for (id, chunk) in samples.chunks(1600).enumerate() {
            sender
                .send(AudioChunk {
                    data: chunk.to_vec(),
                    sample_rate: 16000,
                    timestamp: id as f64 * 0.1,
                    end_timestamp: None,
                    chunk_id: id as u64,
                    device_type: DeviceType::System,
                    source_activity: None,
                })
                .unwrap();
        }
        drop(sender);
        pipeline.run().await.unwrap();
        let mut saved = Vec::new();
        while let Ok(chunk) = recording.try_recv() {
            saved.extend(chunk.data);
        }
        assert_eq!(saved, samples);
        let segments: Vec<AudioChunk> =
            std::iter::from_fn(|| transcription.try_recv().ok()).collect();
        let final_segment = segments.last().expect("Test signal must produce speech");
        assert!(
            (final_segment.end_timestamp.unwrap() - 22.0).abs() < 0.001,
            "The final partial window must reach VAD before it is flushed"
        );
        assert!(
            final_segment
                .source_activity
                .as_ref()
                .unwrap()
                .system_active
        );
    }
    /// Replays saved microphone and system stems (mono f32 at 48 kHz) through
    /// the live pipeline and writes every transcription segment it sends.
    #[tokio::test]
    #[ignore = "requires MEETILY_REPLAY_MICROPHONE, MEETILY_REPLAY_SYSTEM, and MEETILY_REPLAY_OUTPUT"]
    async fn replay_saved_stems_through_live_pipeline() {
        let read = |key: &str| -> Vec<f32> {
            let path = std::env::var(key).unwrap_or_else(|_| panic!("set {key}"));
            std::fs::read(path)
                .unwrap()
                .chunks_exact(4)
                .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
                .collect()
        };
        let microphone = read("MEETILY_REPLAY_MICROPHONE");
        let system = read("MEETILY_REPLAY_SYSTEM");
        let output = std::path::PathBuf::from(
            std::env::var("MEETILY_REPLAY_OUTPUT").expect("set MEETILY_REPLAY_OUTPUT"),
        );
        std::fs::create_dir_all(&output).unwrap();
        let (pipeline, sender, _recording, _stems, mut transcription) = pipeline_for_test(48000);
        let task = tokio::spawn(pipeline.run());
        // 10 ms capture callbacks, alternating sources as the capture streams do.
        for (id, (mic, sys)) in microphone.chunks(480).zip(system.chunks(480)).enumerate() {
            for (device_type, data) in [(DeviceType::Microphone, mic), (DeviceType::System, sys)] {
                sender
                    .send(AudioChunk {
                        data: data.to_vec(),
                        sample_rate: 48000,
                        timestamp: id as f64 * 0.01,
                        end_timestamp: None,
                        chunk_id: id as u64,
                        device_type,
                        source_activity: None,
                    })
                    .unwrap();
            }
        }
        drop(sender);
        task.await.unwrap().unwrap();
        let mut segments = Vec::new();
        while let Ok(chunk) = transcription.try_recv() {
            let path = output.join(format!("{:04}.f32", segments.len()));
            let bytes: Vec<u8> = chunk.data.iter().flat_map(|s| s.to_le_bytes()).collect();
            std::fs::write(&path, bytes).unwrap();
            segments.push(serde_json::json!({
                "start": chunk.timestamp,
                "end": chunk.end_timestamp,
                "samples": chunk.data.len(),
                "path": path,
            }));
        }
        std::fs::write(
            output.join("segments.json"),
            serde_json::to_vec_pretty(&segments).unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn stop_drains_capture_before_pipeline_closes() {
        use super::super::device_detection::InputDeviceKind;
        for rate in [16000, 44100, 48000, 96000] {
            let state = RecordingState::new();
            state.start_recording().unwrap();
            let (transcription_sender, _transcription) = mpsc::unbounded_channel();
            let (recording_sender, mut recording) = mpsc::unbounded_channel();
            let mut manager = AudioPipelineManager::new();
            manager
                .start(
                    state.clone(),
                    transcription_sender,
                    600,
                    48000,
                    Some(recording_sender),
                    None,
                    "Test microphone".into(),
                    InputDeviceKind::Wired,
                    "Test system".into(),
                    InputDeviceKind::Wired,
                )
                .unwrap();
            let capture = AudioCapture::new(
                Arc::new(AudioDevice::new(
                    "Test capture".into(),
                    super::super::devices::DeviceType::Output,
                )),
                state.clone(),
                rate,
                1,
                DeviceType::System,
                None,
            );
            let callback_capture = capture.clone();
            callback_capture.process_audio_data(&[0.25; 127]);
            state.stop_recording();
            capture.finish().unwrap();
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                manager.stop(),
            )
            .await
            .unwrap()
            .unwrap();
            let chunk = recording.try_recv().unwrap();
            assert_eq!(chunk.data.len(), (127usize * 48000).div_ceil(rate as usize));
            assert!(chunk.data.iter().any(|v| v.abs() > 0.01));
            callback_capture.process_audio_data(&[0.75; 127]);
            callback_capture.finish().unwrap();
            assert!(matches!(
                recording.try_recv(),
                Err(mpsc::error::TryRecvError::Disconnected)
            ));
        }
    }
    #[tokio::test]
    async fn pipeline_stops_while_a_capture_is_still_stopping() {
        use super::super::device_detection::InputDeviceKind;
        let state = RecordingState::new();
        state.start_recording().unwrap();
        let (transcription_sender, _transcription) = mpsc::unbounded_channel();
        let (recording_sender, mut recording) = mpsc::unbounded_channel();
        let mut manager = AudioPipelineManager::new();
        manager
            .start(
                state.clone(),
                transcription_sender,
                600,
                48000,
                Some(recording_sender),
                None,
                "Test microphone".into(),
                InputDeviceKind::Wired,
                "Test system".into(),
                InputDeviceKind::Wired,
            )
            .unwrap();
        let capture = AudioCapture::new(
            Arc::new(AudioDevice::new(
                "Test capture".into(),
                super::super::devices::DeviceType::Output,
            )),
            state.clone(),
            48000,
            1,
            DeviceType::Microphone,
            None,
        );
        capture.process_audio_data(&[0.25; 127]);
        state.stop_recording();
        // A swapped-out stream whose native teardown is stalled never reaches
        // capture.finish(). The pipeline must still stop and keep the audio it received.
        tokio::time::timeout(std::time::Duration::from_secs(5), manager.stop())
            .await
            .unwrap()
            .unwrap();
        assert!(recording.try_recv().is_ok());
        // The stalled stream's own stop() runs later and finds nothing to send to.
        capture.process_audio_data(&[0.75; 127]);
        capture.finish().unwrap();
        assert!(matches!(
            recording.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn microphone_drain_keeps_the_final_marker_and_real_duration() {
        for rate in [16000, 44100, 48000, 96000] {
            for length in [1usize, 127, 479, 480, 511, 512, 1024, 1457] {
                let (capture, state, mut receiver) = capture_for_test(rate, DeviceType::Microphone);
                let mut input = vec![0.0; length];
                input[length.saturating_sub(80)..].fill(0.25);
                for chunk in input.chunks(137) {
                    capture.process_audio_data(chunk);
                }
                state.stop_recording();
                capture.finish().unwrap();
                capture.finish().unwrap();
                let output: Vec<f32> = std::iter::from_fn(|| receiver.try_recv().ok())
                    .flat_map(|chunk| chunk.data)
                    .collect();
                assert_eq!(
                    output.len(),
                    (length * 48000).div_ceil(rate as usize),
                    "rate={rate}, length={length}"
                );
                let marker_start = length.saturating_sub(80) * 48000 / rate as usize;
                assert!(
                    output[marker_start..].iter().any(|v| v.abs() > 0.001),
                    "Final microphone marker lost: rate={rate}, length={length}"
                );
            }
        }
    }

    #[test]
    fn noise_suppression_tail_is_sent_once_in_order() {
        let (capture, state, mut receiver) = capture_for_test(48000, DeviceType::Microphone);
        *capture.high_pass_filter.lock().unwrap() = None;
        *capture.noise_suppressor.lock().unwrap() =
            Some(NoiseSuppressionProcessor::new(48000).unwrap());
        let input: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.15).sin() * 0.25).collect();
        for chunk in input.chunks(137) {
            capture.process_audio_data(chunk);
        }
        state.stop_recording();
        capture.finish().unwrap();
        capture.finish().unwrap();
        let output: Vec<f32> = std::iter::from_fn(|| receiver.try_recv().ok())
            .flat_map(|chunk| chunk.data)
            .collect();
        let mut suppressor = NoiseSuppressionProcessor::new(48000).unwrap();
        let mut denoised = suppressor.process(&input);
        denoised.extend(suppressor.flush());
        let mut normalizer = LoudnessNormalizer::new(1, 48000).unwrap();
        let expected = normalizer.normalize_loudness(&denoised);
        assert_eq!(output.len(), input.len());
        assert_eq!(output, expected);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn queued_native_samples_are_delivered_before_the_capture_finishes() {
        let (capture, state, mut receiver) = capture_for_test(48000, DeviceType::System);
        capture.process_captured_samples(&[0.25]);
        state.stop_recording();
        // These samples were admitted by the producer before stop and can arrive afterwards.
        capture.process_captured_samples(&[0.5; 131072]);
        capture.finish().unwrap();
        capture.process_captured_samples(&[0.75; 1024]);
        let output: Vec<f32> = std::iter::from_fn(|| receiver.try_recv().ok())
            .flat_map(|chunk| chunk.data)
            .collect();
        assert_eq!(output.len(), 131073);
        assert_eq!(output[0], 0.25);
        assert_eq!(&output[1..], &[0.5; 131072]);
    }
}
