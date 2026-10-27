use super::encode::encode_single_audio;
use super::recording_state::AudioChunk;
use anyhow::{anyhow, Context, Result};
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::ffmpeg::find_ffmpeg_path;

/// One `file` line for ffmpeg's concat list. A single quote in the path (for
/// example a meeting called "John's Standup") must be escaped or ffmpeg cannot
/// parse the list and the recording never merges.
fn concat_list_line(path: &std::path::Path) -> String {
    format!("file '{}'\n", path.display().to_string().replace('\'', "'\\''"))
}

/// Audio data without device type (we only store mixed audio)
#[derive(Clone)]
struct AudioData {
    data: Vec<f32>,
    // sample_rate: u32,
}

/// Incremental audio saver that writes checkpoints every 30 seconds
/// to minimize memory usage and enable crash recovery
pub struct IncrementalAudioSaver {
    checkpoint_buffer: Vec<AudioData>,
    checkpoint_interval_samples: usize, // 30s at 48kHz = 1,440,000 samples
    samples_since_checkpoint_attempt: usize,
    checkpoint_count: u32,
    checkpoints_dir: PathBuf,
    meeting_folder: PathBuf,
    sample_rate: u32,
}

impl IncrementalAudioSaver {
    /// Create a new incremental saver
    ///
    /// # Arguments
    /// * `meeting_folder` - Path to the meeting folder (contains .checkpoints/)
    /// * `sample_rate` - Sample rate of audio (typically 48000)
    pub fn new(meeting_folder: PathBuf, sample_rate: u32) -> Result<Self> {
        let checkpoints_dir = meeting_folder.join(".checkpoints");

        // Verify checkpoints directory exists
        if !checkpoints_dir.exists() {
            return Err(anyhow!(
                "Checkpoints directory does not exist: {}",
                checkpoints_dir.display()
            ));
        }

        Ok(Self {
            checkpoint_buffer: Vec::new(),
            checkpoint_interval_samples: sample_rate as usize * 30, // 30 seconds
            samples_since_checkpoint_attempt: 0,
            checkpoint_count: 0,
            checkpoints_dir,
            meeting_folder,
            sample_rate,
        })
    }

    /// Add an audio chunk to the buffer
    /// Attempt a checkpoint after each 30 seconds of new audio. Keep buffered
    /// samples after a failed write, and wait for another interval before retry.
    pub fn add_chunk(&mut self, chunk: AudioChunk) -> Result<()> {
        self.samples_since_checkpoint_attempt += chunk.data.len();
        let audio_data = AudioData {
            data: chunk.data,
            // sample_rate: chunk.sample_rate,
        };

        self.checkpoint_buffer.push(audio_data);

        if self.samples_since_checkpoint_attempt >= self.checkpoint_interval_samples {
            // Reset before writing so a failure cannot cause a retry on every chunk.
            self.samples_since_checkpoint_attempt = 0;
            self.save_checkpoint()?;
            self.checkpoint_buffer.clear();
        }

        Ok(())
    }

    /// Save current buffer as a checkpoint file
    fn save_checkpoint(&mut self) -> Result<()> {
        // Concatenate all chunks in buffer
        let audio_data: Vec<f32> = self
            .checkpoint_buffer
            .iter()
            .flat_map(|c| &c.data)
            .cloned()
            .collect();

        if audio_data.is_empty() {
            warn!("Attempted to save empty checkpoint, skipping");
            return Ok(());
        }

        // Generate checkpoint filename
        let checkpoint_path = self
            .checkpoints_dir
            .join(format!("audio_chunk_{:03}.mp4", self.checkpoint_count));

        // Encode to a new path. A failed attempt must not leave a partial
        // checkpoint that prevents the next retry or replaces a complete file.
        let temporary_path = tempfile::Builder::new()
            .prefix(".audio_chunk_")
            .suffix(".tmp")
            .tempfile_in(&self.checkpoints_dir)?
            .into_temp_path();
        // FFmpeg expects an unused output path. TempPath still owns cleanup.
        std::fs::remove_file(&temporary_path)?;
        encode_single_audio(
            bytemuck::cast_slice(&audio_data),
            self.sample_rate,
            1, // mono
            &temporary_path.to_path_buf(),
        )?;
        temporary_path.persist(&checkpoint_path)
            .with_context(|| format!("Failed to save checkpoint at {}", checkpoint_path.display()))?;

        let duration_seconds = audio_data.len() as f32 / self.sample_rate as f32;
        self.checkpoint_count += 1;

        info!(
            "Saved checkpoint {}: {:.2}s of audio ({} samples)",
            self.checkpoint_count,
            duration_seconds,
            audio_data.len()
        );

        Ok(())
    }

