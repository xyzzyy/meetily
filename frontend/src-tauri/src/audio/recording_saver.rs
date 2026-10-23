use std::sync::{Arc, Mutex};
use tokio::sync::Mutex as AsyncMutex;
use anyhow::{Context, Result};
use log::{info, warn, error};
use tauri::{AppHandle, Runtime, Emitter};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use serde::{Serialize, Deserialize};
use std::path::PathBuf;

use super::recording_state::AudioChunk;
use super::audio_processing::create_meeting_folder;
use super::incremental_saver::IncrementalAudioSaver;

/// Structured transcript segment for JSON export
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TranscriptSegment {
    pub id: String,
    pub text: String,
    pub audio_start_time: f64, // Seconds from recording start
    pub audio_end_time: f64,   // Seconds from recording start
    pub duration: f64,          // Segment duration in seconds
    pub display_time: String,   // Formatted time for display like "[02:15]"
    pub confidence: f32,
    pub sequence_id: u64,
    pub speaker_id: Option<String>,
    pub speaker_label: Option<String>,
    pub speaker_profile_id: Option<String>,
    pub speaker_source: Option<String>,
    pub speaker_color: Option<String>,
    pub is_overlap: Option<bool>,
    pub candidate_speaker_ids: Option<Vec<String>>,
    pub diarization_status: Option<String>,
    pub diarization_method: Option<String>,
    pub diarization_confidence: Option<f32>,
}

impl From<super::transcription::TranscriptUpdate> for TranscriptSegment {
    fn from(update: super::transcription::TranscriptUpdate) -> Self {
        Self {
            id: format!("seg_{}", update.sequence_id),
            text: update.text,
            audio_start_time: update.audio_start_time,
            audio_end_time: update.audio_end_time,
            duration: update.duration,
            display_time: update.timestamp,
            confidence: update.confidence,
            sequence_id: update.sequence_id,
            speaker_id: update.speaker_id,
            speaker_label: update.speaker_label,
            speaker_profile_id: update.speaker_profile_id,
            speaker_source: update.speaker_source,
            speaker_color: update.speaker_color,
            is_overlap: update.is_overlap,
            candidate_speaker_ids: update.candidate_speaker_ids,
            diarization_status: update.diarization_status,
            diarization_method: update.diarization_method,
            diarization_confidence: update.diarization_confidence,
        }
    }
}

/// Meeting metadata structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingMetadata {
    pub version: String,
    pub meeting_id: Option<String>,
    pub meeting_name: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub duration_seconds: Option<f64>,
    pub devices: DeviceInfo,
    pub audio_file: String,
    pub transcript_file: String,
    pub sample_rate: u32,
    pub status: String,  // "recording", "completed", "error"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub microphone: Option<String>,
    pub system_audio: Option<String>,
}

/// New recording saver using incremental saving strategy
pub struct RecordingSaver {
    incremental_saver: Option<Arc<AsyncMutex<IncrementalAudioSaver>>>,
    meeting_folder: Option<PathBuf>,
    meeting_name: Option<String>,
    metadata: Option<MeetingMetadata>,
    transcript_segments: Arc<Mutex<Vec<TranscriptSegment>>>,
    accumulation_task: Option<JoinHandle<Result<()>>>,
    accumulation_stop: Option<oneshot::Sender<()>>,
}

impl RecordingSaver {
    /// Create an idle saver with no recording folder or active worker.
    pub fn new() -> Self {
        Self {
            incremental_saver: None,
            meeting_folder: None,
            meeting_name: None,
            metadata: None,
            transcript_segments: Arc::new(Mutex::new(Vec::new())),
            accumulation_task: None,
            accumulation_stop: None,
        }
    }

    /// Set the meeting name for this recording session
    pub fn set_meeting_name(&mut self, name: Option<String>) {
        self.meeting_name = name;
    }

    /// Set device information in metadata
    pub fn set_device_info(&mut self, mic_name: Option<String>, sys_name: Option<String>) {
        if let Some(ref mut metadata) = self.metadata {
            metadata.devices.microphone = mic_name;
            metadata.devices.system_audio = sys_name;

            // Write updated metadata to disk if folder exists
            if let Some(folder) = &self.meeting_folder {
                let metadata_clone = metadata.clone();
                if let Err(e) = self.write_metadata(folder, &metadata_clone) {
                    warn!("Failed to update metadata with device info: {}", e);
                }
            }
        }
    }

