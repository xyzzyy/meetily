use anyhow::Result;
use log::{debug, error, info, warn};
use std::sync::Arc;
use tokio::sync::mpsc;
#[cfg(target_os = "macos")]
use std::time::Duration;
#[cfg(target_os = "macos")]
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::devices::AudioDevice;

use super::recording_state::{RecordingState, AudioChunk};
use super::diarization_stems::DiarizationStemSaver;
use super::pipeline::{AudioPipelineManager, PipelineStopError};
use super::recording_saver::RecordingSaver;
use super::stream::AudioStreamManager;
use super::device_monitor::{AudioDeviceMonitor, DeviceEvent};

/// Stream manager type enumeration
pub enum StreamManagerType {
    Standard(AudioStreamManager),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RecordingStartError {
    #[error("Failed to initialize speech recognition: {0}")]
    TranscriptionRuntime(#[source] anyhow::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Keep audio shutdown errors separate from final transcript warnings.
#[derive(Debug, Default)]
pub(crate) struct RecordingStopOutcome {
    /// Capture or the pipeline failed, so saved audio may be incomplete.
    pub audio_error: Option<anyhow::Error>,
    /// Only the final transcript segments may be missing.
    pub transcript_error: Option<anyhow::Error>,
}

#[derive(Clone)]
pub(crate) struct RecordingMetrics {
    pub duration: Option<f64>,
    pub active_duration: Option<f64>,
    pub pause_duration: f64,
    pub stats: super::recording_state::RecordingStats,
    pub had_fatal_error: bool,
    pub microphone_name: Option<String>,
    pub system_name: Option<String>,
}

// ============================================================================
// macOS Core Audio "pre-wake" — ported from feat/audio-device-handling
// (commit 8bf6af8b, with refinements from d266feac + 7107adc7 + a10f8e55).
// ============================================================================
//
// When a macOS Mac has a Bluetooth device as the active audio output, the
// built-in audio hardware unit — and, crucially, the BT audio connection
// itself — can sit in a low-power state where registered IO procs do not
// fire even though `AudioDeviceStart` returned `noErr`. The symptom is a
// 60–90 second "silent" period at recording start (or during a disconnect
// fallback) where the mic is technically capturing but no samples reach
// the pipeline, until something nudges Core Audio back to life.
//
// The workaround, empirically validated by Sujith in the audio-device-
// handling branch: briefly play 300 ms of digital silence through the
// output device by name. The name-based enumeration is itself part of the
// fix ("enumeration wake" — it forces the HAL to bring the device out of
// its idle power state). The subsequent `stream.play()` on the output
// activates the hardware unit fully, at which point the built-in mic's
// IO proc starts firing normally. Total cost: ~300 ms added to recording
// start, ~150 ms added to the disconnect-fallback hot-swap path.

#[cfg(target_os = "macos")]
const AUDIO_WAKE_DURATION: Duration = Duration::from_millis(300);

#[cfg(target_os = "macos")]
const AUDIO_WAKE_SWAP_DURATION: Duration = Duration::from_millis(150);

/// Synchronous implementation of the wake. Must be called from a blocking
/// thread (not directly from async code) because it sleeps 300 ms and does
/// sync cpal I/O. The async wrapper below routes to it via spawn_blocking.
#[cfg(target_os = "macos")]
fn wake_audio_connection_sync(speaker_device_name: &str) -> Result<()> {
    info!("[AUDIO_WAKE] Waking audio via speaker: '{}'", speaker_device_name);

    let host = cpal::default_host();

    let output_device = host.output_devices()?
        .find(|d| d.name().ok().as_deref() == Some(speaker_device_name))
        .ok_or_else(|| anyhow::anyhow!("Output device '{}' not found", speaker_device_name))?;

    let config = output_device.default_output_config()?;
    info!("[AUDIO_WAKE] Output config: {} Hz, {} channels",
          config.sample_rate().0, config.channels());

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => {
            output_device.build_output_stream(
                &config.into(),
                |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    for sample in data.iter_mut() { *sample = 0.0; }
                },
                |err| error!("[AUDIO_WAKE] Output stream error: {}", err),
                None,
            )?
        }
        cpal::SampleFormat::I16 => {
            // BT HFP mode frequently picks I16 for the output config — without
            // this branch the wake would error out exactly when we need it most.
            output_device.build_output_stream(
                &config.into(),
                |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                    for sample in data.iter_mut() { *sample = 0; }
                },
                |err| error!("[AUDIO_WAKE] Output stream error: {}", err),
                None,
            )?
        }
        format => {
            return Err(anyhow::anyhow!("Unsupported sample format: {:?}", format));
        }
    };

