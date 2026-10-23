use super::batch_processor::AudioMetricsBatcher;
use crate::batch_audio_metric;
use anyhow::Result;
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
use super::vad::ContinuousVadProcessor;

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

/// Ring buffer for synchronized audio mixing
/// Accumulates samples from mic and system streams until we have aligned windows
struct AudioMixerRingBuffer {
    mic_buffer: VecDeque<f32>,
    system_buffer: VecDeque<f32>,
    window_size_samples: usize, // Fixed mixing window (e.g., 50ms)
    max_buffer_size: usize,     // Safety limit (e.g., 100ms)
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
        let window_ms = 600.0;
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
            mic_buffer: VecDeque::with_capacity(max_buffer_size),
            system_buffer: VecDeque::with_capacity(max_buffer_size),
            window_size_samples,
            max_buffer_size,
        }
    }

    fn add_samples(&mut self, device_type: DeviceType, samples: Vec<f32>) {
        // Log buffer health periodically for diagnostics
        static SAMPLE_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        if SAMPLE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 200 == 0 {
            debug!(
                "Ring buffer status: mic={} samples, sys={} samples (max={})",
                self.mic_buffer.len(),
                self.system_buffer.len(),
                self.max_buffer_size
            );
        }

        match device_type {
            DeviceType::Microphone => self.mic_buffer.extend(samples),
            DeviceType::System => self.system_buffer.extend(samples),
        }

        // CRITICAL FIX: Add warnings before dropping samples
        // This helps diagnose timing issues in production
        if self.mic_buffer.len() > self.max_buffer_size {
            warn!(
                "⚠️ Microphone buffer overflow: {} > {} samples, dropping oldest {} samples",
                self.mic_buffer.len(),
                self.max_buffer_size,
                self.mic_buffer.len() - self.max_buffer_size
            );
        }
        if self.system_buffer.len() > self.max_buffer_size {
            error!("🔴 SYSTEM AUDIO BUFFER OVERFLOW: {} > {} samples, dropping {} samples - THIS CAUSES DISTORTION!",
                  self.system_buffer.len(), self.max_buffer_size,
                  self.system_buffer.len() - self.max_buffer_size);
        }

        // Safety: prevent buffer overflow (keep only last 200ms)
        while self.mic_buffer.len() > self.max_buffer_size {
            self.mic_buffer.pop_front();
        }
        while self.system_buffer.len() > self.max_buffer_size {
            self.system_buffer.pop_front();
        }
    }

    fn can_mix(&self) -> bool {
        self.mic_buffer.len() >= self.window_size_samples
            || self.system_buffer.len() >= self.window_size_samples
    }

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
    // Note: Using global recording timestamp for synchronization
}

struct CaptureProcessing {
    finished: bool,
    pending_samples: Vec<f32>,
    sender: Option<mpsc::UnboundedSender<AudioChunk>>,
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