    /// Save the final checkpoint and merge the audio. Keep checkpoints until
    /// the caller finishes saving all recording files.
    ///
    /// Returns the path to the final merged audio.mp4 file
    pub async fn finalize(&mut self) -> Result<PathBuf> {
        info!("Finalizing incremental recording...");

        // Save final buffer if not empty
        if !self.checkpoint_buffer.is_empty() {
            info!(
                "Saving final checkpoint with remaining {} chunks",
                self.checkpoint_buffer.len()
            );
            self.save_checkpoint()?;
            self.checkpoint_buffer.clear();
            self.samples_since_checkpoint_attempt = 0;
        }

        if self.checkpoint_count == 0 {
            return Err(anyhow!(
                "No audio checkpoints to merge - recording may have failed"
            ));
        }

        // Merge all checkpoints using FFmpeg concat
        let final_audio_path = self.meeting_folder.join("audio.mp4");
        self.merge_checkpoints(&final_audio_path).await?;

        info!("Finalized recording: {}", final_audio_path.display());
        Ok(final_audio_path)
    }

    /// Remove checkpoints after all recording files have been saved.
    pub fn cleanup_checkpoints(&self) {
        if let Err(error) = std::fs::remove_dir_all(&self.checkpoints_dir) {
            warn!("Failed to clean up checkpoints directory: {error}");
        }
    }

    /// Merge all checkpoint files into final audio.mp4 using FFmpeg concat
    /// Uses concat demuxer for fast merging without re-encoding
    async fn merge_checkpoints(&self, output: &PathBuf) -> Result<()> {
        info!(
            "Merging {} checkpoints into final audio file...",
            self.checkpoint_count
        );

        // Create concat list file for FFmpeg
        let list_file = self.checkpoints_dir.join("concat_list.txt");
        let mut list_content = String::new();

        for i in 0..self.checkpoint_count {
            let checkpoint_path = self
                .checkpoints_dir
                .join(format!("audio_chunk_{:03}.mp4", i));

            // Verify checkpoint exists
            if !checkpoint_path.exists() {
                return Err(anyhow!(
                    "Checkpoint file missing: {}",
                    checkpoint_path.display()
                ));
            }

            // Use absolute path for FFmpeg (required for safe mode)
            let abs_path = checkpoint_path.canonicalize()?;
            list_content.push_str(&concat_list_line(&abs_path));
        }

        std::fs::write(&list_file, list_content)?;

        let ffmpeg_path = find_ffmpeg_path().ok_or_else(|| {
            anyhow!("FFmpeg not found. Please install FFmpeg to finalize recordings.")
        })?;
        info!("Using FFmpeg at: {:?}", ffmpeg_path);

        // Run FFmpeg concat command
        // Using concat demuxer with copy codec for fast merging (no re-encoding)

        let mut command = std::process::Command::new(ffmpeg_path);

        command.args(&[
            "-f",
            "concat", // Use concat demuxer
            "-safe",
            "0", // Allow absolute paths
            "-i",
            list_file.to_str().unwrap(),
            "-c",
            "copy", // Copy codec - no re-encoding!
            "-y",   // Overwrite output file
            output.to_str().unwrap(),
        ]);

        // Hide console window on Windows to prevent CMD popup during finalization
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            command.creation_flags(CREATE_NO_WINDOW);
        }

        let ffmpeg_output = command.output()?;