    stream.play()?;
    info!("[AUDIO_WAKE] Playing silence for {} ms to wake audio connection...", AUDIO_WAKE_DURATION.as_millis());
    std::thread::sleep(AUDIO_WAKE_DURATION);
    drop(stream);
    info!("[AUDIO_WAKE] Audio wake completed");

    Ok(())
}

/// Async wrapper around `wake_audio_connection_sync`. Used at recording start
/// so the 300 ms sleep doesn't block the tokio runtime. Non-fatal — the caller
/// should log the error and proceed if this fails; recording will still work,
/// it just may have the 60-90s silent startup on BT.
#[cfg(target_os = "macos")]
pub(super) async fn wake_audio_connection(speaker_device_name: &str) -> Result<()> {
    let name = speaker_device_name.to_string();
    tokio::task::spawn_blocking(move || {
        wake_audio_connection_sync(&name)
    }).await.map_err(|e| anyhow::anyhow!("Join error: {}", e))?
}

/// Public async wake for use by the disconnect-fallback hot-swap path in
/// `recording_commands.rs::trigger_mic_fallback_to_default`.
///
/// Differences from the start-time wake:
///   - Shorter 150 ms sleep to minimize fallback latency (the window of no
///     audio between primary death and secondary-taking-over)
///   - Falls back to the system default output if the named device isn't
///     found (e.g. the original BT output just disappeared, and we're
///     waking the MacBook speakers instead)
///   - Still handles both F32 and I16 sample formats for robustness
#[cfg(target_os = "macos")]
pub async fn wake_audio_connection_for_swap(speaker_device_name: &str) -> Result<()> {
    let name = speaker_device_name.to_string();
    tokio::task::spawn_blocking(move || {
        info!("[AUDIO_WAKE] Hot-swap wake via speaker: '{}'", name);
        let host = cpal::default_host();
        // Try the specified device first, fall back to default output. During
        // a BT disconnect the original output device name may no longer
        // enumerate, so the fallback is what actually runs in practice.
        let (output_device, config) = host.output_devices()?
            .find(|d| d.name().ok().as_deref() == Some(&*name))
            .and_then(|d| d.default_output_config().ok().map(|c| (d, c)))
            .or_else(|| {
                info!("[AUDIO_WAKE] '{}' not found or config failed — using default output device", name);
                host.default_output_device()
                    .and_then(|d| d.default_output_config().ok().map(|c| (d, c)))
            })
            .ok_or_else(|| anyhow::anyhow!("No output device available for wake"))?;
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => {
                output_device.build_output_stream(
                    &config.into(),
                    |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                        for sample in data.iter_mut() { *sample = 0.0; }
                    },
                    |err| error!("[AUDIO_WAKE] Hot-swap wake error: {}", err),
                    None,
                )?
            }
            cpal::SampleFormat::I16 => {
                output_device.build_output_stream(
                    &config.into(),
                    |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                        for sample in data.iter_mut() { *sample = 0; }
                    },
                    |err| error!("[AUDIO_WAKE] Hot-swap wake error: {}", err),
                    None,
                )?
            }
            format => {
                return Err(anyhow::anyhow!("Unsupported sample format for wake: {:?}", format));
            }
        };
        stream.play()?;
        std::thread::sleep(AUDIO_WAKE_SWAP_DURATION);
        drop(stream);
        info!("[AUDIO_WAKE] Hot-swap wake completed");
        Ok(())
    }).await.map_err(|e| anyhow::anyhow!("Join error: {}", e))?
}