        let sender = state.audio_sender();
        Self {
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
                sender,
            })),
            noise_suppressor: Arc::new(std::sync::Mutex::new(noise_suppressor)),
            high_pass_filter: Arc::new(std::sync::Mutex::new(high_pass_filter)),
            normalizer: Arc::new(std::sync::Mutex::new(normalizer)),
            // Using global recording time for sync
        }
    }

    /// Process audio data directly from callback
    pub fn process_audio_data(&self, data: &[f32]) {
        let processing = self.processing.lock().unwrap();
        if processing.finished || !self.state.is_active() {
            return;
        }
        self.process_input(data, &processing.sender);
    }

    /// Batch Core Audio samples under the same lock used by finish().
    #[cfg(target_os = "macos")]
    pub fn process_audio_sample(&self, sample: f32) {
        let mut processing = self.processing.lock().unwrap();
        if processing.finished || !self.state.is_active() {
            return;
        }
        processing.pending_samples.push(sample);
        if processing.pending_samples.len() >= 1024 {
            self.process_input(&processing.pending_samples, &processing.sender);
            processing.pending_samples.clear();
        }
    }

    /// Send all accepted samples before closing this capture's pipeline sender.
    pub fn finish(&self) -> Result<()> {
        let mut processing = self.processing.lock().unwrap();
        if processing.finished {
            return Ok(());
        }
        processing.finished = true;
        let samples = std::mem::take(&mut processing.pending_samples);
        self.process_input(&samples, &processing.sender);
        let result = (|| {
            if let Some(resampler) = self.resampler.lock().unwrap().as_mut() {
                let tail = resampler.finish()?;
                self.process_output(tail, &processing.sender);
            }
            Ok(())
        })();
        // Release the sender even if the resampler fails, so stop can finish.
        processing.sender = None;
        result
    }

    fn process_input(&self, data: &[f32], sender: &Option<mpsc::UnboundedSender<AudioChunk>>) {
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
            let mut resampler = self.resampler.lock().unwrap();
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
        self.process_output(mono_data, sender);
    }

    fn process_output(
        &self,
        mut mono_data: Vec<f32>,
        sender: &Option<mpsc::UnboundedSender<AudioChunk>>,
    ) {
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
            if super::ffmpeg_mixer::RNNOISE_APPLY_ENABLED {
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
                                   if !mono_data.is_empty() {
                                       (mono_data.iter().map(|&x| x * x).sum::<f32>() / mono_data.len() as f32).sqrt()
                                   } else { 0.0 });

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

            // STEP 3: Apply EBU R128 normalization (professional loudness standard)
            if let Ok(mut normalizer_lock) = self.normalizer.lock() {
                if let Some(ref mut normalizer) = *normalizer_lock {
                    mono_data = normalizer.normalize_loudness(&mono_data);

                    // Log normalization occasionally for debugging
                    let chunk_id = self.chunk_counter.load(std::sync::atomic::Ordering::SeqCst);
                    if chunk_id % 200 == 0 && !mono_data.is_empty() {
                        let rms = (mono_data.iter().map(|&x| x * x).sum::<f32>()
                            / mono_data.len() as f32)
                            .sqrt();
                        let peak = mono_data.iter().map(|&x| x.abs()).fold(0.0f32, f32::max);
                        debug!(
                            "🎤 After normalization chunk {}: RMS={:.4}, Peak={:.4}",
                            chunk_id, rms, peak
                        );
                    }
                }
            }
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
        let timestamp = self.state.get_recording_duration().unwrap_or(0.0);

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

        // Keep the session sender until finish(), even after recording state stops.
        if let Some(sender) = sender {
            if let Err(e) = self.state.send_captured_chunk(sender, audio_chunk) {
                warn!("Failed to send audio chunk: {}", e);
                self.state.report_error(AudioError::ChannelClosed);
            }
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

/// VAD-driven audio processing pipeline
/// Uses Voice Activity Detection to segment speech in real-time and send only speech to Whisper
pub struct AudioPipeline {
    receiver: mpsc::UnboundedReceiver<AudioChunk>,
    transcription_sender: mpsc::UnboundedSender<AudioChunk>,
    state: Arc<RecordingState>,
    vad_processor: ContinuousVadProcessor,
    sample_rate: u32,
    chunk_id_counter: u64,
    // Performance optimization: reduce logging frequency
    last_summary_time: std::time::Instant,
    processed_chunks: u64,
    // Smart batching for audio metrics
    metrics_batcher: Option<AudioMetricsBatcher>,
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
            sample_rate,
            chunk_id_counter: 0,
            // Performance optimization: reduce logging frequency
            last_summary_time: std::time::Instant::now(),
            processed_chunks: 0,
            // Initialize metrics batcher for smart batching
            metrics_batcher: Some(AudioMetricsBatcher::new()),
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
    pub async fn run(mut self) -> Result<()> {
        info!("VAD-driven audio pipeline started - segments sent in real-time based on speech detection");

        // CRITICAL FIX: Continue processing until channel is closed, not based on recording state
        // This ensures ALL chunks are processed during shutdown, fixing premature meeting completion
        // Previous bug: Loop checked `while self.state.is_recording()` which caused early exit when
        // stop_recording() was called, losing flush signals and remaining chunks in the pipeline
        loop {
            // Receive audio chunks with timeout
            match tokio::time::timeout(
                std::time::Duration::from_millis(50), // Shorter timeout for responsiveness
                self.receiver.recv(),
            )
            .await
            {
                Ok(Some(chunk)) => {
                    // PERFORMANCE: Check for flush signal (special chunk with ID >= u64::MAX - 10)
                    // Multiple flush signals may be sent to ensure processing
                    if chunk.chunk_id >= u64::MAX - 10 {
                        info!(
                            "📥 Received FLUSH signal #{} - flushing VAD processor",
                            u64::MAX - chunk.chunk_id
                        );
                        self.flush_remaining_audio()?;
                        // Continue processing to handle any remaining chunks
                        continue;
                    }

                    // PERFORMANCE OPTIMIZATION: Eliminate per-chunk logging overhead
                    // Logging in hot paths causes severe performance degradation
                    self.processed_chunks += 1;

                    // Smart batching: collect metrics instead of logging every chunk
                    if let Some(ref batcher) = self.metrics_batcher {
                        let avg_level = chunk.data.iter().map(|&x| x.abs()).sum::<f32>()
                            / chunk.data.len() as f32;
                        let duration_ms =
                            chunk.data.len() as f64 / chunk.sample_rate as f64 * 1000.0;

                        batch_audio_metric!(
                            Some(batcher),
                            chunk.chunk_id,
                            chunk.data.len(),
                            duration_ms,
                            avg_level
                        );
                    }

                    // CRITICAL: Log summary only every 200 chunks OR every 60 seconds (99.5% reduction)
                    // This eliminates I/O overhead in the audio processing hot path
                    // Use performance-optimized debug macro that compiles to nothing in release builds
                    if self.processed_chunks % 200 == 0
                        || self.last_summary_time.elapsed().as_secs() >= 60
                    {
                        perf_debug!(
                            "Pipeline processed {} chunks, current chunk: {} ({} samples)",
                            self.processed_chunks,
                            chunk.chunk_id,
                            chunk.data.len()
                        );
                        self.last_summary_time = std::time::Instant::now();
                    }

                    // STEP 1: Add raw audio to ring buffer for mixing
                    // Microphone audio is already normalized at capture level (AudioCapture)
                    // System audio remains raw
                    self.ring_buffer
                        .add_samples(chunk.device_type.clone(), chunk.data);

                    // STEP 2: Mix audio in fixed windows when both streams have sufficient data
                    while self.ring_buffer.can_mix() {
                        if let Some((mic_window, sys_window)) = self.ring_buffer.extract_window() {
                            self.process_window(mic_window, sys_window, chunk.timestamp);
                        }
                    }
                }
                Ok(None) => {
                    info!(
                        "Audio pipeline: sender closed after processing {} chunks",
                        self.processed_chunks
                    );
                    break;
                }
                Err(_) => {
                    // Timeout - just continue, VAD handles all segmentation
                    continue;
                }
            }
        }

        // Flush any remaining VAD segments
        self.flush_remaining_audio()?;

        info!("VAD-driven audio pipeline ended");
        Ok(())
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

        // STEP 3: Send mixed audio for transcription (VAD + Whisper)
        match self.vad_processor.process_audio(&mixed_with_gain) {
            Ok(speech_segments) => {
                for segment in speech_segments {
                    let duration_ms = segment.end_timestamp_ms - segment.start_timestamp_ms;

                    if segment.samples.len() >= 800 {
                        // Minimum 50ms at 16kHz - matches Parakeet capability
                        info!(
                            "📤 Sending VAD segment: {:.1}ms, {} samples",
                            duration_ms,
                            segment.samples.len()
                        );

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

                        if let Err(e) = self.transcription_sender.send(transcription_chunk) {
                            warn!("Failed to send VAD segment: {}", e);
                        } else {
                            self.chunk_id_counter += 1;
                        }
                    } else {
                        debug!(
                            "⏭️ Dropping short VAD segment: {:.1}ms ({} samples < 800)",
                            duration_ms,
                            segment.samples.len()
                        );
                    }
                }
            }
            Err(e) => {
                warn!("⚠️ VAD error: {}", e);
            }
        }

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

    fn flush_remaining_audio(&mut self) -> Result<()> {
        info!(
            "Flushing remaining audio from pipeline (processed {} chunks)",
            self.processed_chunks
        );

        if let Some((mic_window, sys_window)) = self.ring_buffer.extract_remaining() {
            self.process_window(mic_window, sys_window, self.source_timeline_seconds);
        }

        // Flush any remaining audio from VAD processor and send segments to transcription
        match self.vad_processor.flush() {
            Ok(final_segments) => {
                for segment in final_segments {
                    let duration_ms = segment.end_timestamp_ms - segment.start_timestamp_ms;

                    // Send segments >= 50ms (800 samples at 16kHz) - matches main pipeline filter
                    if segment.samples.len() >= 800 {
                        info!(
                            "📤 Sending final VAD segment to Whisper: {:.1}ms duration, {} samples",
                            duration_ms,
                            segment.samples.len()
                        );

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

                        if let Err(e) = self.transcription_sender.send(transcription_chunk) {
                            warn!("Failed to send final VAD segment: {}", e);
                        } else {
                            self.chunk_id_counter += 1;
                        }
                    } else {
                        info!(
                            "⏭️ Skipping short final segment: {:.1}ms ({} samples < 800)",
                            duration_ms,
                            segment.samples.len()
                        );
                    }
                }
            }
            Err(e) => {
                warn!("Failed to flush VAD processor: {}", e);
            }
        }

        Ok(())
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
}

/// Simple audio pipeline manager
pub struct AudioPipelineManager {
    pipeline_handle: Option<JoinHandle<Result<()>>>,
    audio_sender: Option<mpsc::UnboundedSender<AudioChunk>>,
}

impl AudioPipelineManager {
    pub fn new() -> Self {
        Self {
            pipeline_handle: None,
            audio_sender: None,
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

        info!("Audio pipeline manager started with mixed audio recording");
        Ok(())
    }

    /// Stop the audio pipeline
    pub async fn stop(&mut self) -> Result<()> {
        // Drop the sender to close the pipeline
        self.audio_sender = None;

        // Wait for pipeline to finish
        if let Some(handle) = self.pipeline_handle.take() {
            match handle.await {
                Ok(result) => result,
                Err(e) => {
                    error!("Pipeline task failed: {}", e);
                    Ok(())
                }
            }
        } else {
            Ok(())
        }
    }

    /// Drain the channel, mixer, and VAD in that order.
    pub async fn force_flush_and_stop(&mut self) -> Result<()> {
        self.stop().await
    }
}

impl Default for AudioPipelineManager {
    fn default() -> Self {
        Self::new()
    }
}
#[cfg(test)]
mod tests {
    use super::*;

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
    fn capture_drains_after_state_stops_and_closes_sender_once() {
        for rate in [16000, 44100, 48000, 96000] {
            let (capture, state, mut receiver) = capture_for_test(rate, DeviceType::System);
            let input = vec![0.25; 128];
            capture.process_audio_data(&input);
            state.stop_recording();
            capture.finish().unwrap();
            capture.finish().unwrap();
            capture.process_audio_data(&input);
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
        for _ in 0..127 {
            capture.process_audio_sample(0.25);
        }
        state.pause_recording().unwrap();
        capture.process_audio_sample(0.75);
        capture.process_audio_data(&[0.75; 128]);
        state.stop_recording();
        capture.finish().unwrap();
        assert_eq!(receiver.try_recv().unwrap().data, vec![0.25; 127]);
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        ));
    }

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
        for (id, length) in [(0, 28800), (1, 127)] {
            sender
                .send(AudioChunk {
                    data: vec![0.25; length],
                    sample_rate: 48000,
                    timestamp: 0.0,
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
    #[tokio::test]
    async fn stop_drains_capture_before_pipeline_and_releases_cloned_senders() {
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
                manager.force_flush_and_stop(),
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
}
