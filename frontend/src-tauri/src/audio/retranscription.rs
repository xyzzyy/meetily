// Retranscription module - allows re-processing stored audio with different settings

use super::common::{create_transcript_segments, write_transcripts_json};
use super::constants::AUDIO_EXTENSIONS;
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

/// Detect short speech spans, then keep nearby phrases and pauses together.
/// This is recording preparation, so all Enhance engines use the same rule.
const ENHANCEMENT_VAD_REDEMPTION_TIME_MS: u32 = 500;

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
    pub review_id: String,
    pub warning: Option<String>,
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
                    "language": res.language,
                    "review_id": res.review_id,
                    "warning": res.warning
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
    let use_nemotron = batch_engine == super::common::BatchEngine::Nemotron;

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
    let decoded = tokio::task::spawn_blocking(move || {
        super::enhancement::decode_enhancement_audio(&path_for_decode)
    })
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

    if use_nemotron && assign_speakers {
        // Convert to 16kHz mono format (CPU-intensive, run in blocking task)
        let audio_samples = tokio::task::spawn_blocking(move || decoded.to_whisper_format())
            .await
            .map_err(|e| anyhow!("Resample task panicked: {}", e))?;
        info!(
            "Converted to 16kHz mono format: {} samples",
            audio_samples.len()
        );
        let (transcripts, analysis) =
            nemotron_with_speakers(&app, &meeting_id, &folder_path, model.as_deref(), &audio_samples, &original)
                .await?;
        let transcript_sources = vec![None; transcripts.len()];
        return save_retranscription(
            &app,
            meeting_id,
            meeting_folder_path,
            original,
            duration_seconds,
            language,
            transcripts,
            Some(analysis),
            transcript_sources,
            provider,
            model,
        )
        .await;
    }

    let speaker_analysis = if assign_speakers {
        emit_progress(
            &app,
            &meeting_id,
            "speakers",
            18,
            "Identifying speakers in the recording...",
        );
        Some(
            crate::diarization::finalize::prepare_enhancement(
                &app,
                &meeting_id,
                &folder_path,
                &RETRANSCRIPTION_CANCELLED,
            )
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

    // Speaker estimates describe output labels, not safe cuts in the audio.
    // Keep the same speech context with or without speaker assignment.
    let redemption_time_ms = ENHANCEMENT_VAD_REDEMPTION_TIME_MS;
    let known_speech = original.clone();
    let speech_segments = tokio::task::spawn_blocking(move || {
        let on_progress = |vad_progress, segments_found| {
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
            !RETRANSCRIPTION_CANCELLED.load(Ordering::SeqCst)
        };
        super::enhancement::recognition_inputs(
            &decoded,
            batch_engine,
            redemption_time_ms,
            &known_speech,
            on_progress,
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
        let engine =
            super::common::get_or_init_funasr(&app, model.as_deref(), language.as_deref()).await?;
        Some(crate::audio::transcription::FunAsrProvider::new(engine))
    } else {
        None
    };

    let nemotron_provider = if use_nemotron {
        Some(
            crate::nemotron::configured_provider(&app, model.as_deref())
                .await
                .map_err(anyhow::Error::msg)?,
        )
    } else {
        None
    };

    // Transcribe complete speech spans before assigning speakers. A change on
    // one track can occur during speech on the other track; cutting the mixed
    // audio at that estimate can remove word context on both sides of the cut.
    let mut processable_segments = Vec::new();
    for segment in &speech_segments {
        processable_segments.extend(
            super::enhancement::input_chunks(
                segment,
                batch_engine.enhancement_input_policy().max_input_samples,
            )
            .into_iter()
            .map(|chunk| super::enhancement::RecognitionInput {
                segment: chunk,
                source: segment.source.clone(),
            }),
        );
    }

    let processable_count = processable_segments.len();
    info!(
        "Processing {} segments (after splitting)",
        processable_count
    );

    // Process each speech segment with progress updates
    let mut all_transcripts: Vec<(String, f64, f64)> = Vec::new(); // (text, start_ms, end_ms)
    let mut total_confidence = 0.0f32;
    let mut transcript_sources = Vec::new();

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

        let (rows, confidence) = if let Some(provider) = nemotron_provider.as_ref() {
            let words = provider
                .transcribe_words(&segment.samples)
                .await
                .map_err(|e| anyhow!("Nemotron transcription failed on segment {i}: {e}"))?;
            let words = words
                .into_iter()
                .map(|(text, start, end)| {
                    (
                        text,
                        segment.start_timestamp_ms + start,
                        segment.start_timestamp_ms + end,
                    )
                })
                .collect();
            (
                super::enhancement::word_rows(
                    words,
                    speaker_analysis.as_ref(),
                    segment.source.as_deref(),
                ),
                0.9f32,
            )
        } else if let Some(provider) = funasr_provider.as_ref() {
            use crate::audio::transcription::TranscriptionProvider;
            let result = provider
                .transcribe(segment.samples.clone(), language.clone())
                .await
                .map_err(|e| anyhow!("FUN-ASR transcription failed on segment {}: {}", i, e))?;
            // FUN-ASR has no word times. Keep the complete VAD spans
            // selected above, without changing Parakeet's token-timed rows.
            let rows = super::enhancement::untimed_row(
                result.text,
                segment.start_timestamp_ms,
                segment.end_timestamp_ms,
            )
            .into_iter()
            .collect();
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
                    segment.source.as_deref(),
                )?
            };
            (rows, 0.9f32)
        } else {
            let (text, confidence, _) = whisper_engine
                .as_ref()
                .unwrap()
                .transcribe_batch_audio_with_confidence(segment.samples.clone(), language.clone())
                .await
                .map_err(|error| anyhow!("Whisper transcription failed on segment {i}: {error}"))?;
            let rows: Vec<_> = super::enhancement::untimed_row(
                text,
                segment.start_timestamp_ms,
                segment.end_timestamp_ms,
            )
            .into_iter()
            .collect();
            (rows, confidence)
        };
        total_confidence += confidence * rows.len() as f32;
        transcript_sources.extend(std::iter::repeat(segment.source.clone()).take(rows.len()));
        all_transcripts.extend(rows);
    }
    let mut ordered: Vec<_> = all_transcripts
        .into_iter()
        .zip(transcript_sources)
        .collect();
    ordered.sort_by(|a, b| a.0 .1.total_cmp(&b.0 .1).then(a.1.cmp(&b.1)));
    let (all_transcripts, transcript_sources): (Vec<_>, Vec<_>) = ordered.into_iter().unzip();

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

    let effective_model = if let Some(engine) = whisper_engine.as_ref() {
        engine.get_current_model().await
    } else if let Some(engine) = parakeet_engine.as_ref() {
        engine.get_current_model().await
    } else {
        model
    };
    save_retranscription(
        &app,
        meeting_id,
        meeting_folder_path,
        original,
        duration_seconds,
        language,
        all_transcripts,
        speaker_analysis,
        transcript_sources,
        provider,
        effective_model,
    )
    .await
}