    /// Add or update a structured transcript segment (upserts based on sequence_id)
    /// Also saves incrementally to disk
    pub fn add_transcript_segment(&self, segment: TranscriptSegment) {
        if let Ok(mut segments) = self.transcript_segments.lock() {
            // Check if segment with same sequence_id exists (update it)
            if let Some(existing) = segments.iter_mut().find(|s| s.sequence_id == segment.sequence_id) {
                *existing = segment.clone();
                info!("Updated transcript segment {} (seq: {}) - total segments: {}",
                      segment.id, segment.sequence_id, segments.len());
            } else {
                // New segment, add it
                segments.push(segment.clone());
                info!("Added new transcript segment {} (seq: {}) - total segments: {}",
                      segment.id, segment.sequence_id, segments.len());
            }
        } else {
            error!("Failed to lock transcript segments for adding segment {}", segment.id);
        }

        // NEW: Save incrementally to disk
        if let Some(folder) = &self.meeting_folder {
            if let Err(e) = self.write_transcripts_json(folder) {
                warn!("Failed to write incremental transcript update: {}", e);
            }
        }
    }

    /// Legacy method for backward compatibility - converts text to basic segment
    pub fn add_transcript_chunk(&self, text: String) {
        let segment = TranscriptSegment {
            id: format!("seg_{}", chrono::Utc::now().timestamp_millis()),
            text,
            audio_start_time: 0.0,
            audio_end_time: 0.0,
            duration: 0.0,
            display_time: "[00:00]".to_string(),
            confidence: 1.0,
            sequence_id: 0,
            ..Default::default()
        };
        self.add_transcript_segment(segment);
    }

    /// Create the meeting folder (and checkpoints when `auto_save` is on) so
    /// other recorders, like the diarization stem saver, can write into it
    /// before accumulation starts. `start_accumulation` calls this itself if
    /// the folder hasn't been prepared.
    pub fn prepare_meeting_folder(&mut self, auto_save: bool) -> Result<()> {
        if self.meeting_folder.is_some() {
            return Ok(());
        }
        let name = self
            .meeting_name
            .clone()
            .context("Meeting name is missing")?;
        self.initialize_meeting_folder(&name, auto_save)
            .with_context(|| {
                format!(
                    "Failed to prepare the recording folder in {}",
                    super::recording_preferences::get_default_recordings_folder().display()
                )
            })
    }

    /// Start the worker only after storage is ready. With auto-save off, only
    /// transcripts and metadata are saved.
    pub fn start_accumulation(
        &mut self,
        auto_save: bool,
        mut receiver: mpsc::UnboundedReceiver<AudioChunk>,
    ) -> Result<()> {
        anyhow::ensure!(
            self.accumulation_task.is_none(),
            "Recording saver is already running"
        );
        self.prepare_meeting_folder(auto_save)?;
        anyhow::ensure!(
            !auto_save || self.incremental_saver.is_some(),
            "Audio saver is missing"
        );

        let incremental_saver = self.incremental_saver.clone();
        let (stop_sender, mut stop_receiver) = oneshot::channel();
        self.accumulation_stop = Some(stop_sender);
        self.accumulation_task = Some(tokio::spawn(async move {
            let mut first_error = None;
            loop {
                let chunk = tokio::select! {
                    // Close the receiver at stop, then drain every accepted chunk.
                    // A retained sender must not prevent the worker from ending.
                    _ = &mut stop_receiver => {
                        receiver.close();
                        break;
                    }
                    chunk = receiver.recv() => chunk,
                };
                let Some(chunk) = chunk else { break };
                Self::accumulate_chunk(&incremental_saver, chunk, &mut first_error).await;
            }
            while let Some(chunk) = receiver.recv().await {
                Self::accumulate_chunk(&incremental_saver, chunk, &mut first_error).await;
            }
            first_error.map_or(Ok(()), Err)
        }));
        Ok(())
    }

