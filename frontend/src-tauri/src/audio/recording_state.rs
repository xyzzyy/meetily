use anyhow::Result;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::mpsc;

use super::buffer_pool::AudioBufferPool;
use super::devices::AudioDevice;

/// Device type for audio chunks
#[derive(Debug, Clone, PartialEq)]
pub enum DeviceType {
    Microphone,
    System,
}

#[derive(Debug, Clone, Default)]
pub struct SourceActivity {
    pub microphone_active: bool,
    pub system_active: bool,
    pub system_available: bool,
    /// Both sources were active in the same audio window.
    pub overlap_detected: bool,
}

/// Audio chunk with metadata for processing
#[derive(Debug, Clone)]
pub struct AudioChunk {
    pub data: Vec<f32>,
    pub sample_rate: u32,
    /// Recording-relative start time in seconds when this is a VAD segment.
    pub timestamp: f64,
    /// Recording-relative end time supplied by VAD. Raw capture chunks do not
    /// have a speech boundary, so they leave this unset.
    pub end_timestamp: Option<f64>,
    pub chunk_id: u64,
    pub device_type: DeviceType,
    pub source_activity: Option<SourceActivity>,
}

/// Processed audio chunk (post-VAD) for recording
#[derive(Debug, Clone)]
pub struct ProcessedAudioChunk {
    pub data: Vec<f32>,
    pub sample_rate: u32,
    pub timestamp: f64,
    pub device_type: DeviceType,
}

/// Comprehensive error types for audio system
#[derive(Debug, Clone)]
pub enum AudioError {
    DeviceDisconnected,
    StreamFailed,
    ProcessingFailed,
    TranscriptionFailed,
    ChannelClosed,
    InitializationFailed,
    ConfigurationError,
    PermissionDenied,
    BufferOverflow,
    SampleRateUnsupported,
}

impl AudioError {
    /// Check if error is recoverable (can attempt reconnection)
    pub fn is_recoverable(&self) -> bool {
        match self {
            // Device disconnect is now recoverable - we can attempt reconnection
            AudioError::DeviceDisconnected => true,
            AudioError::StreamFailed => true,
            AudioError::ProcessingFailed => true,
            AudioError::TranscriptionFailed => true,
            AudioError::ChannelClosed => false,
            AudioError::InitializationFailed => false,
            AudioError::ConfigurationError => false,
            AudioError::PermissionDenied => false,
            AudioError::BufferOverflow => true,
            AudioError::SampleRateUnsupported => false,
        }
    }

    /// Get user-friendly error message
    pub fn user_message(&self) -> &'static str {
        match self {
            AudioError::DeviceDisconnected => "Audio device was disconnected",
            AudioError::StreamFailed => "Audio stream encountered an error",
            AudioError::ProcessingFailed => "Audio processing failed",
            AudioError::TranscriptionFailed => "Speech transcription failed",
            AudioError::ChannelClosed => "Audio channel was closed unexpectedly",
            AudioError::InitializationFailed => "Failed to initialize audio system",
            AudioError::ConfigurationError => "Audio configuration error",
            AudioError::PermissionDenied => "Microphone permission denied",
            AudioError::BufferOverflow => "Audio buffer overflow",
            AudioError::SampleRateUnsupported => "Audio sample rate not supported",
        }
    }
}

/// Recording statistics
#[derive(Debug, Default)]
pub struct RecordingStats {
    pub chunks_processed: u64,
    pub total_duration: f64,
    pub last_activity: Option<Instant>,
}

/// Unified state management for audio recording
pub struct RecordingState {
    // Core recording state
    is_recording: AtomicBool,
    is_paused: AtomicBool,

    // Audio devices
    microphone_device: Mutex<Option<Arc<AudioDevice>>>,
    system_device: Mutex<Option<Arc<AudioDevice>>>,
    stopped_device_names: Mutex<(Option<String>, Option<String>)>,

    // Audio pipeline
    audio_sender: Mutex<Option<mpsc::UnboundedSender<AudioChunk>>>,

    // Memory optimization
    buffer_pool: AudioBufferPool,

    // Error handling
    error_count: AtomicU32,
    recoverable_error_count: AtomicU32,
    last_error: Mutex<Option<AudioError>>,
    error_callback: Mutex<Option<Box<dyn Fn(&AudioError) + Send + Sync>>>,

    // Statistics
    stats: Mutex<RecordingStats>,

    // Recording start time for accurate timestamps
    recording_start: Mutex<Option<Instant>>,
    recording_end: Mutex<Option<Instant>>,
    // Pause time tracking
    pause_start: Mutex<Option<Instant>>,
    total_pause_duration: Mutex<std::time::Duration>,
}