/// Simplified recording manager that coordinates all audio components
pub struct RecordingManager {
    state: Arc<RecordingState>,
    stream_manager: AudioStreamManager,
    pipeline_manager: AudioPipelineManager,
    recording_saver: RecordingSaver,
    device_monitor: Option<AudioDeviceMonitor>,
    device_event_receiver: Option<mpsc::UnboundedReceiver<DeviceEvent>>,
    diarization_enabled: bool,
    stem_saver: Option<DiarizationStemSaver>,
    stopped_metrics: Option<RecordingMetrics>,
}

// SAFETY: RecordingManager contains types that we've marked as Send
unsafe impl Send for RecordingManager {}

impl RecordingManager {
    /// Create a new recording manager
    pub fn new() -> Self {
        let state = RecordingState::new();
        let stream_manager = AudioStreamManager::new(state.clone());
        let pipeline_manager = AudioPipelineManager::new();
        let (device_monitor, device_event_receiver) = AudioDeviceMonitor::new();

        Self {
            state,
            stream_manager,
            pipeline_manager,
            recording_saver: RecordingSaver::new(),
            stopped_metrics: None,
            device_monitor: Some(device_monitor),
            device_event_receiver: Some(device_event_receiver),
            diarization_enabled: false,
            stem_saver: None,
        }
    }

    // Remove app handle storage for now - will be passed directly when saving

    /// Start recording with specified devices
    ///
    /// On macOS, the command entry points wake audio and verify microphone
    /// access before constructing the manager and calling this method.
    ///
    /// # Arguments
    /// * `microphone_device` - Optional microphone device to use
    /// * `system_device` - Optional system audio device to use
    /// * `auto_save` - Whether to save audio checkpoints (true) or just transcripts/metadata (false)
    pub(crate) async fn start_recording(
        &mut self,
        microphone_device: Option<Arc<AudioDevice>>,
        system_device: Option<Arc<AudioDevice>>,
        auto_save: bool,
    ) -> std::result::Result<mpsc::UnboundedReceiver<AudioChunk>, RecordingStartError> {
        info!("Starting recording manager (auto_save: {})", auto_save);
        self.stopped_metrics = None;

        // Set up transcription channel
        let (transcription_sender, transcription_receiver) = mpsc::unbounded_channel::<AudioChunk>();
        let (recording_sender, recording_receiver) = mpsc::unbounded_channel::<AudioChunk>();

        // Check storage before starting the state, pipeline, or audio streams.
        self.recording_saver.prepare_meeting_folder(auto_save)?;
        self.recording_saver.start_accumulation(auto_save, recording_receiver)?;

        let stem_sender = if self.diarization_enabled && auto_save {
            self.recording_saver
                .get_meeting_folder()
                .and_then(|folder| match DiarizationStemSaver::start(folder, auto_save) {
                    Ok(Some(saver)) => {
                        let sender = saver.sender();
                        self.stem_saver = Some(saver);
                        sender
                    }
                    Ok(None) => None,
                    Err(error) => {
                        warn!("Diarization stems unavailable; recording will continue: {error}");
                        None
                    }
                })
        } else {
            None
        };

        // Start recording state first
        self.state.start_recording()?;

        // Get device information for adaptive mixing
        // The pipeline uses device kind (Bluetooth vs Wired) to apply adaptive buffering:
        // - Bluetooth: Larger buffers (80-200ms) to handle jitter
        // - Wired: Smaller buffers (20-50ms) for low latency
        let (mic_name, mic_kind) = if let Some(ref mic) = microphone_device {
            let device_kind =
                super::device_detection::InputDeviceKind::detect(&mic.name, 512, 48000);
            (mic.name.clone(), device_kind)
        } else {
            (
                "No Microphone".to_string(),
                super::device_detection::InputDeviceKind::Unknown,
            )
        };

        let (sys_name, sys_kind) = if let Some(ref sys) = system_device {
            let device_kind =
                super::device_detection::InputDeviceKind::detect(&sys.name, 512, 48000);
            (sys.name.clone(), device_kind)
        } else {
            (
                "No System Audio".to_string(),
                super::device_detection::InputDeviceKind::Unknown,
            )
        };

        // Start the audio processing pipeline with FFmpeg adaptive mixer
        // Pipeline will: 1) Mix mic+system audio with adaptive buffering, 2) Send mixed to recording_sender,
        // 3) Apply VAD and send speech segments to transcription
        if let Err(error) = self.pipeline_manager.start(
            self.state.clone(),
            transcription_sender,
            0,                      // Ignored - using dynamic sizing internally
            48000,                  // 48kHz sample rate
            Some(recording_sender), // CRITICAL: Pass recording sender to receive pre-mixed audio
            stem_sender,
            mic_name,
            mic_kind,
            sys_name,
            sys_kind,
        ) {
            self.state.stop_recording();
            return Err(RecordingStartError::TranscriptionRuntime(error));
        }

        self.recording_saver.set_device_info(
            microphone_device.as_ref().map(|d| d.name.clone()),
            system_device.as_ref().map(|d| d.name.clone())
        );

        // Give the pipeline a moment to fully initialize before starting streams
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Start audio streams - they send RAW unmixed chunks to pipeline for mixing
        // Pipeline handles mixing and distribution to both recording and transcription
        self.stream_manager
            .start_streams(microphone_device.clone(), system_device.clone(), None)
            .await?;

        // Start device monitoring to detect disconnects
        if let Some(ref mut monitor) = self.device_monitor {
            if let Err(e) = monitor.start_monitoring(microphone_device, system_device) {
                warn!("Failed to start device monitoring: {}", e);
                // Non-fatal - continue without monitoring
            } else {
                info!("✅ Device monitoring started");
            }
        }

        info!(
            "Recording manager started successfully with {} active streams",
            self.stream_manager.active_stream_count()
        );

        Ok(transcription_receiver)
    }