    /// Keep each chunk and retain the first checkpoint write error for stop.
    async fn accumulate_chunk(
        saver: &Option<Arc<AsyncMutex<IncrementalAudioSaver>>>,
        chunk: AudioChunk,
        first_error: &mut Option<anyhow::Error>,
    ) {
        if let Some(saver) = saver {
            if let Err(error) = saver.lock().await.add_chunk(chunk) {
                error!("Failed to save audio checkpoint: {error}");
                // add_chunk retains the buffer on failure. Continue to accept
                // audio, but do not report a successful save after a write error.
                if first_error.is_none() {
                    *first_error = Some(error.context("Failed to save audio checkpoint"));
                }
            }
        }
    }

    /// Close the input queue and wait until all accepted chunks are processed.
    async fn stop_accumulation(&mut self) -> Result<()> {
        if let Some(stop) = self.accumulation_stop.take() {
            let _ = stop.send(());
        }
        // Keep ownership while awaiting so cancellation cannot detach the task.
        let result = if let Some(task) = self.accumulation_task.as_mut() {
            task.await.context("Recording saver task failed")?
        } else {
            Ok(())
        };
        self.accumulation_task = None;
        result
    }

    /// Initialize meeting folder structure and metadata
    ///
    /// # Arguments
    /// * `meeting_name` - Name of the meeting
    /// * `create_checkpoints` - Whether to create .checkpoints/ directory and IncrementalAudioSaver
    fn initialize_meeting_folder(&mut self, meeting_name: &str, create_checkpoints: bool) -> Result<()> {
        // Load preferences to get base recordings folder
        let base_folder = super::recording_preferences::get_default_recordings_folder();

        self.initialize_meeting_folder_at(&base_folder, meeting_name, create_checkpoints)
    }

    /// Prepare storage under the supplied base path. Publish saver state only
    /// after the folder, audio saver, and initial metadata are ready.
    fn initialize_meeting_folder_at(
        &mut self,
        base_folder: &PathBuf,
        meeting_name: &str,
        create_checkpoints: bool,
    ) -> Result<()> {
        let meeting_folder = create_meeting_folder(base_folder, meeting_name, create_checkpoints)?;

        let incremental_saver = if create_checkpoints {
            Some(Arc::new(AsyncMutex::new(IncrementalAudioSaver::new(meeting_folder.clone(), 48000)?)))
        } else {
            None
        };

        // Create initial metadata
        let metadata = MeetingMetadata {
            version: "1.0".to_string(),
            meeting_id: None,  // Will be set by backend
            meeting_name: Some(meeting_name.to_string()),
            created_at: chrono::Utc::now().to_rfc3339(),
            completed_at: None,
            duration_seconds: None,
            devices: DeviceInfo {
                microphone: None,  // Could be enhanced to store actual device names
                system_audio: None,
            },
            audio_file: if create_checkpoints { "audio.mp4".to_string() } else { "".to_string() },
            transcript_file: "transcripts.json".to_string(),
            sample_rate: 48000,
            status: "recording".to_string(),
        };

        // Write initial metadata.json
        self.write_metadata(&meeting_folder, &metadata)?;

        self.incremental_saver = incremental_saver;
        self.meeting_folder = Some(meeting_folder);
        self.metadata = Some(metadata);

        Ok(())
    }

    /// Write metadata.json to disk (atomic write with temp file)
    fn write_metadata(&self, folder: &PathBuf, metadata: &MeetingMetadata) -> Result<()> {
        let metadata_path = folder.join("metadata.json");
        let temp_path = folder.join(".metadata.json.tmp");

        let json_string = serde_json::to_string_pretty(metadata)?;
        std::fs::write(&temp_path, json_string)?;
        std::fs::rename(&temp_path, &metadata_path)?;  // Atomic

        Ok(())
    }

