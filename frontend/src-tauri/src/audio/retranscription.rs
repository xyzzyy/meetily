// Retranscription module - allows re-processing stored audio with different settings

use super::common::{create_transcript_segments, write_transcripts_json};
use super::constants::AUDIO_EXTENSIONS;
use crate::audio::decoder::decode_audio_file;
use crate::audio::vad::get_speech_chunks_with_progress;
use crate::config::{DEFAULT_PARAKEET_MODEL, DEFAULT_WHISPER_MODEL};
use crate::parakeet_engine::ParakeetEngine;
use crate::state::AppState;
use crate::whisper_engine::WhisperEngine;
use anyhow::{anyhow, Result};
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, Runtime};

/// Global flag to track if retranscription is in progress
static RETRANSCRIPTION_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// Global flag to signal cancellation
static RETRANSCRIPTION_CANCELLED: AtomicBool = AtomicBool::new(false);

/// RAII guard for RETRANSCRIPTION_IN_PROGRESS flag
/// Ensures flag is cleared even if retranscription panics or returns early
struct RetranscriptionGuard;

impl RetranscriptionGuard {
    /// Create guard and set flag atomically
    fn acquire() -> Result<Self, String> {
        if RETRANSCRIPTION_IN_PROGRESS
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err("Retranscription already in progress".to_string());
        }
        Ok(RetranscriptionGuard)
    }
}

impl Drop for RetranscriptionGuard {
    fn drop(&mut self) {
        RETRANSCRIPTION_IN_PROGRESS.store(false, Ordering::SeqCst);
    }
}

/// VAD redemption time in milliseconds - bridges natural pauses in speech
/// Batch processing needs longer redemption (2000ms) than the live pipeline
/// because it has no latency requirement, and short redemption fragments
/// speech at every breath. Short Whisper inputs are zero-padded to its 30s
/// window and invite hallucinated boilerplate (see pipeline.rs).
const VAD_REDEMPTION_TIME_MS: u32 = 2000;

/// Redemption time when Enhance also assigns speakers. Short pauses are kept
/// apart so a row can end where the speaker changes. Parakeet row boundaries
/// come from its token times; Whisper uses these shorter spans as a fallback.
const SPEAKER_VAD_REDEMPTION_TIME_MS: u32 = 500;

/// Longest input sent to the engine in one piece (16 kHz samples).
const MAX_SEGMENT_SAMPLES: usize = 25 * 16000;

/// Whisper input limit when assigning speakers, so turns aren't merged.
const SPEAKER_WHISPER_MAX_SEGMENT_SAMPLES: usize = 8 * 16000;

/// Progress update emitted during retranscription
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetranscriptionProgress {
    pub meeting_id: String,
    pub stage: String, // "decoding", "transcribing", "saving"
    pub progress_percentage: u32,
    pub message: String,
}

/// Result of retranscription
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetranscriptionResult {
    pub meeting_id: String,
    pub segments_count: usize,
    pub duration_seconds: f64,
    pub language: Option<String>,
}

/// Error during retranscription
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetranscriptionError {
    pub meeting_id: String,
    pub error: String,
}

/// Check if retranscription is currently in progress
pub fn is_retranscription_in_progress() -> bool {
    RETRANSCRIPTION_IN_PROGRESS.load(Ordering::SeqCst)
}

/// Cancel ongoing retranscription
pub fn cancel_retranscription() {
    RETRANSCRIPTION_CANCELLED.store(true, Ordering::SeqCst);
}

/// Start retranscription of a meeting's audio
pub async fn start_retranscription<R: Runtime>(
    app: AppHandle<R>,
    meeting_id: String,
    meeting_folder_path: String,
    language: Option<String>,
    model: Option<String>,
    provider: Option<String>,
    assign_speakers: bool,
) -> Result<RetranscriptionResult> {
    let _speaker_operation = crate::diarization::operations::try_meeting_edit(&meeting_id)
        .map_err(|error| anyhow!(error))?;
    // Acquire guard - ensures flag is cleared even on panic/early return
    let _guard = RetranscriptionGuard::acquire().map_err(|e| anyhow!(e))?;

    // Reset cancellation flag
    RETRANSCRIPTION_CANCELLED.store(false, Ordering::SeqCst);

    let _processing = if assign_speakers {
        let state = app.state::<AppState>();
        let settings =
            crate::database::repositories::diarization::DiarizationRepository::get_settings(
                state.speaker_store.pool(),
            )
            .await?;
        if settings.enabled == 0 {
            return Err(anyhow!("Enable speaker labels before assigning speakers."));
        }
        Some(
            crate::diarization::operations::try_processing_slot()
                .map_err(|error| anyhow!(error))?,
        )
    } else {
        None
    };
    let batch_engine = super::common::BatchEngine::from_provider(provider.as_deref());
    let result = run_retranscription(
        app.clone(),
        meeting_id.clone(),
        meeting_folder_path,
        language,
        model,
        provider,
        assign_speakers,
    )
    .await;

    // Unload the engine after the batch job (success, failure, or cancellation)
    super::common::unload_engine_after_batch(batch_engine).await;

    // Guard will automatically clear flag on drop
    // No need for manual: RETRANSCRIPTION_IN_PROGRESS.store(false, Ordering::SeqCst);

    match &result {
        Ok(res) => {
            let _ = app.emit(
                "retranscription-complete",
                serde_json::json!({
                    "meeting_id": res.meeting_id,
                    "segments_count": res.segments_count,
                    "duration_seconds": res.duration_seconds,
                    "language": res.language
                }),
            );
        }
        Err(e) => {
            let _ = app.emit(
                "retranscription-error",
                RetranscriptionError {
                    meeting_id: meeting_id.clone(),
                    error: e.to_string(),
                },
            );
        }
    }

    result
}