    /// Stop recording streams without saving (for use when waiting for transcription)
    pub async fn stop_streams_only(&mut self) -> Result<()> {
        info!("Stopping recording streams only");

        // Stop device monitoring
        if let Some(ref mut monitor) = self.device_monitor {
            monitor.stop_monitoring().await;
        }

        // Stop recording state first
        self.state.stop_recording();

        // Stop audio streams
        if let Err(e) = self.stream_manager.stop_streams() {
            error!("Error stopping audio streams: {}", e);
        }

        // Stop audio pipeline
        if let Err(e) = self.pipeline_manager.stop().await {
            error!("Error stopping audio pipeline: {}", e);
        }

        debug!("Recording streams stopped successfully");
        Ok(())
    }

    /// Stop streams and force immediate pipeline flush to process all accumulated audio
    pub(crate) async fn stop_streams_and_force_flush(&mut self) -> RecordingStopOutcome {
        info!("🚀 Stopping recording streams with IMMEDIATE pipeline flush");

        // Keep pause state and device names before stop clears them.
        let mut metrics = self.get_metrics();
        let was_paused = self.state.is_paused();

        // CRITICAL: Stop device monitor FIRST to prevent continuous WASAPI polling on Windows
        // This fixes the slow shutdown issue where device enumeration runs for 90+ seconds
        if let Some(ref mut monitor) = self.device_monitor {
            info!("Stopping device monitor first...");
            monitor.stop_monitoring().await;
        }

        // Stop recording state first - this clears device references
        self.state.stop_recording();

        // Stop capture and drain the pipeline even when either step fails.
        let capture_error = self.stream_manager.stop_streams().err();
        if let Some(error) = &capture_error {
            error!("Error stopping audio streams: {error:#}");
        }
        if self.stopped_metrics.is_none() {
            // Capture can continue while monitoring and streams shut down.
            // Freeze timing here, before waiting for pipeline drain or saving.
            let duration = self.state.get_recording_duration();
            let shutdown_duration = duration.zip(metrics.duration)
                .map(|(end, start)| (end - start).max(0.0)).unwrap_or(0.0);
            metrics.duration = duration;
            if was_paused {
                metrics.pause_duration += shutdown_duration;
            } else {
                metrics.active_duration = metrics.active_duration.map(|value| value + shutdown_duration);
            }
        }
        let (pipeline_error, transcript_error) = match self.pipeline_manager.stop().await {
            Ok(()) => (None, None),
            Err(PipelineStopError::Pipeline(error)) => {
                error!("Audio pipeline failed before stop: {error:#}");
                (Some(error), None)
            }
            Err(PipelineStopError::FinalTranscript(error)) => {
                error!("Error during force flush: {error:#}");
                (None, Some(error))
            }
        };
        let audio_error = match (capture_error, pipeline_error) {
            (Some(capture), Some(pipeline)) => Some(anyhow::anyhow!("{capture:#}. {pipeline:#}")),
            (capture, pipeline) => capture.or(pipeline),
        };

        // CRITICAL: Full cleanup to release all Arc references and resources
        // This ensures microphone is released even if Drop is delayed
        if self.stopped_metrics.is_none() {
            // Include chunks and errors produced while the pipeline drained.
            metrics.stats = self.state.get_stats();
            metrics.had_fatal_error |= self.state.has_fatal_error();
            self.stopped_metrics = Some(metrics);
        }
        self.state.cleanup();

        RecordingStopOutcome { audio_error, transcript_error }
    }

