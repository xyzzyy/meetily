use std::sync::Arc;
use anyhow::Result;
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{Device, Stream, SupportedStreamConfig};
use log::{error, info, warn};
use tokio::sync::mpsc;

use super::devices::{AudioDevice, get_device_and_config};
use super::pipeline::{lock_unpoisoned, AudioCapture};
use super::recording_state::{RecordingState, DeviceType};
use super::capture::{AudioCaptureBackend, get_current_backend};

#[cfg(target_os = "macos")]
use super::capture::CoreAudioCapture;

/// Stream backend implementation
pub enum StreamBackend {
    /// CPAL-based stream (ScreenCaptureKit or default)
    Cpal(Stream),
    /// Core Audio direct implementation (macOS only)
    #[cfg(target_os = "macos")]
    CoreAudio {
        task: Option<tokio::task::JoinHandle<()>>,
        stream: Arc<std::sync::Mutex<super::capture::core_audio::CoreAudioStream>>,
    },
}

// SAFETY: While Stream doesn't implement Send, we ensure it's only accessed
// from the same thread context by using spawn_blocking for operations that cross thread boundaries
unsafe impl Send for StreamBackend {}

/// Simplified audio stream wrapper with multi-backend support
pub struct AudioStream {
    device: Arc<AudioDevice>,
    backend: StreamBackend,
    capture: AudioCapture,
}

// SAFETY: AudioStream contains StreamBackend which we've marked as Send
unsafe impl Send for AudioStream {}

impl AudioStream {
    /// Create a new audio stream for the given device
    pub async fn create(
        device: Arc<AudioDevice>,
        state: Arc<RecordingState>,
        device_type: DeviceType,
        recording_sender: Option<mpsc::UnboundedSender<super::recording_state::AudioChunk>>,
    ) -> Result<Self> {
        // Get current backend from global config
        let backend_type = get_current_backend();
        Self::create_with_backend(device, state, device_type, recording_sender, backend_type).await
    }

    /// Create a new audio stream with explicit backend selection
    pub async fn create_with_backend(
        device: Arc<AudioDevice>,
        state: Arc<RecordingState>,
        device_type: DeviceType,
        recording_sender: Option<mpsc::UnboundedSender<super::recording_state::AudioChunk>>,
        backend_type: AudioCaptureBackend,
    ) -> Result<Self> {
        info!("🎵 Stream: Creating audio stream for device: {} with backend: {:?}, device_type: {:?}",
              device.name, backend_type, device_type);

        // For system audio devices, use the selected backend
        // For microphone devices, always use CPAL
        #[cfg(target_os = "macos")]
        let use_core_audio = device_type == DeviceType::System
            && backend_type == AudioCaptureBackend::CoreAudio;

        #[cfg(not(target_os = "macos"))]
        let use_core_audio = false;

        #[cfg(target_os = "macos")]
        info!("🎵 Stream: use_core_audio = {}, device_type == System: {}, backend == CoreAudio: {}",
              use_core_audio,
              device_type == DeviceType::System,
              backend_type == AudioCaptureBackend::CoreAudio);

        #[cfg(not(target_os = "macos"))]
        info!("🎵 Stream: use_core_audio = {}, device_type == System: {}",
              use_core_audio,
              device_type == DeviceType::System);

        #[cfg(target_os = "macos")]
        if use_core_audio {
            info!("🎵 Stream: Using Core Audio backend (cidre) for system audio");
            return Self::create_core_audio_stream(device, state, device_type, recording_sender).await;
        }

        // Default path: use CPAL
        #[cfg(target_os = "macos")]
        let backend_name = if backend_type == AudioCaptureBackend::ScreenCaptureKit {
            "ScreenCaptureKit"
        } else {
            "CPAL (default)"
        };

        #[cfg(not(target_os = "macos"))]
        let backend_name = "CPAL";

        info!("🎵 Stream: Using CPAL backend ({}) for device: {}", backend_name, device.name);
        Self::create_cpal_stream(device, state, device_type, recording_sender).await
    }

    /// Create a CPAL-based stream (ScreenCaptureKit on macOS)
    async fn create_cpal_stream(
        device: Arc<AudioDevice>,
        state: Arc<RecordingState>,
        device_type: DeviceType,
        recording_sender: Option<mpsc::UnboundedSender<super::recording_state::AudioChunk>>,
    ) -> Result<Self> {
        info!("Creating CPAL stream for device: {}", device.name);

        // Get the underlying cpal device and config
        let (cpal_device, config) = get_device_and_config(&device).await?;

        info!("Audio config - Sample rate: {}, Channels: {}, Format: {:?}",
              config.sample_rate().0, config.channels(), config.sample_format());

        // Create audio capture processor
        let capture = AudioCapture::new(
            device.clone(),
            state.clone(),
            config.sample_rate().0,
            config.channels(),
            device_type,
            recording_sender,
        );

        // Build the appropriate stream based on sample format
        let stream = Self::build_stream(&cpal_device, &config, capture.clone())?;

        // Start the stream
        stream.play()?;
        info!("CPAL stream started for device: {}", device.name);

        Ok(Self {
            device,
            backend: StreamBackend::Cpal(stream),
            capture,
        })
    }