/// Find audio file in meeting folder
/// Tries common names first, then scans for any file with an audio extension
fn find_audio_file(folder: &Path) -> Result<PathBuf> {
    let candidates = [
        "audio.mp4",
        "audio.m4a",
        "audio.wav",
        "audio.mp3",
        "audio.flac",
        "audio.ogg",
        "recording.mp4",
        "audio.mkv",
        "audio.webm",
        "audio.wma",
    ];

    for name in candidates {
        let path = folder.join(name);
        if path.exists() {
            return Ok(path);
        }
    }

    // Fallback: scan folder for any file with an audio extension
    if let Ok(entries) = std::fs::read_dir(folder) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(ext) = path.extension() {
                let ext = ext.to_string_lossy().to_lowercase();
                if AUDIO_EXTENSIONS.contains(&ext.as_str()) {
                    return Ok(path);
                }
            }
        }
    }

    Err(anyhow!("No audio file found in: {}", folder.display()))
}

/// Internal function to run retranscription
async fn run_retranscription<R: Runtime>(
    app: AppHandle<R>,
    meeting_id: String,
    meeting_folder_path: String,
    language: Option<String>,
    model: Option<String>,
    provider: Option<String>,
    assign_speakers: bool,
) -> Result<RetranscriptionResult> {
    let folder_path = PathBuf::from(&meeting_folder_path);
    let audio_path = find_audio_file(&folder_path)?;
    let original = crate::diarization::finalize::load_transcripts(
        app.state::<AppState>().db_manager.pool(),
        &meeting_id,
    )
    .await?;

    // Determine which provider to use (default to whisper)
    let batch_engine = super::common::BatchEngine::from_provider(provider.as_deref());
    let use_parakeet = batch_engine == super::common::BatchEngine::Parakeet;

    info!(
        "Starting retranscription for meeting {} with language {:?}, model {:?}, provider {:?}",
        meeting_id, language, model, provider
    );

    // Emit progress: decoding
    emit_progress(&app, &meeting_id, "decoding", 5, "Decoding audio file...");

    // Check for cancellation
    if RETRANSCRIPTION_CANCELLED.load(Ordering::SeqCst) {
        return Err(anyhow!("Retranscription cancelled"));
    }

    // Decode the audio file (CPU-intensive, run in blocking task)
    let path_for_decode = audio_path.clone();
    let decoded = tokio::task::spawn_blocking(move || decode_audio_file(&path_for_decode))
        .await
        .map_err(|e| anyhow!("Decode task panicked: {}", e))??;
    let duration_seconds = decoded.duration_seconds;

    info!(
        "Decoded audio: {:.2}s, {}Hz, {} channels",
        duration_seconds, decoded.sample_rate, decoded.channels
    );

    emit_progress(
        &app,
        &meeting_id,
        "decoding",
        15,
        "Converting audio format...",
    );

    // Check for cancellation
    if RETRANSCRIPTION_CANCELLED.load(Ordering::SeqCst) {
        return Err(anyhow!("Retranscription cancelled"));
    }

    // Convert to 16kHz mono format (CPU-intensive, run in blocking task)
    let audio_samples = tokio::task::spawn_blocking(move || decoded.to_whisper_format())
        .await
        .map_err(|e| anyhow!("Resample task panicked: {}", e))?;
    info!(
        "Converted to 16kHz mono format: {} samples",
        audio_samples.len()
    );

    let speaker_analysis = if assign_speakers {
        emit_progress(
            &app,
            &meeting_id,
            "speakers",
            18,
            "Identifying speakers in the recording...",
        );
        Some(
            crate::diarization::finalize::prepare_enhancement(&app, &meeting_id, &folder_path, &RETRANSCRIPTION_CANCELLED)
                .await?,
        )
    } else {
        None
    };

    emit_progress(&app, &meeting_id, "vad", 20, "Detecting speech segments...");

    // Check for cancellation
    if RETRANSCRIPTION_CANCELLED.load(Ordering::SeqCst) {
        return Err(anyhow!("Retranscription cancelled"));
    }

    // Use VAD to find natural speech boundaries (same approach as live transcription)
    // IMPORTANT: Run VAD in a blocking task to avoid blocking the async runtime
    // For large files (35+ minutes), VAD processing can take several minutes
    let app_for_vad = app.clone();
    let meeting_id_for_vad = meeting_id.clone();

    let redemption_time_ms = if assign_speakers {
        SPEAKER_VAD_REDEMPTION_TIME_MS
    } else {
        VAD_REDEMPTION_TIME_MS
    };
    let speech_segments = tokio::task::spawn_blocking(move || {
        get_speech_chunks_with_progress(
            &audio_samples,
            redemption_time_ms,
            |vad_progress, segments_found| {
                // Map VAD progress (0-100) to overall progress (20-25)
                let overall_progress = 20 + (vad_progress as f32 * 0.05) as u32;
                emit_progress(
                    &app_for_vad,
                    &meeting_id_for_vad,
                    "vad",
                    overall_progress,
                    &format!(
                        "Detecting speech segments... {}% ({} found)",
                        vad_progress, segments_found
                    ),
                );

                // Return false to cancel if cancellation requested
                !RETRANSCRIPTION_CANCELLED.load(Ordering::SeqCst)
            },
        )
    })
    .await
    .map_err(|e| anyhow!("VAD task panicked: {}", e))?
    .map_err(|e| anyhow!("VAD processing failed: {}", e))?;

    let total_segments = speech_segments.len();
    info!(
        "VAD detected {} speech segments (redemption_time={}ms)",
        total_segments, redemption_time_ms
    );

    // Diagnostic: log segment duration distribution
    if !speech_segments.is_empty() {
        let durations_ms: Vec<f64> = speech_segments
            .iter()
            .map(|s| s.end_timestamp_ms - s.start_timestamp_ms)
            .collect();
        let total_speech_ms: f64 = durations_ms.iter().sum();
        let avg_duration = total_speech_ms / durations_ms.len() as f64;
        let min_duration = durations_ms.iter().cloned().fold(f64::INFINITY, f64::min);
        let max_duration = durations_ms
            .iter()
            .cloned()
            .fold(f64::NEG_INFINITY, f64::max);
        info!(
            "VAD segment stats: avg={:.0}ms, min={:.0}ms, max={:.0}ms, total_speech={:.1}s/{:.1}s ({:.0}%)",
            avg_duration, min_duration, max_duration,
            total_speech_ms / 1000.0, duration_seconds,
            (total_speech_ms / 1000.0 / duration_seconds) * 100.0
        );
        // Log first 10 segments for detailed inspection
        for (i, seg) in speech_segments.iter().take(10).enumerate() {
            let dur = seg.end_timestamp_ms - seg.start_timestamp_ms;
            debug!(
                "  Segment {}: {:.0}ms-{:.0}ms ({:.0}ms, {} samples)",
                i,
                seg.start_timestamp_ms,
                seg.end_timestamp_ms,
                dur,
                seg.samples.len()
            );
        }
        if total_segments > 10 {
            debug!("  ... and {} more segments", total_segments - 10);
        }
    }

    if total_segments == 0 {
        warn!("No speech detected in audio");
        return Err(anyhow!("No speech detected in audio file"));
    }

    emit_progress(
        &app,
        &meeting_id,
        "transcribing",
        25,
        "Loading transcription engine...",
    );

    // Initialize the appropriate engine once (not per-segment)
    let whisper_engine = if batch_engine == super::common::BatchEngine::Whisper {
        Some(get_or_init_whisper(&app, model.as_deref()).await?)
    } else {
        None
    };
    let parakeet_engine = if batch_engine == super::common::BatchEngine::Parakeet {
        Some(get_or_init_parakeet(&app, model.as_deref()).await?)
    } else {
        None
    };
    let funasr_provider = if batch_engine == super::common::BatchEngine::FunAsr {
        let engine = super::common::get_or_init_funasr(&app, model.as_deref(), language.as_deref()).await?;
        Some(crate::audio::transcription::FunAsrProvider::new(engine))
    } else {
        None
    };

    // Keep input context for Parakeet, but use its token times for output rows.
    // Whisper and FUN-ASR have no word timing on this path. When assigning
    // speakers they get shorter, turn-aware input. Otherwise keep longer input.
    let max_samples = if assign_speakers && !use_parakeet {
        SPEAKER_WHISPER_MAX_SEGMENT_SAMPLES
    } else {
        MAX_SEGMENT_SAMPLES
    };
    let turns = if use_parakeet {
        Vec::new()
    } else {
        speaker_analysis
            .as_ref()
            .map(|analysis| analysis.turn_boundaries())
            .unwrap_or_default()
    };
    let mut processable_segments = Vec::new();
    for segment in &speech_segments {
        processable_segments.extend(super::enhancement::input_chunks(
            segment,
            max_samples,
            &turns,
        ));
    }

    let processable_count = processable_segments.len();
    info!(
        "Processing {} segments (after splitting)",
        processable_count
    );

    // Process each speech segment with progress updates
    let mut all_transcripts: Vec<(String, f64, f64)> = Vec::new(); // (text, start_ms, end_ms)
    let mut total_confidence = 0.0f32;

    for (i, segment) in processable_segments.iter().enumerate() {
        // Check for cancellation before each segment
        if RETRANSCRIPTION_CANCELLED.load(Ordering::SeqCst) {
            return Err(anyhow!("Retranscription cancelled"));
        }

        // Calculate progress (25% to 80% range for transcription)
        let progress = 25 + ((i as f32 / processable_count as f32) * 55.0) as u32;
        let segment_duration_sec = (segment.end_timestamp_ms - segment.start_timestamp_ms) / 1000.0;
        emit_progress(
            &app,
            &meeting_id,
            "transcribing",
            progress,
            &format!(
                "Transcribing segment {} of {} ({:.1}s)...",
                i + 1,
                processable_count,
                segment_duration_sec
            ),
        );

        // Skip very short segments (< 100ms of audio = 1600 samples at 16kHz)
        if segment.samples.len() < 1600 {
            debug!(
                "Skipping short segment {} with {} samples",
                i,
                segment.samples.len()
            );
            continue;
        }

        let (rows, confidence) = if let Some(provider) = funasr_provider.as_ref() {
            use crate::audio::transcription::TranscriptionProvider;
            let result = provider
                .transcribe(segment.samples.clone(), language.clone())
                .await
                .map_err(|e| anyhow!("FUN-ASR transcription failed on segment {}: {}", i, e))?;
            // FUN-ASR has no word times. Keep the VAD and speaker-turn spans
            // selected above, without changing Parakeet's token-timed rows.
            let rows = if result.text.trim().is_empty() {
                Vec::new()
            } else {
                vec![(result.text, segment.start_timestamp_ms, segment.end_timestamp_ms)]
            };
            (rows, 0.9f32)
        } else if use_parakeet {
            let result = parakeet_engine
                .as_ref()
                .unwrap()
                .transcribe_audio_timed(segment.samples.clone())
                .await
                .map_err(|error| {
                    anyhow!("Parakeet transcription failed on segment {i}: {error}")
                })?;
            let rows = if result.text.trim().is_empty() {
                Vec::new()
            } else {
                super::enhancement::parakeet_rows(
                    &result,
                    segment.start_timestamp_ms,
                    segment.end_timestamp_ms - segment.start_timestamp_ms,
                    speaker_analysis.as_ref(),
                )?
            };
            (rows, 0.9f32)
        } else {
            let (text, confidence, _) = whisper_engine
                .as_ref()
                .unwrap()
                .transcribe_audio_with_confidence(segment.samples.clone(), language.clone())
                .await
                .map_err(|error| anyhow!("Whisper transcription failed on segment {i}: {error}"))?;
            let rows = if text.trim().is_empty() {
                Vec::new()
            } else {
                vec![(text, segment.start_timestamp_ms, segment.end_timestamp_ms)]
            };
            (rows, confidence)
        };
        total_confidence += confidence * rows.len() as f32;
        all_transcripts.extend(rows);
    }

    let transcribed_count = all_transcripts.len();
    let avg_confidence = if transcribed_count > 0 {
        total_confidence / transcribed_count as f32
    } else {
        0.0
    };

    info!(
        "Transcription complete: {} segments transcribed out of {}, avg confidence: {:.2}",
        transcribed_count, processable_count, avg_confidence
    );

    // Check for cancellation
    if RETRANSCRIPTION_CANCELLED.load(Ordering::SeqCst) {
        return Err(anyhow!("Retranscription cancelled"));
    }

    emit_progress(&app, &meeting_id, "saving", 80, "Saving transcripts...");

    // Create transcript segments with proper timestamps from VAD
    let segments = create_transcript_segments(&all_transcripts);

    let rows = super::enhancement::transcript_rows(&meeting_id, &segments);
    let labels = speaker_analysis
        .map(|analysis| analysis.label(&rows))
        .transpose()?;
    let state = app.state::<AppState>();
    super::enhancement::publish(
        state.db_manager.pool(),
        state.speaker_store.pool(),
        &meeting_id,
        &original,
        &segments,
        labels.as_ref(),
        &RETRANSCRIPTION_CANCELLED,
    )
    .await?;
    let _ = app.emit("diarization-progress", serde_json::json!({
        "meetingId": meeting_id, "status": if labels.is_some() { "completed" } else { "none" }, "progress": if labels.is_some() {100} else {0},
    }));

    // Write updated transcripts.json and metadata.json to the meeting folder
    emit_progress(
        &app,
        &meeting_id,
        "saving",
        90,
        "Writing transcript files...",
    );

    if let Err(e) = write_transcripts_json(&folder_path, &segments) {
        warn!("Failed to write transcripts.json: {}", e);
    }

    // Find audio filename for metadata
    let audio_filename = audio_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("audio.mp4")
        .to_string();

    if let Err(e) =
        write_retranscription_metadata(&folder_path, &meeting_id, duration_seconds, &audio_filename)
    {
        warn!("Failed to update metadata.json: {}", e);
    }

    emit_progress(
        &app,
        &meeting_id,
        "complete",
        100,
        "Retranscription complete",
    );

    Ok(RetranscriptionResult {
        meeting_id,
        segments_count: segments.len(),
        duration_seconds,
        language,
    })
}