        if !ffmpeg_output.status.success() {
            let stderr = String::from_utf8_lossy(&ffmpeg_output.stderr);
            error!("FFmpeg merge failed: {}", stderr);
            return Err(anyhow!("FFmpeg concat failed: {}", stderr));
        }

        // Verify output file was created
        if !output.exists() {
            return Err(anyhow!(
                "Merged audio file was not created: {}",
                output.display()
            ));
        }

        info!(
            "Successfully merged {} checkpoints → {}",
            self.checkpoint_count,
            output.display()
        );

        Ok(())
    }

    /// Return buffered samples in order for loss and retry tests.
    #[cfg(test)]
    pub(super) fn buffered_audio(&self) -> Vec<f32> {
        self.checkpoint_buffer.iter().flat_map(|chunk| chunk.data.iter().copied()).collect()
    }

    /// Get the meeting folder path
    pub fn get_meeting_folder(&self) -> &PathBuf {
        &self.meeting_folder
    }

    /// Get current checkpoint count
    pub fn get_checkpoint_count(&self) -> u32 {
        self.checkpoint_count
    }
}

/// Audio recovery status for transcript recovery feature
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioRecoveryStatus {
    pub status: String, // "success" | "partial" | "failed" | "none"
    pub chunk_count: u32,
    pub estimated_duration_seconds: f64,
    pub audio_file_path: Option<String>,
    pub message: String,
}

/// List published checkpoint files in recording order. Ignore scratch files
/// and directories left by an interrupted write.
fn audio_checkpoint_files(directory: &PathBuf) -> Result<Vec<PathBuf>> {
    let mut checkpoints = Vec::new();
    for entry in std::fs::read_dir(directory)?.filter_map(|entry| entry.ok()) {
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some(index) = name.to_str()
            .and_then(|name| name.strip_prefix("audio_chunk_"))
            .and_then(|name| name.strip_suffix(".mp4"))
            .filter(|index| !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|index| index.parse::<u32>().ok())
        else {
            continue;
        };
        checkpoints.push((index, entry.path()));
    }
    checkpoints.sort_by_key(|(index, _)| *index);
    Ok(checkpoints.into_iter().map(|(_, path)| path).collect())
}