    /// Write transcripts.json to disk (atomic write with temp file and validation)
    fn write_transcripts_json(&self, folder: &PathBuf) -> Result<()> {
        // Clone segments to avoid holding lock during I/O
        let segments_clone = if let Ok(segments) = self.transcript_segments.lock() {
            segments.clone()
        } else {
            error!("Failed to lock transcript segments for writing");
            return Err(anyhow::anyhow!("Failed to lock transcript segments"));
        };

        info!("Writing {} transcript segments to JSON", segments_clone.len());

        let transcript_path = folder.join("transcripts.json");
        let temp_path = folder.join(".transcripts.json.tmp");

        // Create JSON structure
        let json = serde_json::json!({
            "version": "1.0",
            "segments": segments_clone,
            "last_updated": chrono::Utc::now().to_rfc3339(),
            "total_segments": segments_clone.len()
        });

        // Serialize to pretty JSON string
        let json_string = serde_json::to_string_pretty(&json)
            .map_err(|e| {
                error!("Failed to serialize transcripts to JSON: {}", e);
                anyhow::anyhow!("JSON serialization failed: {}", e)
            })?;

        // Write to temp file with error handling
        std::fs::write(&temp_path, &json_string)
            .map_err(|e| {
                error!("Failed to write transcript temp file to {}: {}", temp_path.display(), e);
                anyhow::anyhow!("Failed to write temp file: {}", e)
            })?;

        // Verify temp file was written correctly
        if !temp_path.exists() {
            error!("Temp transcript file does not exist after write: {}", temp_path.display());
            return Err(anyhow::anyhow!("Temp file verification failed"));
        }

        // Atomic rename
        std::fs::rename(&temp_path, &transcript_path)
            .map_err(|e| {
                error!("Failed to rename transcript file from {} to {}: {}",
                       temp_path.display(), transcript_path.display(), e);
                anyhow::anyhow!("Failed to rename transcript file: {}", e)
            })?;

        info!("✅ Successfully wrote transcripts.json with {} segments", segments_clone.len());
        Ok(())
    }

    // in frontend/src-tauri/src/audio/recording_saver.rs
    pub fn get_stats(&self) -> (usize, u32) {
        if let Some(ref saver) = self.incremental_saver {
            if let Ok(guard) = saver.try_lock() {
                (guard.get_checkpoint_count() as usize, 48000)
            } else {
                (0, 48000)
            }
        } else {
            (0, 48000)
        }
    }

    /// Stop and save using incremental saving approach
    ///
    /// # Arguments
    /// * `app` - Tauri app handle for emitting events
    /// * `recording_duration` - Actual recording duration in seconds (from RecordingState)
    pub async fn stop_and_save<R: Runtime>(
        &mut self,
        app: &AppHandle<R>,
        recording_duration: Option<f64>,
    ) -> Result<Option<String>, String> {
        let result = self.finish_save(recording_duration).await;
        match result {
            Ok(final_audio_path) => {
                let save_event = serde_json::json!({
                    "audio_file": final_audio_path,
                    "transcript_file": self.meeting_folder.as_ref()
                        .map(|f| f.join("transcripts.json").to_string_lossy().to_string()),
                    "meeting_name": self.meeting_name,
                    "meeting_folder": self.meeting_folder.as_ref()
                        .map(|f| f.to_string_lossy().to_string())
                });
                if let Err(error) = app.emit("recording-saved", &save_event) {
                    warn!("Failed to emit recording-saved event: {error}");
                }
                if let Ok(mut segments) = self.transcript_segments.lock() {
                    segments.clear();
                }
                Ok(final_audio_path)
            }
            Err(error) => Err(error),
        }
    }

    /// Save final files and mark metadata as an error if any step fails.
    async fn finish_save(
        &mut self,
        recording_duration: Option<f64>,
    ) -> Result<Option<String>, String> {
        let result = self.finalize_files(recording_duration).await;
        if result.is_err() {
            if let (Some(folder), Some(mut metadata)) =
                (&self.meeting_folder, self.metadata.clone())
            {
                metadata.status = "error".to_string();
                if let Err(error) = self.write_metadata(folder, &metadata) {
                    warn!("Failed to write recording error status: {error}");
                }
            }
        }
        result
    }