/// Transcribes the whole recording with Nemotron, using the speakers the model
/// finds. Rows come from its word times and end where the speaker changes.
async fn nemotron_with_speakers<R: Runtime>(
    app: &AppHandle<R>,
    meeting_id: &str,
    folder_path: &Path,
    model: Option<&str>,
    audio_samples: &[f32],
    original: &[crate::database::models::Transcript],
) -> Result<(
    Vec<(String, f64, f64)>,
    crate::diarization::finalize::EnhancementSpeakerAnalysis,
)> {
    emit_progress(app, meeting_id, "transcribing", 25, "Loading Nemotron...");
    let provider = crate::nemotron::configured_provider(app, model)
        .await
        .map_err(anyhow::Error::msg)?;
    let minutes = audio_samples.len() as f64 / 16000.0 / 60.0;
    emit_progress(
        app,
        meeting_id,
        "transcribing",
        30,
        &format!("Transcribing {minutes:.0} min and identifying speakers with Nemotron..."),
    );
    let meeting = provider
        .transcribe_meeting(audio_samples, &RETRANSCRIPTION_CANCELLED)
        .await;
    if RETRANSCRIPTION_CANCELLED.load(Ordering::SeqCst) {
        return Err(anyhow!("Retranscription cancelled"));
    }
    let meeting = meeting.map_err(|e| anyhow!("Nemotron transcription failed: {e}"))?;
    drop(provider);
    emit_progress(app, meeting_id, "speakers", 75, "Matching speakers...");
    let analysis = crate::diarization::finalize::prepare_enhancement_from_turns(
        app,
        meeting_id,
        folder_path,
        &meeting.turns,
        &RETRANSCRIPTION_CANCELLED,
    )
    .await?;
    Ok((
        super::enhancement::word_rows(
            super::enhancement::retime_collapsed_words(meeting.words, original),
            Some(&analysis),
            None,
        ),
        analysis,
    ))
}