/// Emit progress event
fn emit_progress<R: Runtime>(
    app: &AppHandle<R>,
    meeting_id: &str,
    stage: &str,
    progress: u32,
    message: &str,
) {
    let _ = app.emit(
        "retranscription-progress",
        RetranscriptionProgress {
            meeting_id: meeting_id.to_string(),
            stage: stage.to_string(),
            progress_percentage: progress,
            message: message.to_string(),
        },
    );
}

/// Get or initialize the Whisper engine, auto-loading the model if needed
/// If `requested_model` is provided, ensures that specific model is loaded
async fn get_or_init_whisper<R: Runtime>(
    app: &AppHandle<R>,
    requested_model: Option<&str>,
) -> Result<Arc<WhisperEngine>> {
    use crate::whisper_engine::commands::WHISPER_ENGINE;

    let engine = {
        let guard = WHISPER_ENGINE.lock().unwrap_or_else(|e| e.into_inner());
        guard.as_ref().cloned()
    };

    match engine {
        Some(e) => {
            // Determine which model to use
            let target_model = match requested_model {
                Some(model) => model.to_string(),
                None => get_configured_whisper_model(app).await?,
            };

            // Check if the correct model is already loaded
            let current_model = e.get_current_model().await;
            let needs_load = match &current_model {
                Some(loaded) => loaded != &target_model,
                None => true,
            };

            if needs_load {
                info!(
                    "Loading Whisper model '{}' (current: {:?})",
                    target_model, current_model
                );

                // Discover available models first (populates the internal cache)
                info!("Discovering available Whisper models...");
                if let Err(discover_err) = e.discover_models().await {
                    warn!(
                        "Error during model discovery (continuing anyway): {}",
                        discover_err
                    );
                }

                match e.load_model(&target_model).await {
                    Ok(_) => {
                        info!("Whisper model '{}' loaded successfully", target_model);
                        Ok(e)
                    }
                    Err(load_err) => {
                        error!(
                            "Failed to load Whisper model '{}': {}",
                            target_model, load_err
                        );
                        Err(anyhow!(
                            "Failed to load Whisper model '{}': {}",
                            target_model,
                            load_err
                        ))
                    }
                }
            } else {
                info!("Whisper model '{}' already loaded", target_model);
                Ok(e)
            }
        }
        None => Err(anyhow!("Whisper engine not initialized")),
    }
}