    /// Drain audio and save all final files before removing recovery checkpoints.
    async fn finalize_files(
        &mut self,
        recording_duration: Option<f64>,
    ) -> Result<Option<String>, String> {
        let accumulation_result = self.stop_accumulation().await;
        // Attempt the final checkpoint even after an earlier write error.
        let final_audio_path = if let Some(saver) = &self.incremental_saver {
            Some(
                saver
                    .lock()
                    .await
                    .finalize()
                    .await
                    .map_err(|error| format!("Failed to finalize audio: {error:#}"))?,
            )
        } else {
            None
        };
        accumulation_result.map_err(|error| format!("{error:#}"))?;

        // Save final transcripts.json with validation
        if let Some(folder) = &self.meeting_folder {
            if let Err(e) = self.write_transcripts_json(folder) {
                error!("❌ Failed to write final transcripts: {}", e);
                return Err(format!("Failed to save transcripts: {}", e));
            }

            // Verify transcripts were written correctly
            let transcript_path = folder.join("transcripts.json");
            if !transcript_path.exists() {
                error!(
                    "❌ Transcript file was not created at: {}",
                    transcript_path.display()
                );
                return Err("Transcript file verification failed".to_string());
            }
            info!(
                "✅ Transcripts saved and verified at: {}",
                transcript_path.display()
            );
        }

        // Update metadata to completed status with actual recording duration
        if let (Some(folder), Some(mut metadata)) = (&self.meeting_folder, self.metadata.clone()) {
            metadata.status = "completed".to_string();
            metadata.completed_at = Some(chrono::Utc::now().to_rfc3339());

            // Use actual recording duration from RecordingState (more accurate than transcript segments)
            // Falls back to last transcript segment if duration not provided
            metadata.duration_seconds = recording_duration.or_else(|| {
                if let Ok(segments) = self.transcript_segments.lock() {
                    segments.last().map(|seg| seg.audio_end_time)
                } else {
                    None
                }
            });

            if let Err(e) = self.write_metadata(folder, &metadata) {
                error!("❌ Failed to update metadata to completed: {}", e);
                return Err(format!("Failed to update metadata: {}", e));
            }

            info!(
                "✅ Metadata updated with duration: {:?}s",
                metadata.duration_seconds
            );
        }

        // Keep checkpoints until all final files have been saved.
        if let Some(saver) = &self.incremental_saver {
            saver.lock().await.cleanup_checkpoints();
        }
        Ok(final_audio_path.map(|path| path.to_string_lossy().to_string()))
    }

    /// Get the meeting folder path (for passing to backend)
    pub fn get_meeting_folder(&self) -> Option<&PathBuf> {
        self.meeting_folder.as_ref()
    }

    /// Get accumulated transcript segments (for reload sync)
    pub fn get_transcript_segments(&self) -> Vec<TranscriptSegment> {
        if let Ok(segments) = self.transcript_segments.lock() {
            segments.clone()
        } else {
            Vec::new()
        }
    }

    /// Get meeting name (for reload sync)
    pub fn get_meeting_name(&self) -> Option<String> {
        self.meeting_name.clone()
    }
}

impl Drop for RecordingSaver {
    fn drop(&mut self) {
        if let Some(task) = &self.accumulation_task {
            task.abort();
        }
    }
}