    /// Create a Core Audio stream (macOS only)
    #[cfg(target_os = "macos")]
    async fn create_core_audio_stream(
        device: Arc<AudioDevice>,
        state: Arc<RecordingState>,
        device_type: DeviceType,
        recording_sender: Option<mpsc::UnboundedSender<super::recording_state::AudioChunk>>,
    ) -> Result<Self> {
        info!("🔊 Stream: Creating Core Audio stream for device: {}", device.name);

        // Create Core Audio capture
        info!("🔊 Stream: Calling CoreAudioCapture::new()...");
        let capture_impl = CoreAudioCapture::new()
            .map_err(|e| {
                error!("❌ Stream: CoreAudioCapture::new() failed: {}", e);
                anyhow::anyhow!("Failed to create Core Audio capture: {}", e)
            })?;

        info!("✅ Stream: CoreAudioCapture created, calling stream()...");
        let core_stream = capture_impl.stream_for_recording(state.clone())
            .map_err(|e| {
                error!("❌ Stream: capture_impl.stream() failed: {}", e);
                anyhow::anyhow!("Failed to create Core Audio stream: {}", e)
            })?;

        let sample_rate = core_stream.sample_rate();
        let core_stream = Arc::new(std::sync::Mutex::new(core_stream));
        info!("✅ Stream: Core Audio stream created with sample rate: {} Hz", sample_rate);

        // Create audio capture processor for pipeline integration
        // CRITICAL: Core Audio tap is MONO (with_mono_global_tap_excluding_processes)
        let capture = AudioCapture::new(
            device.clone(),
            state.clone(),
            sample_rate,
            1, // Core Audio tap is MONO (not stereo!)
            device_type,
            recording_sender,
        );

        // Spawn task to process Core Audio stream samples
        // The stream needs to be polled continuously to produce samples
        let device_name = device.name.clone();
        info!("🔊 Stream: Spawning tokio task to poll Core Audio stream...");
        let task = tokio::spawn({
            let capture = capture.clone();
            let stream = core_stream.clone();

            async move {
                use futures_util::StreamExt;

                info!("Core Audio processing task started for {}", device_name);
                const MAX_BATCH: usize = 4096;
                let mut batch = Vec::with_capacity(MAX_BATCH);
                loop {
                    let ended = futures_util::future::poll_fn(|cx| {
                        // Keep poll and delivery under one lock so stop cannot overtake a sample.
                        let mut stream = lock_unpoisoned(&stream);
                        batch.clear();
                        let mut ended = false;
                        while batch.len() < MAX_BATCH {
                            match stream.poll_next_unpin(cx) {
                                std::task::Poll::Ready(Some(sample)) => batch.push(sample),
                                std::task::Poll::Ready(None) => {
                                    ended = true;
                                    break;
                                }
                                std::task::Poll::Pending => break,
                            }
                        }
                        if batch.is_empty() && !ended {
                            return std::task::Poll::Pending;
                        }
                        capture.process_captured_samples(&batch);
                        std::task::Poll::Ready(ended)
                    })
                    .await;
                    if ended {
                        break;
                    }
                }
                if let Err(e) = capture.finish() {
                    error!("Failed to drain Core Audio capture: {}", e);
                }

                info!("⚠️ Stream: Core Audio processing task ended for {}", device_name);
            }
        });

        info!("✅ Stream: Core Audio stream fully initialized for device: {}", device.name);

        Ok(Self {
            device: device.clone(),
            backend: StreamBackend::CoreAudio {
                task: Some(task),
                stream: core_stream,
            },
            capture,
        })
    }