/// Get the configured Whisper model name from the database
async fn get_configured_whisper_model<R: Runtime>(app: &AppHandle<R>) -> Result<String> {
    debug!("Getting configured Whisper model from database...");

    let app_state = app.try_state::<AppState>().ok_or_else(|| {
        error!("App state not available");
        anyhow!("App state not available")
    })?;

    debug!("Querying transcript_settings table...");

    // Query the transcript settings from the database - get both provider and model
    let result: Option<(String, String)> =
        sqlx::query_as("SELECT provider, model FROM transcript_settings WHERE id = '1'")
            .fetch_optional(app_state.db_manager.pool())
            .await
            .map_err(|e| {
                error!("Failed to query transcript config: {}", e);
                anyhow!("Failed to query transcript config: {}", e)
            })?;

    match result {
        Some((provider, model)) => {
            info!(
                "Found transcript config: provider={}, model={}",
                provider, model
            );

            // Check if provider is Whisper-based
            if provider == "localWhisper" || provider == "whisper" {
                Ok(model)
            } else {
                error!(
                    "Retranscription requires Whisper provider, but configured provider is: {}",
                    provider
                );
                Err(anyhow!("Retranscription requires Whisper. Current provider '{}' does not support retranscription with language selection.", provider))
            }
        }
        None => {
            // Default to configured Whisper model if no config exists
            warn!(
                "No transcript config found, using default model '{}'",
                DEFAULT_WHISPER_MODEL
            );
            Ok(DEFAULT_WHISPER_MODEL.to_string())
        }
    }
}