impl RecordingState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            is_recording: AtomicBool::new(false),
            is_paused: AtomicBool::new(false),
            microphone_device: Mutex::new(None),
            system_device: Mutex::new(None),
            stopped_device_names: Mutex::new((None, None)),
            audio_sender: Mutex::new(None),
            buffer_pool: AudioBufferPool::new(16, 48000), // Pool of 16 buffers with 48kHz samples capacity
            error_count: AtomicU32::new(0),
            recoverable_error_count: AtomicU32::new(0),
            last_error: Mutex::new(None),
            error_callback: Mutex::new(None),
            stats: Mutex::new(RecordingStats::default()),
            recording_start: Mutex::new(None),
            recording_end: Mutex::new(None),
            pause_start: Mutex::new(None),
            total_pause_duration: Mutex::new(std::time::Duration::ZERO),
        })
    }

    // Recording control
    pub fn start_recording(&self) -> Result<()> {
        let mut start = self.recording_start.lock().unwrap();
        *self.recording_end.lock().unwrap() = None;
        *self.stopped_device_names.lock().unwrap() = (None, None);
        *self.pause_start.lock().unwrap() = None;
        *self.total_pause_duration.lock().unwrap() = std::time::Duration::ZERO;
        *start = Some(Instant::now());
        self.is_paused.store(false, Ordering::SeqCst);
        self.is_recording.store(true, Ordering::SeqCst);
        self.error_count.store(0, Ordering::SeqCst);
        self.recoverable_error_count.store(0, Ordering::SeqCst);
        *self.last_error.lock().unwrap() = None;
        Ok(())
    }

    pub fn stop_recording(&self) {
        // Use the start lock to serialize timing changes with start and pause.
        let _start = self.recording_start.lock().unwrap();
        if self.is_recording.swap(false, Ordering::SeqCst) {
            let end = Instant::now();
            *self.recording_end.lock().unwrap() = Some(end);
            // Retain text for analytics while releasing device references below.
            *self.stopped_device_names.lock().unwrap() = (
                self.get_microphone_device().map(|device| device.name.clone()),
                self.get_system_device().map(|device| device.name.clone()),
            );
            if let Some(pause_start) = self.pause_start.lock().unwrap().take() {
                *self.total_pause_duration.lock().unwrap() += end.saturating_duration_since(pause_start);
            }
        }
        self.is_paused.store(false, Ordering::SeqCst);
        // The audio sender stays open so captures can drain their final audio.
        // AudioPipelineManager::stop() closes it after the streams stop.
        // CRITICAL: Clear device references to release microphone/speaker
        // Without this, Arc<AudioDevice> references persist and keep the mic active
        *self.microphone_device.lock().unwrap() = None;
        *self.system_device.lock().unwrap() = None;
        log::info!("Recording stopped, device references cleared");
    }

    pub fn pause_recording(&self) -> Result<()> {
        let _start = self.recording_start.lock().unwrap();
        if !self.is_recording() {
            return Err(anyhow::anyhow!("Cannot pause when not recording"));
        }
        if self.is_paused() {
            return Err(anyhow::anyhow!("Recording is already paused"));
        }

        self.is_paused.store(true, Ordering::SeqCst);
        *self.pause_start.lock().unwrap() = Some(Instant::now());
        log::info!("Recording paused");
        Ok(())
    }

    pub fn resume_recording(&self) -> Result<()> {
        let _start = self.recording_start.lock().unwrap();
        if !self.is_recording() {
            return Err(anyhow::anyhow!("Cannot resume when not recording"));
        }
        if !self.is_paused() {
            return Err(anyhow::anyhow!("Recording is not paused"));
        }

        // Calculate pause duration and add to total
        if let Some(pause_start) = self.pause_start.lock().unwrap().take() {
            let pause_duration = pause_start.elapsed();
            *self.total_pause_duration.lock().unwrap() += pause_duration;
            log::info!(
                "Recording resumed after pause of {:.2}s",
                pause_duration.as_secs_f64()
            );
        }

        self.is_paused.store(false, Ordering::SeqCst);
        Ok(())
    }

    pub fn is_recording(&self) -> bool {
        self.is_recording.load(Ordering::SeqCst)
    }

    pub fn is_paused(&self) -> bool {
        self.is_paused.load(Ordering::SeqCst)
    }

    pub fn is_active(&self) -> bool {
        self.is_recording() && !self.is_paused()
    }

    // Device management
    pub fn set_microphone_device(&self, device: Arc<AudioDevice>) {
        *self.microphone_device.lock().unwrap() = Some(device);
    }

    pub fn set_system_device(&self, device: Arc<AudioDevice>) {
        *self.system_device.lock().unwrap() = Some(device);
    }

    pub fn get_microphone_device(&self) -> Option<Arc<AudioDevice>> {
        self.microphone_device.lock().unwrap().clone()
    }

    pub fn get_system_device(&self) -> Option<Arc<AudioDevice>> {
        self.system_device.lock().unwrap().clone()
    }

    /// Device names remain available after an error stops capture.
    pub fn get_device_names(&self) -> (Option<String>, Option<String>) {
        // Wait for any concurrent stop to finish storing its snapshot.
        let _start = self.recording_start.lock().unwrap();
        if self.is_recording() {
            (
                self.get_microphone_device().map(|device| device.name.clone()),
                self.get_system_device().map(|device| device.name.clone()),
            )
        } else {
            self.stopped_device_names.lock().unwrap().clone()
        }
    }

    // Audio pipeline management
    pub fn set_audio_sender(&self, sender: mpsc::UnboundedSender<AudioChunk>) {
        *self.audio_sender.lock().unwrap() = Some(sender);
    }

    /// Close the pipeline channel. Captures still holding this state cannot reopen it.
    pub(super) fn close_audio_sender(&self) {
        *self.audio_sender.lock().unwrap() = None;
    }

    /// Send a chunk that capture already accepted. Pause does not drop it.
    pub(super) fn send_captured_chunk(&self, chunk: AudioChunk) -> Result<()> {
        let mut sender = self.audio_sender.lock().unwrap();
        let Some(channel) = sender.as_ref() else {
            // Return an error when no sender is available (pipeline not ready or closed)
            return Err(anyhow::anyhow!(
                "Audio pipeline not ready - no sender available"
            ));
        };
        if channel.send(chunk).is_err() {
            // The receiver is gone. Later chunks have nowhere to go, so report once.
            *sender = None;
            return Err(anyhow::anyhow!("Failed to send audio chunk: channel closed"));
        }
        drop(sender);

        // Update statistics
        let mut stats = self.stats.lock().unwrap();
        stats.chunks_processed += 1;
        stats.last_activity = Some(Instant::now());
        Ok(())
    }

    // Error handling
    pub fn set_error_callback<F>(&self, callback: F)
    where
        F: Fn(&AudioError) + Send + Sync + 'static,
    {
        *self.error_callback.lock().unwrap() = Some(Box::new(callback));
    }

    pub fn report_error(&self, error: AudioError) {
        let count = self.error_count.fetch_add(1, Ordering::SeqCst) + 1;

        // Track recoverable vs non-recoverable errors separately
        if error.is_recoverable() {
            let recoverable_count = self.recoverable_error_count.fetch_add(1, Ordering::SeqCst) + 1;
            log::warn!(
                "Recoverable audio error ({}): {:?}",
                recoverable_count,
                error
            );

            // Allow more recoverable errors before stopping
            if recoverable_count >= 10 {
                log::error!(
                    "Too many recoverable errors ({}), stopping recording",
                    recoverable_count
                );
                self.stop_recording();
            }
        } else {
            log::error!("Non-recoverable audio error: {:?}", error);
            // Stop immediately for non-recoverable errors
            self.stop_recording();
        }

        *self.last_error.lock().unwrap() = Some(error.clone());

        // Call error callback if set
        if let Some(callback) = self.error_callback.lock().unwrap().as_ref() {
            callback(&error);
        }

        // Fallback: stop recording after too many total errors
        if count >= 15 {
            log::error!(
                "Too many total audio errors ({}), stopping recording",
                count
            );
            self.stop_recording();
        }
    }

    pub fn get_error_count(&self) -> u32 {
        self.error_count.load(Ordering::SeqCst)
    }

    pub fn get_recoverable_error_count(&self) -> u32 {
        self.recoverable_error_count.load(Ordering::SeqCst)
    }

    pub fn get_last_error(&self) -> Option<AudioError> {
        self.last_error.lock().unwrap().clone()
    }

    pub fn has_fatal_error(&self) -> bool {
        if let Some(error) = &*self.last_error.lock().unwrap() {
            !error.is_recoverable() && self.error_count.load(Ordering::SeqCst) > 0
        } else {
            false
        }
    }

    // Statistics
    pub fn get_stats(&self) -> RecordingStats {
        self.stats.lock().unwrap().clone()
    }

    pub fn get_recording_duration(&self) -> Option<f64> {
        self.recording_start
            .lock()
            .unwrap()
            .map(|start| self.recording_end.lock().unwrap()
                .unwrap_or_else(Instant::now).saturating_duration_since(start).as_secs_f64())
    }

    pub fn get_active_recording_duration(&self) -> Option<f64> {
        self.get_recording_duration().map(|total_duration| {
            let pause_duration = self.get_total_pause_duration();
            let current_pause = if self.is_paused() {
                self.pause_start
                    .lock()
                    .unwrap()
                    .map(|p| p.elapsed().as_secs_f64())
                    .unwrap_or(0.0)
            } else {
                0.0
            };
            (total_duration - pause_duration - current_pause).max(0.0)
        })
    }

    pub fn get_total_pause_duration(&self) -> f64 {
        self.total_pause_duration.lock().unwrap().as_secs_f64()
    }

    pub fn get_current_pause_duration(&self) -> Option<f64> {
        if self.is_paused() {
            self.pause_start
                .lock()
                .unwrap()
                .map(|start| start.elapsed().as_secs_f64())
        } else {
            None
        }
    }

    // Memory management
    pub fn get_buffer_pool(&self) -> AudioBufferPool {
        self.buffer_pool.clone()
    }

    // Cleanup
    pub fn cleanup(&self) {
        self.stop_recording();
        *self.microphone_device.lock().unwrap() = None;
        *self.system_device.lock().unwrap() = None;
        *self.audio_sender.lock().unwrap() = None;
        *self.last_error.lock().unwrap() = None;
        *self.error_callback.lock().unwrap() = None;
        *self.stats.lock().unwrap() = RecordingStats::default();
        *self.recording_start.lock().unwrap() = None;
        *self.recording_end.lock().unwrap() = None;
        *self.stopped_device_names.lock().unwrap() = (None, None);
        *self.pause_start.lock().unwrap() = None;
        *self.total_pause_duration.lock().unwrap() = std::time::Duration::ZERO;
        self.error_count.store(0, Ordering::SeqCst);
        self.recoverable_error_count.store(0, Ordering::SeqCst);

        // Clear buffer pool to free memory
        self.buffer_pool.clear();
    }
}