    /// Save recording after transcription is complete
    pub async fn save_recording_only<R: tauri::Runtime>(
        &mut self,
        app: &tauri::AppHandle<R>,
    ) -> Result<()> {
        debug!("Saving recording with transcript chunks");

        // Get actual recording duration from state
        let recording_duration = self.get_active_recording_duration();
        info!("Recording duration from state: {:?}s", recording_duration);

        // Save the recording with actual duration
        let save_result = self.recording_saver.stop_and_save(app, recording_duration).await;
        self.finalize_diarization_stems().await;
        match save_result {
            Ok(Some(file_path)) => {
                info!("Recording saved successfully to: {}", file_path);
            }
            Ok(None) => {
                debug!("Recording not saved (auto-save disabled or no audio data)");
            }
            Err(e) => {
                error!("Failed to save recording: {}", e);
                return Err(anyhow::anyhow!(e));
            }
        }

        debug!("Recording save operation completed");
        Ok(())
    }

    /// Stop recording and save audio (legacy method)
    pub async fn stop_recording<R: tauri::Runtime>(
        &mut self,
        app: &tauri::AppHandle<R>,
    ) -> Result<()> {
        info!("Stopping recording manager");

        // Get recording duration BEFORE stopping (important!)
        let recording_duration = self.state.get_active_recording_duration();
        info!("Recording duration before stop: {:?}s", recording_duration);

        // Stop recording state first
        self.state.stop_recording();

        // Stop audio streams
        if let Err(e) = self.stream_manager.stop_streams() {
            error!("Error stopping audio streams: {}", e);
        }

        // Stop audio pipeline
        if let Err(e) = self.pipeline_manager.stop().await {
            error!("Error stopping audio pipeline: {}", e);
        }

        // Save the recording with actual duration
        let save_result = self.recording_saver.stop_and_save(app, recording_duration).await;
        self.finalize_diarization_stems().await;
        match save_result {
            Ok(Some(file_path)) => {
                info!("Recording saved successfully to: {}", file_path);
            }
            Ok(None) => {
                info!("Recording not saved (auto-save disabled or no audio data)");
            }
            Err(e) => {
                error!("Failed to save recording: {}", e);
                return Err(anyhow::anyhow!(e));
            }
        }

        info!("Recording manager stopped");
        Ok(())
    }