/// Get or initialize the Parakeet engine, auto-loading the model if needed
async fn get_or_init_parakeet<R: Runtime>(
    app: &AppHandle<R>,
    requested_model: Option<&str>,
) -> Result<Arc<ParakeetEngine>> {
    use crate::parakeet_engine::commands::PARAKEET_ENGINE;

    let engine = {
        let guard = PARAKEET_ENGINE.lock().unwrap_or_else(|e| e.into_inner());
        guard.as_ref().cloned()
    };

    match engine {
        Some(e) => {
            // Determine which model to use
            let target_model = match requested_model {
                Some(model) => model.to_string(),
                None => get_configured_parakeet_model(app).await?,
            };

            // Check if the correct model is already loaded
            let current_model = e.get_current_model().await;
            let needs_load = match &current_model {
                Some(loaded) => loaded != &target_model,
                None => true,
            };

            if needs_load {
                info!(
                    "Loading Parakeet model '{}' (current: {:?})",
                    target_model, current_model
                );

                // Discover available models first
                info!("Discovering available Parakeet models...");
                if let Err(discover_err) = e.discover_models().await {
                    warn!(
                        "Error during Parakeet model discovery (continuing anyway): {}",
                        discover_err
                    );
                }

                match e.load_model(&target_model).await {
                    Ok(_) => {
                        info!("Parakeet model '{}' loaded successfully", target_model);
                        Ok(e)
                    }
                    Err(load_err) => {
                        error!(
                            "Failed to load Parakeet model '{}': {}",
                            target_model, load_err
                        );
                        Err(anyhow!(
                            "Failed to load Parakeet model '{}': {}",
                            target_model,
                            load_err
                        ))
                    }
                }
            } else {
                info!("Parakeet model '{}' already loaded", target_model);
                Ok(e)
            }
        }
        None => Err(anyhow!("Parakeet engine not initialized")),
    }
}