    /// Build stream based on sample format
    fn build_stream(
        device: &Device,
        config: &SupportedStreamConfig,
        capture: AudioCapture,
    ) -> Result<Stream> {
        let config_copy = config.clone();

        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => {
                let capture_clone = capture.clone();
                device.build_input_stream(
                    &config_copy.into(),
                    move |data: &[f32], _: &cpal::InputCallbackInfo| {
                        capture.process_audio_data(data);
                    },
                    move |err| {
                        capture_clone.handle_stream_error(err);
                    },
                    None,
                )?
            }
            cpal::SampleFormat::I16 => {
                let capture_clone = capture.clone();
                device.build_input_stream(
                    &config_copy.into(),
                    move |data: &[i16], _: &cpal::InputCallbackInfo| {
                        let f32_data: Vec<f32> = data.iter()
                            .map(|&sample| sample as f32 / i16::MAX as f32)
                            .collect();
                        capture.process_audio_data(&f32_data);
                    },
                    move |err| {
                        capture_clone.handle_stream_error(err);
                    },
                    None,
                )?
            }
            cpal::SampleFormat::I32 => {
                let capture_clone = capture.clone();
                device.build_input_stream(
                    &config_copy.into(),
                    move |data: &[i32], _: &cpal::InputCallbackInfo| {
                        let f32_data: Vec<f32> = data.iter()
                            .map(|&sample| sample as f32 / i32::MAX as f32)
                            .collect();
                        capture.process_audio_data(&f32_data);
                    },
                    move |err| {
                        capture_clone.handle_stream_error(err);
                    },
                    None,
                )?
            }
            cpal::SampleFormat::I8 => {
                let capture_clone = capture.clone();
                device.build_input_stream(
                    &config_copy.into(),
                    move |data: &[i8], _: &cpal::InputCallbackInfo| {
                        let f32_data: Vec<f32> = data.iter()
                            .map(|&sample| sample as f32 / i8::MAX as f32)
                            .collect();
                        capture.process_audio_data(&f32_data);
                    },
                    move |err| {
                        capture_clone.handle_stream_error(err);
                    },
                    None,
                )?
            }
            _ => {
                return Err(anyhow::anyhow!("Unsupported sample format: {:?}", config.sample_format()));
            }
        };

        Ok(stream)
    }

    /// Get device info
    pub fn device(&self) -> &AudioDevice {
        &self.device
    }

    /// Stop the stream
    pub fn stop(self) -> Result<()> {
        info!("Stopping audio stream for device: {}", self.device.name);

        let drain_result = match self.backend {
            StreamBackend::Cpal(stream) => finish_cpal_capture(&self.capture, || {
                if let Err(e) = stream.pause() {
                    warn!("Failed to pause stream before drop: {}", e);
                }
                info!("Stream paused, now dropping to release callbacks");
                drop(stream);
            }),
            #[cfg(target_os = "macos")]
            StreamBackend::CoreAudio { task, stream } => {
                // Stop owns the shared stream, so cancelling the task does not discard its queue.
                if let Some(task_handle) = task {
                    task_handle.abort();
                }
                let mut stream = lock_unpoisoned(&stream);
                let samples = stream.stop_and_drain();
                self.capture.process_captured_samples(&samples);
                // Finish before releasing the lock, even if the cancelled task is still running.
                self.capture.finish()
            }
        };

        // Explicitly drop self.device Arc reference
        drop(self.device);
        info!("Audio stream stopped and device reference dropped");
        drain_result
    }
}

fn finish_cpal_capture(capture: &AudioCapture, teardown: impl FnOnce()) -> Result<()> {
    // Finish waits for the current callback and prevents later callbacks from sending.
    // Pause or drop can stall on a disconnected device. Send the tail before that
    // stall lets session stop close the pipeline sender.
    let drain_result = capture.finish();
    teardown();
    drain_result
}

/// Audio stream manager for handling multiple streams
pub struct AudioStreamManager {
    microphone_stream: Option<AudioStream>,
    system_stream: Option<AudioStream>,
    state: Arc<RecordingState>,
}

// SAFETY: AudioStreamManager contains AudioStream which we've marked as Send
unsafe impl Send for AudioStreamManager {}

impl AudioStreamManager {
    pub fn new(state: Arc<RecordingState>) -> Self {
        Self {
            microphone_stream: None,
            system_stream: None,
            state,
        }
    }

    /// Start audio streams for the given devices
    pub async fn start_streams(
        &mut self,
        microphone_device: Option<Arc<AudioDevice>>,
        system_device: Option<Arc<AudioDevice>>,
        recording_sender: Option<mpsc::UnboundedSender<super::recording_state::AudioChunk>>,
    ) -> Result<()> {
        use super::capture::get_current_backend;
        let backend = get_current_backend();
        info!("🎙️ Starting audio streams with backend: {:?}", backend);

        // Start microphone stream
        if let Some(mic_device) = microphone_device {
            info!("🎤 Creating microphone stream: {} (always uses CPAL)", mic_device.name);
            match AudioStream::create(mic_device.clone(), self.state.clone(), DeviceType::Microphone, recording_sender.clone()).await {
                Ok(stream) => {
                    self.state.set_microphone_device(mic_device);
                    self.microphone_stream = Some(stream);
                    info!("✅ Microphone stream created successfully");
                }
                Err(e) => {
                    error!("❌ Failed to create microphone stream: {}", e);
                    return Err(e);
                }
            }
        } else {
            info!("ℹ️ No microphone device specified, skipping microphone stream");
        }

        // Start system audio stream
        if let Some(sys_device) = system_device {
            info!("🔊 Creating system audio stream: {} (backend: {:?})", sys_device.name, backend);
            match AudioStream::create(sys_device.clone(), self.state.clone(), DeviceType::System, recording_sender.clone()).await {
                Ok(stream) => {
                    self.state.set_system_device(sys_device);
                    self.system_stream = Some(stream);
                    info!("✅ System audio stream created with {:?} backend", backend);
                }
                Err(e) => {
                    warn!("⚠️ Failed to create system audio stream: {}", e);
                    // Don't fail if only system audio fails
                }
            }
        } else {
            info!("ℹ️ No system device specified, skipping system audio stream");
        }

        // Ensure at least one stream was created
        if self.microphone_stream.is_none() && self.system_stream.is_none() {
            return Err(anyhow::anyhow!("No audio streams could be created"));
        }

        Ok(())
    }