    /// Get recording stats from the saver
    pub fn get_recording_stats(&self) -> (usize, u32) {
        self.recording_saver.get_stats()
    }

    /// Check if currently recording
    pub fn is_recording(&self) -> bool {
        self.state.is_recording()
    }

    /// Pause the current recording session
    pub fn pause_recording(&self) -> Result<()> {
        info!("Pausing recording");
        self.state.pause_recording()
    }

    /// Resume the current recording session
    pub fn resume_recording(&self) -> Result<()> {
        info!("Resuming recording");
        self.state.resume_recording()
    }

    /// Check if recording is currently paused
    pub fn is_paused(&self) -> bool {
        self.state.is_paused()
    }

    /// Check if recording is active (recording and not paused)
    pub fn is_active(&self) -> bool {
        self.state.is_active()
    }

    /// Get recording statistics
    pub fn get_stats(&self) -> super::recording_state::RecordingStats {
        self.get_metrics().stats
    }

    pub(crate) fn get_metrics(&self) -> RecordingMetrics {
        self.stopped_metrics.clone().unwrap_or_else(|| RecordingMetrics {
            duration: self.state.get_recording_duration(),
            active_duration: self.state.get_active_recording_duration(),
            pause_duration: self.state.get_total_pause_duration()
                + self.state.get_current_pause_duration().unwrap_or(0.0),
            stats: self.state.get_stats(),
            had_fatal_error: self.state.has_fatal_error(),
            microphone_name: self.state.get_microphone_device().map(|device| device.name.clone()),
            system_name: self.state.get_system_device().map(|device| device.name.clone()),
        })
    }

    /// Get recording duration
    pub fn get_recording_duration(&self) -> Option<f64> {
        self.get_metrics().duration
    }

    /// Get active recording duration (excluding pauses)
    pub fn get_active_recording_duration(&self) -> Option<f64> {
        self.get_metrics().active_duration
    }

    /// Get total pause duration
    pub fn get_total_pause_duration(&self) -> f64 {
        self.get_metrics().pause_duration
    }

    /// Get current pause duration if paused
    pub fn get_current_pause_duration(&self) -> Option<f64> {
        self.state.get_current_pause_duration()
    }

    /// Get error information
    pub fn get_error_info(&self) -> (u32, Option<super::recording_state::AudioError>) {
        (self.state.get_error_count(), self.state.get_last_error())
    }

    /// Get active stream count
    pub fn active_stream_count(&self) -> usize {
        self.stream_manager.active_stream_count()
    }

    /// Set error callback for handling errors
    pub fn set_error_callback<F>(&self, callback: F)
    where
        F: Fn(&super::recording_state::AudioError) + Send + Sync + 'static,
    {
        self.state.set_error_callback(callback);
    }

    /// Check if there's a fatal error
    pub fn has_fatal_error(&self) -> bool {
        self.state.has_fatal_error()
    }

    /// Set the meeting name for this recording session
    pub fn set_meeting_name(&mut self, name: Option<String>) {
        self.recording_saver.set_meeting_name(name);
    }

    pub fn set_diarization_enabled(&mut self, enabled: bool) {
        self.diarization_enabled = enabled;
    }

    async fn finalize_diarization_stems(&mut self) {
        if let Some(mut saver) = self.stem_saver.take() {
            if let Err(error) = saver.finalize().await {
                warn!("Diarization stem finalization failed; recording remains usable: {error}");
            }
        }
    }

    /// Add a structured transcript segment to be saved later
    pub fn add_transcript_segment(&self, segment: super::recording_saver::TranscriptSegment) {
        self.recording_saver.add_transcript_segment(segment);
    }

    /// Add a transcript chunk to be saved later (legacy method)
    pub fn add_transcript_chunk(&self, text: String) {
        self.recording_saver.add_transcript_chunk(text);
    }