/// Get the configured Parakeet model name from the database
async fn get_configured_parakeet_model<R: Runtime>(app: &AppHandle<R>) -> Result<String> {
    debug!("Getting configured Parakeet model from database...");

    let app_state = app.try_state::<AppState>().ok_or_else(|| {
        error!("App state not available");
        anyhow!("App state not available")
    })?;

    // Query the transcript settings from the database
    let result: Option<(String, String)> =
        sqlx::query_as("SELECT provider, model FROM transcript_settings WHERE id = '1'")
            .fetch_optional(app_state.db_manager.pool())
            .await
            .map_err(|e| {
                error!("Failed to query transcript config: {}", e);
                anyhow!("Failed to query transcript config: {}", e)
            })?;

    match result {
        Some((provider, model)) => {
            info!(
                "Found transcript config: provider={}, model={}",
                provider, model
            );

            if provider == "parakeet" {
                Ok(model)
            } else {
                // Default to configured Parakeet model
                warn!("Configured provider is not Parakeet, using default model");
                Ok(DEFAULT_PARAKEET_MODEL.to_string())
            }
        }
        None => {
            // Default to configured Parakeet model if no config exists
            warn!("No transcript config found, using default Parakeet model");
            Ok(DEFAULT_PARAKEET_MODEL.to_string())
        }
    }
}

/// Write or update metadata.json for retranscription (preserves existing fields, adds retranscribed_at)
fn write_retranscription_metadata(
    folder: &Path,
    meeting_id: &str,
    duration_seconds: f64,
    audio_filename: &str,
) -> Result<()> {
    let metadata_path = folder.join("metadata.json");
    let temp_path = folder.join(".metadata.json.tmp");
    let now = chrono::Utc::now().to_rfc3339();

    // Try to read existing metadata and update it
    let json = if metadata_path.exists() {
        let existing = std::fs::read_to_string(&metadata_path)?;
        let mut value: serde_json::Value = serde_json::from_str(&existing)?;
        if let Some(obj) = value.as_object_mut() {
            obj.insert(
                "duration_seconds".to_string(),
                serde_json::json!(duration_seconds),
            );
            obj.insert("retranscribed_at".to_string(), serde_json::json!(now));
            obj.insert("status".to_string(), serde_json::json!("completed"));
            obj.insert(
                "transcript_file".to_string(),
                serde_json::json!("transcripts.json"),
            );
            obj.remove("detected_summary_language");
        }
        value
    } else {
        serde_json::json!({
            "version": "1.0",
            "meeting_id": meeting_id,
            "created_at": now,
            "completed_at": now,
            "retranscribed_at": now,
            "duration_seconds": duration_seconds,
            "audio_file": audio_filename,
            "transcript_file": "transcripts.json",
            "status": "completed",
            "source": "retranscription"
        })
    };

    let json_string = serde_json::to_string_pretty(&json)?;
    std::fs::write(&temp_path, &json_string)?;
    std::fs::rename(&temp_path, &metadata_path)?;

    info!("Wrote metadata.json to {}", metadata_path.display());
    Ok(())
}