    /// Stop all audio streams
    pub fn stop_streams(&mut self) -> Result<()> {
        info!("Stopping all audio streams");

        let mut errors = Vec::new();

        // Stop microphone stream
        if let Some(mic_stream) = self.microphone_stream.take() {
            if let Err(e) = mic_stream.stop() {
                error!("Failed to stop microphone stream: {}", e);
                errors.push(e);
            }
        }

        // Stop system stream
        if let Some(sys_stream) = self.system_stream.take() {
            if let Err(e) = sys_stream.stop() {
                error!("Failed to stop system stream: {}", e);
                errors.push(e);
            }
        }

        if !errors.is_empty() {
            Err(anyhow::anyhow!("Failed to stop some streams: {:?}", errors))
        } else {
            info!("All audio streams stopped successfully");
            Ok(())
        }
    }

    /// Take the microphone stream OUT of the manager, keeping system audio
    /// running. The caller stops/drops it OUTSIDE any lock — a cpal teardown
    /// of a dead BT device can stall and must not block a held mutex.
    pub fn take_mic_stream(&mut self) -> Option<AudioStream> {
        self.microphone_stream.take()
    }

    /// Set a new microphone stream (used after hot-swap creation).
    pub fn set_mic_stream(&mut self, stream: AudioStream) {
        self.microphone_stream = Some(stream);
    }

    /// Get stream count
    pub fn active_stream_count(&self) -> usize {
        let mut count = 0;
        if self.microphone_stream.is_some() {
            count += 1;
        }
        if self.system_stream.is_some() {
            count += 1;
        }
        count
    }

    /// Check if any streams are active
    pub fn has_active_streams(&self) -> bool {
        self.microphone_stream.is_some() || self.system_stream.is_some()
    }
}

impl Drop for AudioStreamManager {
    fn drop(&mut self) {
        if let Err(e) = self.stop_streams() {
            error!("Error stopping streams during drop: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::device_detection::InputDeviceKind;
    use super::super::pipeline::AudioPipelineManager;
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn cpal_tail_reaches_recording_while_native_teardown_is_blocked() {
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
                "Test microphone".into(),
                super::super::devices::DeviceType::Input,
            )),
            state.clone(),
            16000,
            1,
            DeviceType::Microphone,
            None,
        );
        // Less than one resampler block: all 384 output samples need the final drain.
        capture.process_audio_data(&[0.25; 128]);
        let callback_capture = capture.clone();
        let (entered_sender, entered_receiver) = tokio::sync::oneshot::channel();
        let (release_sender, release_receiver) = std::sync::mpsc::channel();
        // A swapped-out microphone stops outside the stream manager. Its native
        // teardown can remain blocked while the rest of the session stops.
        let stopping = std::thread::spawn(move || {
            let mut released = false;
            let result = finish_cpal_capture(&capture, || {
                let _ = entered_sender.send(());
                released = release_receiver
                    .recv_timeout(Duration::from_secs(15))
                    .is_ok();
            });
            (result, released)
        });

        let entered = tokio::time::timeout(Duration::from_secs(5), entered_receiver).await;
        callback_capture.process_audio_data(&[0.75; 128]);
        state.stop_recording();
        let stopped = tokio::time::timeout(Duration::from_secs(5), manager.stop()).await;
        let saved: Vec<f32> = std::iter::from_fn(|| recording.try_recv().ok())
            .flat_map(|chunk| chunk.data)
            .collect();
        let teardown_still_blocked = !stopping.is_finished();

        // Release and join before assertions, including on a timeout or stop error.
        let _ = release_sender.send(());
        let teardown = stopping.join();
        entered.expect("Native teardown was not reached").unwrap();
        stopped
            .expect("Session stop waited for native teardown")
            .unwrap();
        let (drained, released) = teardown.unwrap();
        drained.unwrap();
        assert!(released, "Native teardown did not receive its release signal");
        assert!(teardown_still_blocked);
        assert_eq!(saved.len(), 384);
        assert!(saved.iter().any(|sample| sample.abs() > 0.001));
    }
}