    /// Get accumulated transcript segments from current recording session
    /// Used for syncing frontend state after page reload during active recording
    pub fn get_transcript_segments(&self) -> Vec<super::recording_saver::TranscriptSegment> {
        self.recording_saver.get_transcript_segments()
    }

    /// Get meeting name from current recording session
    /// Used for syncing frontend state after page reload during active recording
    pub fn get_meeting_name(&self) -> Option<String> {
        self.recording_saver.get_meeting_name()
    }

    /// Cleanup all resources without saving
    pub async fn cleanup_without_save(&mut self) {
        if self.is_recording() {
            debug!("Stopping recording without saving during cleanup");

            // Stop recording state first
            self.state.stop_recording();

            // Stop audio streams
            if let Err(e) = self.stream_manager.stop_streams() {
                error!("Error stopping audio streams during cleanup: {}", e);
            }

            // Stop audio pipeline
            if let Err(e) = self.pipeline_manager.stop().await {
                error!("Error stopping audio pipeline during cleanup: {}", e);
            }
        }
        self.state.cleanup();
    }

    /// Get the meeting folder path (if available)
    /// Returns None if no meeting name was set or folder structure not initialized
    pub fn get_meeting_folder(&self) -> Option<std::path::PathBuf> {
        self.recording_saver.get_meeting_folder().map(|p| p.clone())
    }

    /// Take ownership of the device event receiver for use by a background processor.
    pub fn take_device_event_receiver(&mut self) -> Option<mpsc::UnboundedReceiver<DeviceEvent>> {
        self.device_event_receiver.take()
    }

    /// Take the mic stream OUT for hot-swap (Phase 1) so the caller can tear it
    /// down without holding RECORDING_MANAGER. System audio continues uninterrupted.
    pub fn take_mic_stream_for_swap(&mut self) -> Option<super::stream::AudioStream> {
        self.stream_manager.take_mic_stream()
    }

    /// Set new mic stream and update device state after hot-swap (Phase 3).
    pub fn set_mic_stream_after_swap(
        &mut self,
        stream: super::stream::AudioStream,
        device: Arc<AudioDevice>,
        system_name: Option<String>,
    ) {
        self.stream_manager.set_mic_stream(stream);
        // Notify the device monitor so it tracks the new mic (and updated
        // system output) instead of the dead ones (M8 fix). BT devices like
        // AirPods are often both mic AND speaker — when they disconnect, both
        // monitor entries go stale. `system_name` is the *current* default
        // output, resolved lock-free in Phase 2 by the caller (do_mic_swap)
        // so a CoreAudio stall can't happen under the RECORDING_MANAGER lock;
        // `state.get_system_device()` still holds the pre-swap
        // reference (e.g. AirPods) because we don't mutate system_device
        // during a mic-only hot-swap, which would leave the monitor tracking
        // the dead name and spamming "missing for N checks".
        if let Some(ref monitor) = self.device_monitor {
            monitor.notify_mic_swapped(device.name.clone(), system_name);
        }
        self.state.set_microphone_device(device);
    }

    /// Get reference to recording state for external access
    pub fn get_state(&self) -> &Arc<RecordingState> {
        &self.state
    }
}

impl Default for RecordingManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for RecordingManager {
    fn drop(&mut self) {
        // Note: Can't call async cleanup in Drop, but streams have their own Drop implementations
        self.state.cleanup();
    }
}

#[cfg(all(test, target_os = "macos"))]
mod wake_tests {
    use super::*;

    #[test]
    fn wake_fails_gracefully_for_unknown_device() {
        // Nonexistent device name fails at the `find` step — no audio hardware needed.
        // Asserts the wake is non-fatal (returns Err, does not panic).
        assert!(wake_audio_connection_sync("__no_such_device__").is_err());
    }
}

#[cfg(test)]
mod stop_tests {
    use super::*;