// Tauri commands

// Start retranscription (Beta gated using configContext.betaFeatures)
#[tauri::command]
pub async fn start_retranscription_command<R: Runtime>(
    app: AppHandle<R>,
    meeting_id: String,
    meeting_folder_path: String,
    language: Option<String>,
    model: Option<String>,
    provider: Option<String>,
    assign_speakers: Option<bool>,
) -> Result<RetranscriptionResult, String> {
    // Keep the command future alive so its caller receives every failure,
    // including errors before progress events can be emitted.
    start_retranscription(
        app, meeting_id, meeting_folder_path, language, model, provider,
        assign_speakers.unwrap_or(false),
    ).await.map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn cancel_retranscription_command() -> Result<(), String> {
    if !is_retranscription_in_progress() {
        return Err("No retranscription in progress".to_string());
    }
    cancel_retranscription();
    Ok(())
}

#[tauri::command]
pub async fn is_retranscription_in_progress_command() -> bool {
    is_retranscription_in_progress()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_transcript_segments_empty() {
        let transcripts: Vec<(String, f64, f64)> = vec![];
        let segments = create_transcript_segments(&transcripts);
        assert!(segments.is_empty());
    }

    #[test]
    fn test_create_transcript_segments_single() {
        let transcripts = vec![
            ("Hello world".to_string(), 0.0, 1500.0), // 0-1.5 seconds
        ];
        let segments = create_transcript_segments(&transcripts);

        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].text, "Hello world");
        assert_eq!(segments[0].audio_start_time, Some(0.0));
        assert_eq!(segments[0].audio_end_time, Some(1.5));
        assert_eq!(segments[0].duration, Some(1.5));
    }

    #[test]
    fn test_create_transcript_segments_multiple() {
        let transcripts = vec![
            ("First segment".to_string(), 0.0, 2000.0), // 0-2 seconds
            ("Second segment".to_string(), 3000.0, 5000.0), // 3-5 seconds
            ("Third segment".to_string(), 6500.0, 8000.0), // 6.5-8 seconds
        ];
        let segments = create_transcript_segments(&transcripts);

        assert_eq!(segments.len(), 3);

        // First segment
        assert_eq!(segments[0].text, "First segment");
        assert_eq!(segments[0].audio_start_time, Some(0.0));
        assert_eq!(segments[0].audio_end_time, Some(2.0));
        assert_eq!(segments[0].duration, Some(2.0));

        // Second segment
        assert_eq!(segments[1].text, "Second segment");
        assert_eq!(segments[1].audio_start_time, Some(3.0));
        assert_eq!(segments[1].audio_end_time, Some(5.0));
        assert_eq!(segments[1].duration, Some(2.0));

        // Third segment
        assert_eq!(segments[2].text, "Third segment");
        assert_eq!(segments[2].audio_start_time, Some(6.5));
        assert_eq!(segments[2].audio_end_time, Some(8.0));
        assert_eq!(segments[2].duration, Some(1.5));
    }

    #[test]
    fn test_create_transcript_segments_trims_whitespace() {
        let transcripts = vec![("  Hello with spaces  ".to_string(), 0.0, 1000.0)];
        let segments = create_transcript_segments(&transcripts);

        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].text, "Hello with spaces");
    }

    #[test]
    fn test_create_transcript_segments_generates_unique_ids() {
        let transcripts = vec![
            ("Segment one".to_string(), 0.0, 1000.0),
            ("Segment two".to_string(), 1000.0, 2000.0),
        ];
        let segments = create_transcript_segments(&transcripts);

        assert_eq!(segments.len(), 2);
        assert_ne!(segments[0].id, segments[1].id);
        assert!(segments[0].id.starts_with("transcript-"));
        assert!(segments[1].id.starts_with("transcript-"));
    }

    #[test]
    fn test_cancellation_flag() {
        // Reset flag to known state
        RETRANSCRIPTION_CANCELLED.store(false, Ordering::SeqCst);
        RETRANSCRIPTION_IN_PROGRESS.store(false, Ordering::SeqCst);

        assert!(!is_retranscription_in_progress());

        // Test cancellation
        cancel_retranscription();
        assert!(RETRANSCRIPTION_CANCELLED.load(Ordering::SeqCst));

        // Reset for other tests
        RETRANSCRIPTION_CANCELLED.store(false, Ordering::SeqCst);
    }

    #[test]
    fn test_vad_redemption_time_constant() {
        // Batch processing uses 2000ms to bridge natural pauses in full-file VAD.
        // Only speaker assignment keeps short pauses apart.
        assert_eq!(VAD_REDEMPTION_TIME_MS, 2000);
        assert_eq!(SPEAKER_VAD_REDEMPTION_TIME_MS, 500);
        assert_eq!(MAX_SEGMENT_SAMPLES, 25 * 16000);
    }

    #[test]
    fn test_find_audio_file_common_candidates() {
        let dir = tempfile::tempdir().unwrap();

        // No audio file → error
        assert!(find_audio_file(dir.path()).is_err());

        // Create audio.mp4 — should be found first
        std::fs::write(dir.path().join("audio.mp4"), b"fake").unwrap();
        let found = find_audio_file(dir.path()).unwrap();
        assert_eq!(found.file_name().unwrap(), "audio.mp4");
    }

    #[test]
    fn test_find_audio_file_non_mp4_extensions() {
        let dir = tempfile::tempdir().unwrap();

        // Create audio.wav (imported as .wav, not .mp4)
        std::fs::write(dir.path().join("audio.wav"), b"fake").unwrap();
        let found = find_audio_file(dir.path()).unwrap();
        assert_eq!(found.file_name().unwrap(), "audio.wav");
    }

    #[test]
    fn test_find_audio_file_fallback_scan() {
        let dir = tempfile::tempdir().unwrap();

        // Create a file with an audio extension but non-standard name
        std::fs::write(dir.path().join("my_recording.flac"), b"fake").unwrap();
        // Also add a non-audio file that should be ignored
        std::fs::write(dir.path().join("notes.txt"), b"text").unwrap();

        let found = find_audio_file(dir.path()).unwrap();
        assert_eq!(found.file_name().unwrap(), "my_recording.flac");
    }

    #[test]
    fn test_find_audio_file_priority_order() {
        let dir = tempfile::tempdir().unwrap();

        // Create both audio.m4a and audio.mp4 — mp4 should win (listed first in candidates)
        std::fs::write(dir.path().join("audio.m4a"), b"fake").unwrap();
        std::fs::write(dir.path().join("audio.mp4"), b"fake").unwrap();
        let found = find_audio_file(dir.path()).unwrap();
        assert_eq!(found.file_name().unwrap(), "audio.mp4");
    }

    #[test]
    fn test_find_audio_file_empty_folder() {
        let dir = tempfile::tempdir().unwrap();
        let result = find_audio_file(dir.path());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No audio file found"));
    }

    #[test]
    fn test_find_audio_file_nonexistent_folder() {
        let result = find_audio_file(Path::new("/nonexistent/path/12345"));
        assert!(result.is_err());
    }

    #[test]
    fn test_audio_extensions_constant() {
        // Verify all expected formats are covered
        assert!(AUDIO_EXTENSIONS.contains(&"mp4"));
        assert!(AUDIO_EXTENSIONS.contains(&"m4a"));
        assert!(AUDIO_EXTENSIONS.contains(&"wav"));
        assert!(AUDIO_EXTENSIONS.contains(&"mp3"));
        assert!(AUDIO_EXTENSIONS.contains(&"flac"));
        assert!(AUDIO_EXTENSIONS.contains(&"ogg"));
        assert!(AUDIO_EXTENSIONS.contains(&"aac"));
        // FFmpeg-backed formats
        assert!(AUDIO_EXTENSIONS.contains(&"mkv"));
        assert!(AUDIO_EXTENSIONS.contains(&"webm"));
        assert!(AUDIO_EXTENSIONS.contains(&"wma"));
        // Non-audio formats
        assert!(!AUDIO_EXTENSIONS.contains(&"txt"));
        assert!(!AUDIO_EXTENSIONS.contains(&"pdf"));
    }

    #[test]
    fn retranscription_metadata_replaces_stale_duration_without_losing_existing_fields() {
        let dir = tempfile::tempdir().unwrap();
        let metadata_path = dir.path().join("metadata.json");
        std::fs::write(
            &metadata_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "meeting_id": "existing-meeting",
                "duration_seconds": 2523.2,
                "audio_file": "recording.m4a",
                "summary_language": "fr",
                "detected_summary_language": "en",
                "custom_field": "preserve me"
            }))
            .unwrap(),
        )
        .unwrap();

        write_retranscription_metadata(
            dir.path(),
            "ignored-for-existing-metadata",
            5046.4,
            "recording.m4a",
        )
        .unwrap();

        let metadata: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(metadata_path).unwrap()).unwrap();
        assert_eq!(metadata["duration_seconds"], 5046.4);
        assert_eq!(metadata["meeting_id"], "existing-meeting");
        assert_eq!(metadata["audio_file"], "recording.m4a");
        assert_eq!(metadata["summary_language"], "fr");
        assert_eq!(metadata["custom_field"], "preserve me");
        assert!(metadata.get("detected_summary_language").is_none());
    }
}