/// Recover audio from checkpoint files
/// This is called by the transcript recovery system to merge audio chunks after a crash
#[tauri::command]
pub async fn recover_audio_from_checkpoints(
    meeting_folder: String,
    _sample_rate: u32,
) -> Result<AudioRecoveryStatus, String> {
    info!("Starting audio recovery for folder: {}", meeting_folder);

    let folder_path = PathBuf::from(&meeting_folder);
    let checkpoints_dir = folder_path.join(".checkpoints");

    // Check if checkpoints directory exists
    if !checkpoints_dir.exists() {
        info!(
            "No checkpoints directory found at: {}",
            checkpoints_dir.display()
        );
        return Ok(AudioRecoveryStatus {
            status: "none".to_string(),
            chunk_count: 0,
            estimated_duration_seconds: 0.0,
            audio_file_path: None,
            message: "No audio checkpoints found".to_string(),
        });
    }

    // Scan for checkpoint files
    let checkpoint_files = audio_checkpoint_files(&checkpoints_dir)
        .map_err(|e| format!("Failed to read checkpoints directory: {}", e))?;

    if checkpoint_files.is_empty() {
        info!(
            "No checkpoint files found in: {}",
            checkpoints_dir.display()
        );
        return Ok(AudioRecoveryStatus {
            status: "none".to_string(),
            chunk_count: 0,
            estimated_duration_seconds: 0.0,
            audio_file_path: None,
            message: "No audio checkpoint files found".to_string(),
        });
    }

    let chunk_count = checkpoint_files.len() as u32;
    let estimated_duration = (chunk_count as f64) * 30.0; // 30 seconds per chunk

    info!(
        "Found {} checkpoint files, estimated duration: {:.2}s",
        chunk_count, estimated_duration
    );

    // Create FFmpeg concat file
    let concat_file_path = checkpoints_dir.join("concat_list.txt");
    let mut concat_content = String::new();

    for checkpoint in &checkpoint_files {
        let path = checkpoint
            .canonicalize()
            .map_err(|e| format!("Failed to canonicalize path: {}", e))?;
        concat_content.push_str(&concat_list_line(&path));
    }

    std::fs::write(&concat_file_path, concat_content)
        .map_err(|e| format!("Failed to write concat file: {}", e))?;

    // Run FFmpeg to merge chunks
    let output_path = folder_path.join("audio.mp4");
    let output_path_str = output_path
        .to_str()
        .ok_or("Invalid output path")?
        .to_string();

    let ffmpeg_path = find_ffmpeg_path()
        .ok_or_else(|| "FFmpeg not found. Please install FFmpeg to recover audio.".to_string())?;
    info!("Using FFmpeg at: {:?}", ffmpeg_path);

    let mut command = std::process::Command::new(ffmpeg_path);

    command.args(&[
        "-f",
        "concat",
        "-safe",
        "0",
        "-i",
        concat_file_path.to_str().unwrap(),
        "-c",
        "copy",
        "-y", // Overwrite if exists
        &output_path_str,
    ]);

    // Hide console window on Windows
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let ffmpeg_result = command.output();

    match ffmpeg_result {
        Ok(output) if output.status.success() => {
            // Clean up concat file
            let _ = std::fs::remove_file(concat_file_path);

            info!("Successfully recovered audio: {}", output_path_str);

            Ok(AudioRecoveryStatus {
                status: "success".to_string(),
                chunk_count,
                estimated_duration_seconds: estimated_duration,
                audio_file_path: Some(output_path_str),
                message: format!("Successfully recovered {} audio chunks", chunk_count),
            })
        }
        Ok(output) => {
            let error = String::from_utf8_lossy(&output.stderr);
            error!("FFmpeg recovery failed: {}", error);
            Ok(AudioRecoveryStatus {
                status: "failed".to_string(),
                chunk_count,
                estimated_duration_seconds: estimated_duration,
                audio_file_path: None,
                message: format!("FFmpeg failed: {}", error),
            })
        }
        Err(e) => {
            error!("Failed to run FFmpeg: {}", e);
            Ok(AudioRecoveryStatus {
                status: "failed".to_string(),
                chunk_count,
                estimated_duration_seconds: estimated_duration,
                audio_file_path: None,
                message: format!("Failed to run FFmpeg: {}", e),
            })
        }
    }
}

/// Clean up checkpoint files after successful recording or recovery
/// This command is called by the frontend after successful save to clean up checkpoint files
#[tauri::command]
pub async fn cleanup_checkpoints(meeting_folder: String) -> Result<(), String> {
    info!("Cleaning up checkpoints for folder: {}", meeting_folder);

    let folder_path = PathBuf::from(&meeting_folder);
    let checkpoints_dir = folder_path.join(".checkpoints");

    if checkpoints_dir.exists() {
        std::fs::remove_dir_all(&checkpoints_dir)
            .map_err(|e| format!("Failed to remove checkpoints directory: {}", e))?;
        info!("Successfully cleaned up checkpoints directory");
    } else {
        info!("No checkpoints directory to clean up");
    }

    Ok(())
}

/// Check if a meeting folder has audio checkpoint files
/// Return true if the checkpoint directory contains published audio chunks.
#[tauri::command]
pub async fn has_audio_checkpoints(meeting_folder: String) -> Result<bool, String> {
    let folder_path = PathBuf::from(&meeting_folder);
    let checkpoints_dir = folder_path.join(".checkpoints");

    // Check if checkpoints directory exists
    if !checkpoints_dir.exists() {
        return Ok(false);
    }

    audio_checkpoint_files(&checkpoints_dir)
        .map(|files| !files.is_empty())
        .map_err(|e| format!("Failed to read checkpoints directory: {}", e))
}