impl Default for RecordingState {
    fn default() -> Self {
        Self {
            is_recording: AtomicBool::new(false),
            is_paused: AtomicBool::new(false),
            microphone_device: Mutex::new(None),
            system_device: Mutex::new(None),
            stopped_device_names: Mutex::new((None, None)),
            audio_sender: Mutex::new(None),
            buffer_pool: AudioBufferPool::new(16, 48000), // Pool of 16 buffers with 48kHz samples capacity
            error_count: AtomicU32::new(0),
            recoverable_error_count: AtomicU32::new(0),
            last_error: Mutex::new(None),
            error_callback: Mutex::new(None),
            stats: Mutex::new(RecordingStats::default()),
            recording_start: Mutex::new(None),
            recording_end: Mutex::new(None),
            pause_start: Mutex::new(None),
            total_pause_duration: Mutex::new(std::time::Duration::ZERO),
        }
    }
}

// Thread-safe cloning for RecordingStats
impl Clone for RecordingStats {
    fn clone(&self) -> Self {
        Self {
            chunks_processed: self.chunks_processed,
            total_duration: self.total_duration,
            last_activity: self.last_activity,
        }
    }
}

#[cfg(test)]
mod timing_tests {
    use super::*;

    #[test]
    fn stopped_device_names_follow_device_changes_and_reset_with_the_session() {
        use super::super::devices::DeviceType;
        let state = RecordingState::new();
        state.start_recording().unwrap();
        state.set_microphone_device(Arc::new(AudioDevice::new("Original microphone".into(), DeviceType::Input)));
        state.set_microphone_device(Arc::new(AudioDevice::new("Replacement microphone".into(), DeviceType::Input)));
        state.set_system_device(Arc::new(AudioDevice::new("Speakers".into(), DeviceType::Output)));
        let names = (Some("Replacement microphone".into()), Some("Speakers".into()));
        assert_eq!(state.get_device_names(), names);
        state.stop_recording();
        state.stop_recording();
        assert_eq!(state.get_device_names(), names);
        state.start_recording().unwrap();
        assert_eq!(state.get_device_names(), (None, None));
        state.set_microphone_device(Arc::new(AudioDevice::new("New microphone".into(), DeviceType::Input)));
        state.stop_recording();
        state.cleanup();
        assert_eq!(state.get_device_names(), (None, None));
    }

    #[test]
    fn stopped_timing_does_not_grow_during_teardown_or_a_repeated_stop() {
        let state = RecordingState::new();
        state.start_recording().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        state.stop_recording();
        let duration = state.get_recording_duration().unwrap();
        let active = state.get_active_recording_duration();
        assert!(duration >= 0.02);
        std::thread::sleep(std::time::Duration::from_millis(30));
        state.stop_recording();
        assert_eq!(state.get_recording_duration(), Some(duration));
        assert_eq!(state.get_active_recording_duration(), active);
    }

    #[test]
    fn a_new_recording_resets_stopped_timing_and_pause_totals() {
        let state = RecordingState::new();
        state.start_recording().unwrap();
        state.pause_recording().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        state.stop_recording();
        assert!(state.get_total_pause_duration() >= 0.02);
        state.start_recording().unwrap();
        assert_eq!(state.get_total_pause_duration(), 0.0);
        assert!(!state.is_paused());
        assert!(state.get_recording_duration().unwrap() < 0.02);
    }
}