    #[tokio::test]
    async fn final_timing_includes_capture_shutdown_for_active_and_paused_recordings() {
        for paused in [false, true] {
            let mut manager = RecordingManager::new();
            manager.state.start_recording().unwrap();
            if paused { manager.state.pause_recording().unwrap(); }
            manager.device_monitor = Some(AudioDeviceMonitor::with_task_for_test(tokio::spawn(async {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            })));
            manager.stop_streams_and_force_flush().await;
            let metrics = manager.get_metrics();
            assert!(metrics.duration.unwrap() >= 0.05);
            if paused {
                assert!(metrics.pause_duration >= 0.05);
                assert!(metrics.active_duration.unwrap() < metrics.pause_duration);
            } else {
                assert!(metrics.active_duration.unwrap() >= 0.05);
            }
        }
    }

    #[tokio::test]
    async fn stop_keeps_final_metrics_after_resource_cleanup() {
        let mut manager = RecordingManager::new();
        manager.state.start_recording().unwrap();
        let microphone = Arc::new(AudioDevice::new(
            "Test microphone".into(), super::super::devices::DeviceType::Input,
        ));
        let device = Arc::downgrade(&microphone);
        manager.state.set_microphone_device(microphone);
        let (sender, _receiver) = mpsc::unbounded_channel();
        manager.state.set_audio_sender(sender);
        manager.state.send_captured_chunk(AudioChunk {
            data: vec![0.0], sample_rate: 48_000, timestamp: 0.0,
            end_timestamp: None, chunk_id: 1,
            device_type: super::super::recording_state::DeviceType::Microphone,
            source_activity: None,
        }).unwrap();
        manager.state.pause_recording().unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        manager.stop_streams_and_force_flush().await;
        let metrics = manager.get_metrics();
        assert!(metrics.duration.unwrap() >= 0.02);
        assert!(metrics.pause_duration >= 0.02);
        assert!(metrics.active_duration.unwrap() < metrics.pause_duration);
        assert_eq!(metrics.microphone_name.as_deref(), Some("Test microphone"));
        assert_eq!(metrics.stats.chunks_processed, 1);
        assert!(device.upgrade().is_none(), "metrics must not retain audio devices");
        assert_eq!(manager.state.get_stats().chunks_processed, 0);
        assert!(manager.state.get_recording_duration().is_none());
        assert!(!manager.state.is_recording());
        assert!(!manager.state.is_paused());
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        manager.stop_streams_and_force_flush().await;
        assert_eq!(manager.get_recording_duration(), metrics.duration);
        assert_eq!(manager.get_active_recording_duration(), metrics.active_duration);
        assert_eq!(manager.get_total_pause_duration(), metrics.pause_duration);
    }

    #[tokio::test]
    async fn final_flush_failure_reaches_the_caller_after_cleanup() {
        let mut manager = RecordingManager::new();
        manager.state.start_recording().unwrap();
        manager.pipeline_manager = AudioPipelineManager::with_task_for_test(tokio::spawn(async {
            Err(anyhow::anyhow!("Injected final flush failure"))
        }));

        let outcome = manager.stop_streams_and_force_flush().await;
        assert!(outcome.audio_error.is_none());
        assert!(outcome.transcript_error.unwrap().to_string().contains("Injected final flush failure"));
        assert!(!manager.state.is_recording());
        // cleanup() clears the start time as well as the recording flag.
        assert!(manager.state.get_recording_duration().is_none());
        let outcome = manager.stop_streams_and_force_flush().await;
        assert!(outcome.audio_error.is_none());
        assert!(outcome.transcript_error.is_none());
    }
    #[tokio::test]
    async fn pipeline_failure_reports_incomplete_audio() {
        let mut manager = RecordingManager::new();
        manager.state.start_recording().unwrap();
        manager.pipeline_manager = AudioPipelineManager::with_task_for_test(tokio::spawn(async {
            panic!("Injected pipeline panic")
        }));

        let outcome = manager.stop_streams_and_force_flush().await;
        assert!(outcome.audio_error.unwrap().to_string().contains("Audio pipeline task failed"));
        assert!(outcome.transcript_error.is_none());
        assert!(!manager.state.is_recording());
    }
}