impl Default for RecordingSaver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepared_saver(auto_save: bool) -> (tempfile::TempDir, RecordingSaver) {
        let directory = tempfile::tempdir().unwrap();
        let mut saver = RecordingSaver::new();
        saver.set_meeting_name(Some("Save test".to_string()));
        saver
            .initialize_meeting_folder_at(&directory.path().into(), "Save test", auto_save)
            .unwrap();
        (directory, saver)
    }

    fn chunk(id: u64, samples: usize) -> AudioChunk {
        AudioChunk {
            data: vec![id as f32 / 128.0; samples],
            sample_rate: 48000,
            timestamp: 0.0,
            end_timestamp: None,
            chunk_id: id,
            device_type: super::super::recording_state::DeviceType::Microphone,
            source_activity: None,
        }
    }

    fn metadata(saver: &RecordingSaver) -> serde_json::Value {
        serde_json::from_slice(
            &std::fs::read(saver.meeting_folder.as_ref().unwrap().join("metadata.json")).unwrap(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn startup_fails_before_spawning_without_storage() {
        let mut saver = RecordingSaver::new();
        let (_sender, receiver) = mpsc::unbounded_channel();
        assert!(saver.start_accumulation(true, receiver).is_err());
        assert!(saver.accumulation_task.is_none());

        let directory = tempfile::tempdir().unwrap();
        let blocked_base = directory.path().join("file");
        std::fs::write(&blocked_base, b"not a directory").unwrap();
        assert!(saver
            .initialize_meeting_folder_at(&blocked_base, "Meeting", true)
            .is_err());
        assert!(saver.meeting_folder.is_none());
        assert!(saver.incremental_saver.is_none());

        // A metadata write failure must not leave a partially prepared saver.
        let base = directory.path().to_path_buf();
        let folder = create_meeting_folder(&base, "Meeting", true).unwrap();
        std::fs::create_dir(folder.join(".metadata.json.tmp")).unwrap();
        assert!(saver
            .initialize_meeting_folder_at(&base, "Meeting", true)
            .is_err());
        assert!(saver.meeting_folder.is_none());
        assert!(saver.incremental_saver.is_none());
    }

    #[tokio::test]
    async fn stop_waits_for_slow_saver_and_drains_every_queued_sample() {
        let (_directory, mut saver) = prepared_saver(true);
        let audio = saver.incremental_saver.clone().unwrap();
        let guard = audio.lock().await;
        let (sender, receiver) = mpsc::unbounded_channel();
        saver.start_accumulation(true, receiver).unwrap();
        let mut expected = Vec::new();
        for id in 0..128 {
            let chunk = chunk(id, 128);
            expected.extend_from_slice(&chunk.data);
            sender.send(chunk).unwrap();
        }
        // Keep the sender alive: shutdown must close it and drain the queue.
        let stop = tokio::spawn(async move {
            saver.stop_accumulation().await.unwrap();
            saver
        });
        tokio::task::yield_now().await;
        assert!(!stop.is_finished());
        drop(guard);
        let _saver = tokio::time::timeout(std::time::Duration::from_secs(5), stop)
            .await
            .unwrap()
            .unwrap();
        assert!(sender.is_closed());
        assert_eq!(audio.lock().await.buffered_audio(), expected);
    }

    #[tokio::test]
    async fn cancelled_stop_keeps_task_owned_until_saver_is_dropped() {
        let (_directory, mut saver) = prepared_saver(true);
        let audio = saver.incremental_saver.clone().unwrap();
        let guard = audio.lock().await;
        let (sender, receiver) = mpsc::unbounded_channel();
        saver.start_accumulation(true, receiver).unwrap();
        sender.send(chunk(1, 128)).unwrap();
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(10),
            saver.stop_accumulation()
        )
        .await
        .is_err());
        assert!(saver.accumulation_task.is_some());
        drop(saver);
        tokio::task::yield_now().await;
        assert!(sender.is_closed());
        drop(guard);
    }

    #[tokio::test]
    async fn checkpoint_write_error_reaches_stop_and_keeps_buffer() {
        let (_directory, mut saver) = prepared_saver(true);
        let folder = saver.meeting_folder.clone().unwrap();
        std::fs::create_dir(folder.join(".checkpoints/audio_chunk_000.mp4")).unwrap();
        let (sender, receiver) = mpsc::unbounded_channel();
        saver.start_accumulation(true, receiver).unwrap();
        sender.send(chunk(1, 48000 * 30)).unwrap();
        let error = saver.stop_accumulation().await.unwrap_err();
        assert!(format!("{error:#}").contains("Failed to save audio checkpoint"));
        assert_eq!(
            saver
                .incremental_saver
                .as_ref()
                .unwrap()
                .lock()
                .await
                .buffered_audio()
                .len(),
            48000 * 30
        );
    }

    #[tokio::test]
    async fn transcript_only_stop_writes_final_files_and_reports_write_errors() {
        let (_directory, mut saver) = prepared_saver(false);
        saver.add_transcript_chunk("Keep this transcript".to_string());
        let folder = saver.meeting_folder.clone().unwrap();
        std::fs::create_dir(folder.join(".transcripts.json.tmp")).unwrap();
        let error = saver.finish_save(Some(3.0)).await.unwrap_err();
        assert!(error.contains("Failed to save transcripts"));
        assert_eq!(metadata(&saver)["status"], "error");
        assert_eq!(saver.get_transcript_segments().len(), 1);
        std::fs::remove_dir(folder.join(".transcripts.json.tmp")).unwrap();
        assert_eq!(saver.finish_save(Some(3.0)).await.unwrap(), None);
        assert_eq!(metadata(&saver)["status"], "completed");
        assert_eq!(metadata(&saver)["duration_seconds"], 3.0);
        assert!(!folder.join("audio.mp4").exists());
    }

    #[tokio::test]
    async fn final_audio_failure_keeps_checkpoints_and_marks_error() {
        let (_directory, mut saver) = prepared_saver(true);
        let folder = saver.meeting_folder.clone().unwrap();
        saver
            .incremental_saver
            .as_ref()
            .unwrap()
            .lock()
            .await
            .add_chunk(chunk(1, 48000))
            .unwrap();
        std::fs::create_dir(folder.join("audio.mp4")).unwrap();
        assert!(saver
            .finish_save(Some(1.0))
            .await
            .unwrap_err()
            .contains("Failed to finalize audio"));
        assert!(folder.join(".checkpoints/audio_chunk_000.mp4").is_file());
        assert_eq!(metadata(&saver)["status"], "error");
    }

    #[tokio::test]
    async fn metadata_failure_keeps_recovery_files_until_successful_retry() {
        let (_directory, mut saver) = prepared_saver(true);
        let folder = saver.meeting_folder.clone().unwrap();
        saver
            .incremental_saver
            .as_ref()
            .unwrap()
            .lock()
            .await
            .add_chunk(chunk(1, 48000))
            .unwrap();
        std::fs::create_dir(folder.join(".metadata.json.tmp")).unwrap();
        assert!(saver
            .finish_save(Some(1.0))
            .await
            .unwrap_err()
            .contains("Failed to update metadata"));
        assert!(folder.join("audio.mp4").is_file());
        assert!(folder.join(".checkpoints/audio_chunk_000.mp4").is_file());
        assert_ne!(metadata(&saver)["status"], "completed");
        std::fs::remove_dir(folder.join(".metadata.json.tmp")).unwrap();
        assert!(saver.finish_save(Some(1.0)).await.unwrap().is_some());
        assert_eq!(metadata(&saver)["status"], "completed");
        assert!(!folder.join(".checkpoints").exists());
    }

    #[test]
    fn live_speaker_data_survives_history_and_json_round_trips() {
        for overlap in [false, true] {
            let event = serde_json::json!({
                "text": "Hello", "timestamp": "14:30:05", "source": "Audio", "sequence_id": 7,
                "chunk_start_time": 1.0, "is_partial": false, "confidence": 0.85,
                "audio_start_time": 1.0, "audio_end_time": 2.0, "duration": 1.0,
                "speaker_id": if overlap { None } else { Some("alice") },
                "speaker_label": if overlap { "Multiple speakers" } else { "Alice" },
                "speaker_profile_id": if overlap { None } else { Some("profile-alice") },
                "speaker_source": "microphone", "speaker_color": "#2563eb", "is_overlap": overlap,
                "candidate_speaker_ids": if overlap { vec!["alice", "bob"] } else { vec![] },
                "diarization_status": "provisional", "diarization_method": "live_source_wespeaker",
                "diarization_confidence": 0.75,
            });
            let update: super::super::transcription::TranscriptUpdate = serde_json::from_value(event.clone()).unwrap();
            let directory = tempfile::tempdir().unwrap();
            let mut saver = RecordingSaver::new();
            saver.meeting_folder = Some(directory.path().into());
            saver.add_transcript_segment(update.into());
            let history = serde_json::to_value(saver.get_transcript_segments()).unwrap();
            let disk: serde_json::Value = serde_json::from_slice(&std::fs::read(directory.path().join("transcripts.json")).unwrap()).unwrap();
            for field in ["speaker_id", "speaker_label", "speaker_profile_id", "speaker_source", "speaker_color",
                "is_overlap", "candidate_speaker_ids", "diarization_status", "diarization_method", "diarization_confidence"] {
                assert_eq!(history[0][field], event[field], "history lost {field}");
                assert_eq!(disk["segments"][0][field], event[field], "JSON lost {field}");
            }
            let restored: Vec<TranscriptSegment> = serde_json::from_value(disk["segments"].clone()).unwrap();
            assert_eq!(serde_json::to_value(restored).unwrap(), history);
        }
    }

    #[test]
    fn old_transcript_json_without_speaker_fields_still_loads() {
        let segment: TranscriptSegment = serde_json::from_value(serde_json::json!({
            "id": "old", "text": "Hello", "audio_start_time": 0.0, "audio_end_time": 1.0,
            "duration": 1.0, "display_time": "00:00", "confidence": 1.0, "sequence_id": 0,
        })).unwrap();
        assert!(segment.speaker_id.is_none());
        assert!(segment.is_overlap.is_none());
    }
}