#[cfg(test)]
mod tests {
    use super::super::recording_state::DeviceType;
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn concat_list_escapes_single_quotes_in_paths() {
        let line = concat_list_line(std::path::Path::new("/meetings/John's Standup/chunk.mp4"));
        assert_eq!(line, "file '/meetings/John'\\''s Standup/chunk.mp4'\n");
        assert_eq!(
            concat_list_line(std::path::Path::new("/meetings/plain/chunk.mp4")),
            "file '/meetings/plain/chunk.mp4'\n"
        );
    }

    #[test]
    fn checkpoints_in_a_folder_with_an_apostrophe_merge() {
        let Some(ffmpeg) = find_ffmpeg_path() else { return };
        let temporary = tempdir().unwrap();
        let directory = temporary.path().join("John's Standup");
        std::fs::create_dir_all(&directory).unwrap();
        let mut list = String::new();
        for index in 0..2 {
            let chunk = directory.join(format!("chunk{index}.mp4"));
            let status = std::process::Command::new(&ffmpeg)
                .args(["-loglevel", "error", "-f", "lavfi", "-i", "sine=duration=1", "-c:a", "aac"])
                .arg(&chunk)
                .status()
                .unwrap();
            assert!(status.success());
            list.push_str(&concat_list_line(&chunk));
        }
        let list_file = directory.join("list.txt");
        std::fs::write(&list_file, list).unwrap();
        let output = directory.join("merged.mp4");
        let status = std::process::Command::new(&ffmpeg)
            .args(["-loglevel", "error", "-f", "concat", "-safe", "0", "-i"])
            .arg(&list_file)
            .args(["-c", "copy"])
            .arg(&output)
            .status()
            .unwrap();
        assert!(status.success() && output.is_file());
    }

    #[test]
    fn published_checkpoints_exclude_scratch_files_and_sort_by_sequence() {
        let directory = tempdir().unwrap();
        for name in ["audio_chunk_1000.mp4", "audio_chunk_002.mp4", "audio_chunk_010.mp4",
            ".audio_chunk_partial.mp4", ".audio_chunk_partial.tmp", "other.mp4",
            "audio_chunk_.mp4", "audio_chunk_+5.mp4"] {
            std::fs::write(directory.path().join(name), b"test data").unwrap();
        }
        std::fs::create_dir(directory.path().join("audio_chunk_001.mp4")).unwrap();
        let files = audio_checkpoint_files(&directory.path().to_path_buf()).unwrap();
        assert_eq!(files, ["audio_chunk_002.mp4", "audio_chunk_010.mp4", "audio_chunk_1000.mp4"]
            .map(|name| directory.path().join(name)));
    }

    #[tokio::test]
    async fn recovery_ignores_interrupted_writes_and_merges_only_published_audio() {
        let directory = tempdir().unwrap();
        let checkpoints = directory.path().join(".checkpoints");
        std::fs::create_dir(&checkpoints).unwrap();
        for name in [".audio_chunk_partial.mp4", ".audio_chunk_partial.tmp", "other.mp4"] {
            std::fs::write(checkpoints.join(name), b"incomplete audio").unwrap();
        }
        std::fs::create_dir(checkpoints.join("audio_chunk_001.mp4")).unwrap();
        let folder = directory.path().to_string_lossy().to_string();
        assert!(!has_audio_checkpoints(folder.clone()).await.unwrap());
        let empty = recover_audio_from_checkpoints(folder.clone(), 48000).await.unwrap();
        assert_eq!(empty.status, "none");
        assert_eq!(empty.chunk_count, 0);

        encode_single_audio(bytemuck::cast_slice(&vec![0.25f32; 4800]), 48000, 1,
            &checkpoints.join("audio_chunk_000.mp4")).unwrap();
        assert!(has_audio_checkpoints(folder.clone()).await.unwrap());
        let recovered = recover_audio_from_checkpoints(folder, 48000).await.unwrap();
        assert_eq!(recovered.status, "success", "{}", recovered.message);
        assert_eq!(recovered.chunk_count, 1);
        assert!(directory.path().join("audio.mp4").is_file());
        assert!(checkpoints.join(".audio_chunk_partial.mp4").is_file());
    }