#[allow(clippy::too_many_arguments)]
async fn save_retranscription<R: Runtime>(
    app: &AppHandle<R>,
    meeting_id: String,
    meeting_folder_path: String,
    original: Vec<crate::database::models::Transcript>,
    duration_seconds: f64,
    language: Option<String>,
    all_transcripts: Vec<(String, f64, f64)>,
    speaker_analysis: Option<crate::diarization::finalize::EnhancementSpeakerAnalysis>,
    transcript_sources: Vec<Option<String>>,
    provider: Option<String>,
    model: Option<String>,
) -> Result<RetranscriptionResult> {
    let app = app.clone();
    // Check for cancellation
    if RETRANSCRIPTION_CANCELLED.load(Ordering::SeqCst) {
        return Err(anyhow!("Retranscription cancelled"));
    }

    emit_progress(&app, &meeting_id, "saving", 80, "Saving transcripts...");

    // Create transcript segments with proper timestamps from VAD
    let segments = create_transcript_segments(&all_transcripts);

    let rows = super::enhancement::transcript_rows(&meeting_id, &segments);
    let labels = speaker_analysis
        .map(|analysis| analysis.label_with_sources(&rows, &transcript_sources))
        .transpose()?;
    let state = app.state::<AppState>();
    let review_id = super::enhancement_review::stage(
        state.db_manager.pool(),
        state.speaker_store.pool(),
        super::enhancement_review::Proposal {
            meeting_id: meeting_id.clone(),
            folder: meeting_folder_path,
            original,
            segments,
            labels,
            duration_seconds,
            language: language.clone(),
            provider,
            model,
            core_backup: Vec::new(),
            speaker_backup: String::new(),
            publication_rows: String::new(),
        },
    )
    .await?;
    let warning = apply_review_content(&app, &meeting_id, &review_id, &RETRANSCRIPTION_CANCELLED)
        .await
        .map_err(|e| anyhow!(e))?;
    emit_progress(
        &app,
        &meeting_id,
        "complete",
        100,
        "Enhancement applied. Undo is available.",
    );
    Ok(RetranscriptionResult {
        meeting_id,
        segments_count: rows.len(),
        duration_seconds,
        language,
        review_id,
        warning,
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

/// Response when retranscription is started
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetranscriptionStarted {
    pub meeting_id: String,
    pub message: String,
}

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
) -> Result<RetranscriptionStarted, String> {
    // Check if retranscription is already in progress (guard will be acquired in start_retranscription)
    if RETRANSCRIPTION_IN_PROGRESS.load(Ordering::SeqCst) {
        return Err("Retranscription already in progress".to_string());
    }

    // Clone values for the spawned task
    let meeting_id_clone = meeting_id.clone();

    // Spawn the retranscription in a background task
    tauri::async_runtime::spawn(async move {
        let result = start_retranscription(
            app,
            meeting_id_clone,
            meeting_folder_path,
            language,
            model,
            provider,
            assign_speakers.unwrap_or(false),
        )
        .await;

        // Errors are already emitted as events in start_retranscription
        // so we just log here for debugging
        if let Err(e) = result {
            error!("Retranscription failed: {}", e);
        }
    });

    Ok(RetranscriptionStarted {
        meeting_id,
        message: "Retranscription started".to_string(),
    })
}

#[tauri::command]
pub async fn get_enhancement_review<R: Runtime>(
    app: AppHandle<R>,
    meeting_id: String,
) -> Result<Option<super::enhancement_review::ReviewPreview>, String> {
    super::enhancement_review::latest(app.state::<AppState>().speaker_store.pool(), &meeting_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn discard_enhancement_review<R: Runtime>(
    app: AppHandle<R>,
    meeting_id: String,
    review_id: String,
) -> Result<(), String> {
    let _edit = crate::diarization::operations::try_meeting_edit(&meeting_id)?;
    super::enhancement_review::discard(
        app.state::<AppState>().speaker_store.pool(),
        &meeting_id,
        &review_id,
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn apply_enhancement_review<R: Runtime>(
    app: AppHandle<R>,
    meeting_id: String,
    review_id: String,
) -> Result<Option<String>, String> {
    let _edit = crate::diarization::operations::try_meeting_edit(&meeting_id)?;
    apply_review_content(&app, &meeting_id, &review_id, &AtomicBool::new(false)).await
}

async fn apply_review_content<R: Runtime>(
    app: &AppHandle<R>,
    meeting_id: &str,
    review_id: &str,
    cancelled: &AtomicBool,
) -> Result<Option<String>, String> {
    let state = app.state::<AppState>();
    let proposal =
        super::enhancement_review::load(state.speaker_store.pool(), &meeting_id, &review_id)
            .await
            .map_err(|e| e.to_string())?;
    let speaker_snapshot =
        super::enhancement_review::speaker_snapshot(state.speaker_store.pool(), &meeting_id)
            .await
            .map_err(|e| e.to_string())?;
    if speaker_snapshot != proposal.speaker_backup {
        return Err("Speaker labels changed while Enhance was running. The saved transcript was kept. Run Enhance again.".into());
    }
    super::enhancement::publish(
        state.db_manager.pool(),
        state.speaker_store.pool(),
        &meeting_id,
        &proposal.original,
        &proposal.segments,
        proposal.labels.as_ref(),
        cancelled,
    )
    .await
    .map_err(|e| e.to_string())?;
    emit_speaker_refresh(app, meeting_id);
    let folder = PathBuf::from(&proposal.folder);
    let transcript_export = write_transcripts_json(&folder, &proposal.segments);
    let audio = find_audio_file(&folder)
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "audio.mp4".into());
    let metadata_export =
        write_retranscription_metadata(&folder, &meeting_id, proposal.duration_seconds, &audio);
    if transcript_export.is_err() || metadata_export.is_err() {
        return Ok(Some("The transcript is saved in the app, but its recording folder export could not be updated.".into()));
    }
    Ok(None)
}

#[tauri::command]
pub async fn undo_enhancement<R: Runtime>(
    app: AppHandle<R>,
    meeting_id: String,
    review_id: String,
) -> Result<Option<String>, String> {
    let _edit = crate::diarization::operations::try_meeting_edit(&meeting_id)?;
    let state = app.state::<AppState>();
    let proposal = super::enhancement_review::undo(
        state.db_manager.pool(),
        state.speaker_store.pool(),
        &meeting_id,
        &review_id,
    )
    .await
    .map_err(|e| e.to_string())?;
    emit_speaker_refresh(&app, &meeting_id);
    let segments =
        super::enhancement_review::original_segments(&proposal).map_err(|e| e.to_string())?;
    if write_transcripts_json(&PathBuf::from(&proposal.folder), &segments).is_err() {
        return Ok(Some("Undo restored the transcript in the app, but its recording folder export could not be updated.".into()));
    }
    Ok(None)
}

fn emit_speaker_refresh<R: Runtime>(app: &AppHandle<R>, meeting_id: &str) {
    let _ = app.emit(
        "diarization-progress",
        serde_json::json!({
            "meetingId": meeting_id, "status": "completed", "progress": 100, "error": null,
        }),
    );
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
    /// Local full-recording replay. Supply private audio and model paths in the
    /// environment; the test does not download models or package meeting audio.
    /// MEETILY_BENCHMARK_PROVIDER selects whisper, parakeet, or funasr.
    /// MEETILY_BENCHMARK_BASELINE can supply saved live JSON for quiet-speech regions.
    #[tokio::test]
    #[ignore = "requires MEETILY_BENCHMARK_AUDIO, MEETILY_BENCHMARK_MODELS, and MEETILY_BENCHMARK_OUTPUT"]
    async fn replay_whole_recording_for_quality_comparison() {
        use std::io::Write;
        let audio = PathBuf::from(
            std::env::var("MEETILY_BENCHMARK_AUDIO").expect("set the source audio path"),
        );
        let models = PathBuf::from(
            std::env::var("MEETILY_BENCHMARK_MODELS").expect("set the read-only model directory"),
        );
        let output = PathBuf::from(
            std::env::var("MEETILY_BENCHMARK_OUTPUT").expect("set a new output path"),
        );
        let provider =
            std::env::var("MEETILY_BENCHMARK_PROVIDER").unwrap_or_else(|_| "whisper".into());
        assert!(
            ["whisper", "parakeet", "funasr"].contains(&provider.as_str()),
            "unknown benchmark provider"
        );
        let batch_engine = super::super::common::BatchEngine::from_provider(Some(&provider));
        let model =
            std::env::var("MEETILY_BENCHMARK_MODEL").unwrap_or_else(|_| match batch_engine {
                super::super::common::BatchEngine::Whisper => "large-v3".into(),
                super::super::common::BatchEngine::Parakeet => {
                    crate::config::DEFAULT_PARAKEET_MODEL.into()
                }
                super::super::common::BatchEngine::FunAsr => {
                    crate::config::DEFAULT_FUNASR_MODEL.into()
                }
                super::super::common::BatchEngine::Nemotron => {
                    unreachable!("Nemotron uses its dedicated native-server regression test")
                }
            });
        let language = std::env::var("MEETILY_BENCHMARK_LANGUAGE")
            .ok()
            .filter(|s| s != "auto");
        let owned = tempfile::tempdir().unwrap();
        let whisper = if batch_engine == super::super::common::BatchEngine::Whisper {
            let engine = WhisperEngine::new_with_models_dir(Some(owned.path().to_path_buf()))
                .unwrap()
                .with_shared_models_dir(Some(models.clone()));
            engine.discover_models().await.unwrap();
            engine.load_model(&model).await.unwrap();
            Some(engine)
        } else {
            None
        };
        let parakeet = if batch_engine == super::super::common::BatchEngine::Parakeet {
            let engine = ParakeetEngine::new_with_models_dir(Some(owned.path().to_path_buf()))
                .unwrap()
                .with_shared_models_dir(Some(models.join("parakeet")));
            engine.discover_models().await.unwrap();
            engine.load_model(&model).await.unwrap();
            Some(engine)
        } else {
            None
        };
        let funasr = if batch_engine == super::super::common::BatchEngine::FunAsr {
            let engine = crate::funasr_engine::engine::FunAsrEngine::new(
                owned.path().to_path_buf(),
                Some(models),
            )
            .unwrap();
            engine.load_selected_model(&model).await.unwrap();
            Some(engine)
        } else {
            None
        };
        let decoded = super::super::enhancement::decode_enhancement_audio(&audio).unwrap();
        let baseline:Vec<crate::database::models::Transcript>=std::env::var("MEETILY_BENCHMARK_BASELINE").ok().map(|path| {
            let source:serde_json::Value=serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            source["segments"].as_array().unwrap().iter().enumerate().map(|(i,row)| {
                serde_json::from_value(serde_json::json!({"id":format!("baseline-{i}"),"meeting_id":"benchmark",
                    "transcript":row["text"],"timestamp":"","audio_start_time":row["audio_start_time"],
                    "audio_end_time":row["audio_end_time"],"duration":row["duration"]})).unwrap()
            }).collect()
        }).unwrap_or_default();
        let contexts = super::super::enhancement::recognition_inputs(
            &decoded,
            batch_engine,
            ENHANCEMENT_VAD_REDEMPTION_TIME_MS,
            &baseline,
            |_, _| true,
        )
        .unwrap();
        let chunks: Vec<_> = contexts
            .iter()
            .flat_map(|s| {
                super::super::enhancement::input_chunks(
                    s,
                    batch_engine.enhancement_input_policy().max_input_samples,
                )
                .into_iter()
                .map(|segment| super::super::enhancement::RecognitionInput {
                    segment,
                    source: s.source.clone(),
                })
            })
            .collect();
        use sha2::Digest;
        let recognition_inputs:Vec<_>=chunks.iter().map(|chunk|serde_json::json!({
            "source":chunk.source,"start_ms":chunk.start_timestamp_ms,"end_ms":chunk.end_timestamp_ms,
            "samples":chunk.samples.len(),"audio_sha256":format!("{:x}",sha2::Sha256::digest(bytemuck::cast_slice::<f32,u8>(&chunk.samples)))
        })).collect();
        let mut rows = Vec::new();
        let mut sources = Vec::new();
        let live_settings = std::env::var("MEETILY_BENCHMARK_LIVE_SETTINGS").as_deref() == Ok("1");
        for chunk in chunks {
            let decoded_rows = if let Some(engine) = parakeet.as_ref() {
                let result = engine
                    .transcribe_audio_timed(chunk.samples.clone())
                    .await
                    .unwrap();
                if result.text.trim().is_empty() {
                    Vec::new()
                } else {
                    super::super::enhancement::parakeet_rows(
                        &result,
                        chunk.start_timestamp_ms,
                        chunk.end_timestamp_ms - chunk.start_timestamp_ms,
                        None,
                        chunk.source.as_deref(),
                    )
                    .unwrap()
                }
            } else {
                let text = if let Some(engine) = funasr.as_ref() {
                    engine
                        .transcribe(
                            chunk.samples.clone(),
                            crate::funasr_engine::normalize_language(language.as_deref()).unwrap(),
                        )
                        .await
                        .unwrap()
                } else if live_settings {
                    whisper
                        .as_ref()
                        .unwrap()
                        .transcribe_audio_with_confidence(chunk.samples.clone(), language.clone())
                        .await
                        .unwrap()
                        .0
                } else {
                    whisper
                        .as_ref()
                        .unwrap()
                        .transcribe_batch_audio_with_confidence(
                            chunk.samples.clone(),
                            language.clone(),
                        )
                        .await
                        .unwrap()
                        .0
                };
                super::super::enhancement::untimed_row(
                    text,
                    chunk.start_timestamp_ms,
                    chunk.end_timestamp_ms,
                )
                .into_iter()
                .collect()
            };
            sources.extend(std::iter::repeat(chunk.source.clone()).take(decoded_rows.len()));
            rows.extend(decoded_rows);
        }
        if let Some(engine) = funasr {
            engine.unload_model().await;
        }
        let mut ordered: Vec<_> = rows.into_iter().zip(sources).collect();
        ordered.sort_by(|a, b| a.0 .1.total_cmp(&b.0 .1).then(a.1.cmp(&b.1)));
        let (rows, sources): (Vec<_>, Vec<_>) = ordered.into_iter().unzip();
        let segments = create_transcript_segments(&rows);
        assert!(!segments.is_empty(), "the replay must produce text");
        let report = serde_json::json!({"provider":provider,"model":model,"language":language,
            "duration_seconds":decoded.duration_seconds,"source_sample_rate":decoded.sample_rate,"preprocessing":"source-aware","input_policy":batch_engine.enhancement_input_policy(),"live_settings":live_settings,"segments":segments,"sources":sources,"baseline":baseline,"recognition_inputs":recognition_inputs});
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output)
            .unwrap();
        file.write_all(&serde_json::to_vec_pretty(&report).unwrap())
            .unwrap();
        if let Ok(expected) = std::env::var("MEETILY_BENCHMARK_EXPECT_TEXT") {
            let normalize = |text: &str| {
                text.to_lowercase()
                    .chars()
                    .map(|c| if c.is_alphanumeric() { c } else { ' ' })
                    .collect::<String>()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            let actual = report["segments"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["text"].as_str().unwrap())
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                normalize(&actual).contains(&normalize(&expected)),
                "the checked phrase was not retained; inspect {}",
                output.display()
            );
        }
    }

    /// Controlled word-error diagnosis through the loaded application engine.
    #[tokio::test]
    #[ignore = "requires MEETILY_DIAGNOSTIC_CASES, MEETILY_BENCHMARK_MODELS, and MEETILY_BENCHMARK_OUTPUT"]
    async fn diagnose_recognition_on_fixed_audio() {
        use sha2::Digest;
        use std::io::Write;
        #[derive(Deserialize)]
        struct Case {
            name: String,
            path: PathBuf,
            start: f64,
            end: f64,
            #[serde(default)]
            language: Option<String>,
            #[serde(default)]
            live_settings: bool,
            #[serde(default)]
            options: crate::whisper_engine::whisper_engine::DiagnosticDecodeOptions,
        }
        let cases: Vec<Case> = serde_json::from_slice(
            &std::fs::read(std::env::var("MEETILY_DIAGNOSTIC_CASES").unwrap()).unwrap(),
        )
        .unwrap();
        let owned = tempfile::tempdir().unwrap();
        let engine = WhisperEngine::new_with_models_dir(Some(owned.path().to_path_buf()))
            .unwrap()
            .with_shared_models_dir(Some(PathBuf::from(
                std::env::var("MEETILY_BENCHMARK_MODELS").unwrap(),
            )));
        engine.discover_models().await.unwrap();
        engine.load_model("large-v3").await.unwrap();
        let mut output = Vec::new();
        for case in cases {
            let bytes = std::fs::read(&case.path).unwrap();
            let all: Vec<f32> = bytes
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                .collect();
            let audio = all
                [(case.start * 16000.0).round() as usize..(case.end * 16000.0).round() as usize]
                .to_vec();
            let digest = format!(
                "{:x}",
                sha2::Sha256::digest(bytemuck::cast_slice::<f32, u8>(&audio))
            );
            let (text, _, _) = engine
                .transcribe_diagnostic(audio, case.language, !case.live_settings, case.options)
                .await
                .unwrap();
            println!("{}: {}", case.name, text);
            output.push(serde_json::json!({"case":case.name,"start":case.start,"end":case.end,"audio_sha256":digest,"text":text}));
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(std::env::var("MEETILY_BENCHMARK_OUTPUT").unwrap())
            .unwrap();
        file.write_all(&serde_json::to_vec_pretty(&output).unwrap())
            .unwrap();
    }
}