    #[tokio::test]
    async fn test_checkpoint_creation() {
        // Create temp meeting folder
        let temp_dir = tempdir().unwrap();
        let meeting_folder = temp_dir.path().join("Test_Meeting");
        std::fs::create_dir_all(&meeting_folder).unwrap();
        std::fs::create_dir_all(meeting_folder.join(".checkpoints")).unwrap();

        let mut saver = IncrementalAudioSaver::new(meeting_folder.clone(), 48000).unwrap();

        // Add 60 seconds worth of audio (should create 2 checkpoints)
        for i in 0..120 {
            // 120 chunks of 0.5s each
            let chunk = AudioChunk {
                data: vec![0.5f32; 24000], // 0.5s at 48kHz
                sample_rate: 48000,
                timestamp: i as f64 * 0.5, // timestamp in seconds
                end_timestamp: None,
                chunk_id: i as u64,
                device_type: DeviceType::Microphone,
                source_activity: None,
            };
            saver.add_chunk(chunk).unwrap();
        }

        // Verify 2 checkpoints created
        assert_eq!(saver.checkpoint_count, 2);

        // Finalize and verify merge
        let final_path = saver.finalize().await.unwrap();
        assert!(final_path.exists());

        // Finalization keeps recovery files until the caller commits all files.
        assert!(meeting_folder.join(".checkpoints").exists());
        saver.cleanup_checkpoints();
        assert!(!meeting_folder.join(".checkpoints").exists());
    }

    #[tokio::test]
    async fn failed_checkpoints_wait_for_new_audio_before_retry_and_keep_all_samples() {
        let directory = tempdir().unwrap();
        let folder = directory.path().to_path_buf();
        let checkpoints = folder.join(".checkpoints");
        std::fs::create_dir(&checkpoints).unwrap();
        let mut saver = IncrementalAudioSaver::new(folder, 48000).unwrap();
        // A short interval keeps this test small. Production uses 30 seconds.
        saver.checkpoint_interval_samples = 4800;
        let chunk = |id, count| AudioChunk {
            data: vec![id as f32 / 10.0; count],
            sample_rate: 48000,
            timestamp: 0.0,
            end_timestamp: None,
            chunk_id: id,
            device_type: DeviceType::Microphone,
            source_activity: None,
        };
        let blocked = checkpoints.join("audio_chunk_000.mp4");
        std::fs::create_dir(&blocked).unwrap();
        let mut expected = chunk(1, 4800).data;
        assert!(saver.add_chunk(chunk(1, 4800)).is_err());
        for id in 2..10 {
            let audio = chunk(id, 500);
            expected.extend_from_slice(&audio.data);
            // An error here would mean an early retry against the blocked path.
            assert!(saver.add_chunk(audio).is_ok());
        }
        let audio = chunk(1, 800);
        expected.extend_from_slice(&audio.data);
        assert!(saver.add_chunk(audio).is_err());
        assert_eq!(saver.buffered_audio(), expected);
        assert_eq!(saver.checkpoint_count, 0);
        std::fs::remove_dir(blocked).unwrap();
        assert!(saver.add_chunk(chunk(1, 4800)).is_ok());
        assert_eq!(saver.checkpoint_count, 1);
        assert!(saver.checkpoint_buffer.is_empty());
        assert_eq!(saver.samples_since_checkpoint_attempt, 0);
        assert!(saver.add_chunk(chunk(2, 1)).is_ok());
        assert_eq!(saver.checkpoint_count, 1);
        // Finalization must save even less than one interval of new audio.
        assert!(saver.finalize().await.unwrap().is_file());
        assert_eq!(saver.checkpoint_count, 2);
    }

    #[tokio::test]
    async fn test_empty_recording() {
        let temp_dir = tempdir().unwrap();
        let meeting_folder = temp_dir.path().join("Empty_Test");
        std::fs::create_dir_all(&meeting_folder).unwrap();
        std::fs::create_dir_all(meeting_folder.join(".checkpoints")).unwrap();

        let mut saver = IncrementalAudioSaver::new(meeting_folder.clone(), 48000).unwrap();

        // Try to finalize without adding any chunks
        let result = saver.finalize().await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No audio checkpoints"));
    }
}
