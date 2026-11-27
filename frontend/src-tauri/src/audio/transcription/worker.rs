// audio/transcription/worker.rs
//
// Parallel transcription worker pool and chunk processing logic.

use super::engine::TranscriptionEngine;
use super::provider::TranscriptionError;
use crate::audio::recording_state::SourceActivity;
use crate::audio::AudioChunk;
use crate::database::repositories::speaker_profile::SpeakerProfilesRepository;
use crate::diarization::session::DiarizationSession;
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, Runtime};

// Sequence counter for transcript updates
static SEQUENCE_COUNTER: AtomicU64 = AtomicU64::new(0);

// Speech detection flag - reset per recording session
static SPEECH_DETECTED_EMITTED: AtomicBool = AtomicBool::new(false);

/// Reset the speech detected flag for a new recording session
pub fn reset_speech_detected_flag() {
    SPEECH_DETECTED_EMITTED.store(false, Ordering::SeqCst);
}

/// Returns true if the transcript text is non-trivial and should be emitted.
/// Filters empty/whitespace-only text; no confidence gating is applied.
fn should_emit_transcript(text: &str) -> bool {
    !text.trim().is_empty()
}

fn should_log_transcript_snippet(accepted_count: u64) -> bool {
    accepted_count <= 5 || accepted_count % 25 == 0
}

#[derive(Default)]
struct WorkerLogStats {
    queued: u64,
    completed: u64,
    emitted: u64,
    empty: u64,
    too_short: u64,
    failed: u64,
    model_not_loaded_failures: u64,
    engine_failures: u64,
    unsupported_language_failures: u64,
    speech_detected_emit_failures: u64,
    transcript_update_emit_failures: u64,
    max_queue_depth: u64,
    total_processing_duration: Duration,
    confidence_total: f64,
    confidence_count: u64,
}

enum WorkerFailureCategory {
    ModelNotLoaded,
    EngineFailed,
    UnsupportedLanguage,
    SpeechDetectedEmit,
    TranscriptUpdateEmit,
}

impl WorkerLogStats {
    fn record_failure(&mut self, category: WorkerFailureCategory) {
        self.failed += 1;
        match category {
            WorkerFailureCategory::ModelNotLoaded => self.model_not_loaded_failures += 1,
            WorkerFailureCategory::EngineFailed => self.engine_failures += 1,
            WorkerFailureCategory::UnsupportedLanguage => self.unsupported_language_failures += 1,
            WorkerFailureCategory::SpeechDetectedEmit => self.speech_detected_emit_failures += 1,
            WorkerFailureCategory::TranscriptUpdateEmit => {
                self.transcript_update_emit_failures += 1
            }
        }
    }

    fn snapshot_and_reset(&mut self) -> Self {
        std::mem::take(self)
    }
}

fn emit_worker_log_snapshot(stats: WorkerLogStats, final_summary: bool) {
    let confidence_average = if stats.confidence_count == 0 {
        0.0
    } else {
        stats.confidence_total / stats.confidence_count as f64
    };

    if final_summary {
        info!(
            "transcription worker final summary queued={} completed={} emitted={} empty={} too_short={} failed={} model_not_loaded_failures={} engine_failures={} unsupported_language_failures={} speech_detected_emit_failures={} transcript_update_emit_failures={} max_queue_depth={} processing_ms={} confidence_average={:.2} confidence_count={}",
            stats.queued,
            stats.completed,
            stats.emitted,
            stats.empty,
            stats.too_short,
            stats.failed,
            stats.model_not_loaded_failures,
            stats.engine_failures,
            stats.unsupported_language_failures,
            stats.speech_detected_emit_failures,
            stats.transcript_update_emit_failures,
            stats.max_queue_depth,
            stats.total_processing_duration.as_millis(),
            confidence_average,
            stats.confidence_count
        );
        if stats.failed > 0 {
            warn!(
                "transcription worker final failures={} model_not_loaded_failures={} engine_failures={} unsupported_language_failures={} speech_detected_emit_failures={} transcript_update_emit_failures={} queued={} completed={}",
                stats.failed,
                stats.model_not_loaded_failures,
                stats.engine_failures,
                stats.unsupported_language_failures,
                stats.speech_detected_emit_failures,
                stats.transcript_update_emit_failures,
                stats.queued,
                stats.completed
            );
        }
    } else if stats.failed > 0 {
        warn!(
            "transcription worker summary queued={} completed={} emitted={} empty={} too_short={} failed={} model_not_loaded_failures={} engine_failures={} unsupported_language_failures={} speech_detected_emit_failures={} transcript_update_emit_failures={} max_queue_depth={} processing_ms={} confidence_average={:.2} confidence_count={}",
            stats.queued,
            stats.completed,
            stats.emitted,
            stats.empty,
            stats.too_short,
            stats.failed,
            stats.model_not_loaded_failures,
            stats.engine_failures,
            stats.unsupported_language_failures,
            stats.speech_detected_emit_failures,
            stats.transcript_update_emit_failures,
            stats.max_queue_depth,
            stats.total_processing_duration.as_millis(),
            confidence_average,
            stats.confidence_count
        );
    } else {
        debug!(
            "transcription worker summary queued={} completed={} emitted={} empty={} too_short={} failed={} model_not_loaded_failures={} engine_failures={} unsupported_language_failures={} speech_detected_emit_failures={} transcript_update_emit_failures={} max_queue_depth={} processing_ms={} confidence_average={:.2} confidence_count={}",
            stats.queued,
            stats.completed,
            stats.emitted,
            stats.empty,
            stats.too_short,
            stats.failed,
            stats.model_not_loaded_failures,
            stats.engine_failures,
            stats.unsupported_language_failures,
            stats.speech_detected_emit_failures,
            stats.transcript_update_emit_failures,
            stats.max_queue_depth,
            stats.total_processing_duration.as_millis(),
            confidence_average,
            stats.confidence_count
        );
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TranscriptUpdate {
    pub text: String,
    pub timestamp: String, // Wall-clock time for reference (e.g., "14:30:05")
    pub source: String,
    pub sequence_id: u64,
    pub chunk_start_time: f64, // Legacy field, kept for compatibility
    pub is_partial: bool,
    pub confidence: f32,
    // NEW: Recording-relative timestamps for playback sync
    pub audio_start_time: f64, // Seconds from recording start (e.g., 125.3)
    pub audio_end_time: f64,   // Seconds from recording start (e.g., 128.6)
    pub duration: f64,         // Segment duration in seconds (e.g., 3.3)
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

#[derive(Default)]
struct LiveSpeakerAssignment {
    speaker_id: Option<String>,
    speaker_label: Option<String>,
    speaker_profile_id: Option<String>,
    speaker_source: Option<String>,
    speaker_color: Option<String>,
    is_overlap: Option<bool>,
    candidate_speaker_ids: Option<Vec<String>>,
    diarization_status: Option<String>,
    diarization_method: Option<String>,
    confidence: Option<f32>,
}

async fn live_labels_enabled_for_app<R: Runtime>(app: &AppHandle<R>) -> bool {
    let state = app.state::<crate::state::AppState>();
    crate::database::repositories::diarization::DiarizationRepository::get_settings(
        state.speaker_store.pool(),
    )
    .await
    .map(|settings| {
        crate::diarization::types::should_show_live_labels(
            settings.enabled != 0,
            &settings.mode,
            settings.show_provisional_labels != 0,
        )
    })
    .unwrap_or(false)
}

async fn initialize_live_diarization<R: Runtime>(app: &AppHandle<R>) -> Option<DiarizationSession> {
    let state = app.state::<crate::state::AppState>();
    let settings = crate::database::repositories::diarization::DiarizationRepository::get_settings(
        state.speaker_store.pool(),
    )
    .await
    .ok()?;
    if !crate::diarization::types::should_show_live_labels(
        settings.enabled != 0,
        &settings.mode,
        settings.show_provisional_labels != 0,
    ) {
        return None;
    }

    let model_path = crate::diarization::models::embedding_model_path(app).ok()?;
    if crate::diarization::models::validate_model_file(&model_path).is_err() {
        warn!("Live speaker labels disabled because the WeSpeaker model is unavailable");
        return None;
    }
    let profiles = SpeakerProfilesRepository::list(state.speaker_store.pool())
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|profile| {
            profile.model_version == crate::diarization::models::EMBEDDING_MODEL_VERSION
        })
        .map(|profile| (profile.id, profile.name, profile.embedding))
        .collect();

    tokio::task::spawn_blocking(move || DiarizationSession::with_profiles(&model_path, profiles))
        .await
        .ok()?
        .map_err(|error| warn!("Unable to start live speaker recognition: {error}"))
        .ok()
}

fn assign_live_speaker(
    activity: Option<&SourceActivity>,
    samples: &[f32],
    session: &mut Option<DiarizationSession>,
) -> LiveSpeakerAssignment {
    let Some(activity) = activity else {
        return LiveSpeakerAssignment::default();
    };
    let base = |id: &str, label: &str, source: &str, overlap: bool| LiveSpeakerAssignment {
        speaker_id: Some(id.to_string()),
        speaker_label: Some(label.to_string()),
        speaker_source: Some(source.to_string()),
        speaker_color: Some(
            if overlap { crate::diarization::colors::LIVE_OVERLAP_COLOR } else { crate::diarization::colors::LIVE_SPEAKER_COLOR }
                .to_string(),
        ),
        is_overlap: Some(overlap),
        diarization_status: Some("provisional".to_string()),
        diarization_method: Some("live_source_wespeaker".to_string()),
        ..Default::default()
    };

    match (activity.microphone_active, activity.system_active) {
        (true, true) if activity.overlap_detected => {
            let mut assignment = base("multiple-speakers", "Multiple speakers", "mixed", true);
            assignment.speaker_id = None;
            assignment
        }
        (true, true) => {
            let mut assignment = base("unknown-speaker", "Unknown speaker", "mixed", false);
            assignment.diarization_status = Some("needs_review".to_string());
            assignment
        }
        (true, false) | (false, true) => {
            let source = if activity.system_active {
                "system"
            } else {
                "microphone"
            };
            if let Some(result) = session
                .as_mut()
                .and_then(|session| session.label_segment_detailed(source, samples))
            {
                let mut assignment = base(
                    result.profile_id.as_deref().unwrap_or_else(|| {
                        if source == "system" {
                            "remote-speaker"
                        } else {
                            "room-speaker"
                        }
                    }),
                    &result.label,
                    source,
                    false,
                );
                if result.profile_id.is_none() {
                    assignment.speaker_id = Some(format!(
                        "{}-{}",
                        if source == "system" { "remote" } else { "room" },
                        result
                            .label
                            .to_lowercase()
                            .replace(|character: char| !character.is_ascii_alphanumeric(), "-")
                    ));
                }
                assignment.speaker_profile_id = result.profile_id;
                assignment.confidence = Some(result.confidence);
                if result.needs_review {
                    assignment.diarization_status = Some("needs_review".to_string());
                    assignment.speaker_color = Some(crate::diarization::colors::LIVE_REVIEW_COLOR.to_string());
                }
                assignment
            } else {
                let mut assignment = base("unknown-speaker", "Unknown speaker", source, false);
                assignment.diarization_status = Some("needs_review".to_string());
                assignment.speaker_color = Some(crate::diarization::colors::LIVE_REVIEW_COLOR.to_string());
                assignment
            }
        }
        (false, false) => base("unknown-speaker", "Unknown speaker", "mixed", false),
    }
}

/// Resolve stable recording-relative timing for a transcription chunk.
///
/// VAD boundaries are authoritative because they describe where speech was
/// detected on the recording timeline. Sample-derived duration is retained as
/// a fallback for providers and older producers that do not supply an end.
fn resolve_transcript_timing(chunk: &AudioChunk) -> (f64, f64, f64) {
    let start = if chunk.timestamp.is_finite() {
        chunk.timestamp.max(0.0)
    } else {
        0.0
    };
    let sample_duration = if chunk.sample_rate > 0 {
        chunk.data.len() as f64 / chunk.sample_rate as f64
    } else {
        0.0
    };
    let fallback_end = start + sample_duration;
    let end = chunk
        .end_timestamp
        .filter(|end| end.is_finite() && *end >= start)
        .unwrap_or(fallback_end);

    (start, end, end - start)
}

// NOTE: get_transcript_history and get_recording_meeting_name functions
// have been moved to recording_commands.rs where they have access to RECORDING_MANAGER

/// Join every worker and pass failures to the recording stop command.
async fn wait_for_transcription_workers(
    workers: Vec<tokio::task::JoinHandle<()>>,
) -> Result<(), String> {
    let mut failure = None;
    for (worker_id, handle) in workers.into_iter().enumerate() {
        if let Err(error) = handle.await {
            let message = format!("Transcription worker {worker_id} failed: {error}");
            error!("{message}");
            failure.get_or_insert(message);
        } else {
            info!("Worker {worker_id} completed successfully");
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Run the transcription workers and report failures or missing chunks.
pub fn start_transcription_task<R: Runtime>(
    app: AppHandle<R>,
    transcription_receiver: tokio::sync::mpsc::UnboundedReceiver<AudioChunk>,
) -> (tokio::task::JoinHandle<Result<(), String>>, tokio::sync::oneshot::Receiver<Result<(), String>>) {
    let (init_sender, init_receiver) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        info!("Starting transcription worker task");

        let transcription_engine = match super::engine::get_or_init_transcription_engine(&app).await {
            Ok(engine) => engine,
            Err(e) => {
                error!("Failed to initialize transcription engine");
                // The recording owner must stop capture. It runs outside this
                // task, because shutdown waits for this task to finish.
                let _ = init_sender.send(Err(e.clone()));
                return Err(e);
            }
        };
        // A closed channel before this signal means that initialization did
        // not finish, for example because this task panicked.
        let _ = init_sender.send(Ok(()));

        const NUM_WORKERS: usize = 1;
        let (work_sender, work_receiver) = tokio::sync::mpsc::unbounded_channel::<AudioChunk>();
        let work_receiver = Arc::new(tokio::sync::Mutex::new(work_receiver));
        let chunks_queued = Arc::new(AtomicU64::new(0));
        let chunks_completed = Arc::new(AtomicU64::new(0));
        let mut worker_handles = Vec::new();

        for worker_id in 0..NUM_WORKERS {
            let engine_clone = match &transcription_engine {
                TranscriptionEngine::Whisper(e) => TranscriptionEngine::Whisper(e.clone()),
                TranscriptionEngine::Parakeet(e) => TranscriptionEngine::Parakeet(e.clone()),
                TranscriptionEngine::Provider(p) => TranscriptionEngine::Provider(p.clone()),
            };
            let app_clone = app.clone();
            let work_receiver_clone = work_receiver.clone();
            let chunks_completed_clone = chunks_completed.clone();
            let chunks_queued_clone = chunks_queued.clone();

            worker_handles.push(tokio::spawn(async move {
                let mut stats = WorkerLogStats::default();
                let mut last_summary = Instant::now();
                let mut diarization_session = initialize_live_diarization(&app_clone).await;
                let mut live_labels_enabled = live_labels_enabled_for_app(&app_clone).await;

                while let Some(chunk) = {
                    let mut receiver = work_receiver_clone.lock().await;
                    receiver.recv().await
                } {
                    stats.queued += 1;
                    let queued = chunks_queued_clone.load(Ordering::SeqCst);
                    let completed_before = chunks_completed_clone.load(Ordering::SeqCst);
                    stats.max_queue_depth = stats
                        .max_queue_depth
                        .max(queued.saturating_sub(completed_before));
                    let processing_started = Instant::now();

                    if !engine_clone.is_model_loaded().await {
                        stats.record_failure(WorkerFailureCategory::ModelNotLoaded);
                    } else {
                        let (audio_start_time, audio_end_time, chunk_duration) =
                            resolve_transcript_timing(&chunk);
                        let current_live_labels_enabled =
                            live_labels_enabled_for_app(&app_clone).await;
                        if current_live_labels_enabled != live_labels_enabled {
                            diarization_session = if current_live_labels_enabled {
                                initialize_live_diarization(&app_clone).await
                            } else {
                                None
                            };
                            live_labels_enabled = current_live_labels_enabled;
                        }
                        let live_speaker = if live_labels_enabled {
                            assign_live_speaker(
                                chunk.source_activity.as_ref(),
                                &chunk.data,
                                &mut diarization_session,
                            )
                        } else {
                            LiveSpeakerAssignment::default()
                        };

                        match transcribe_chunk_with_provider(&engine_clone, chunk, &app_clone).await {
                            Ok((transcript, confidence_opt, is_partial)) => {
                                if let Some(confidence) = confidence_opt {
                                    stats.confidence_total += confidence as f64;
                                    stats.confidence_count += 1;
                                }

                                if should_emit_transcript(&transcript) {
                                    if SPEECH_DETECTED_EMITTED
                                        .compare_exchange(
                                            false,
                                            true,
                                            Ordering::SeqCst,
                                            Ordering::SeqCst,
                                        )
                                        .is_ok()
                                        && app_clone
                                            .emit(
                                                "speech-detected",
                                                serde_json::json!({
                                                    "message": "Speech activity detected"
                                                }),
                                            )
                                            .is_err()
                                    {
                                        stats.record_failure(WorkerFailureCategory::SpeechDetectedEmit);
                                    }

                                    // recording_commands listens to transcript-update and saves it.
                                    let sequence_id =
                                        SEQUENCE_COUNTER.fetch_add(1, Ordering::SeqCst);
                                    let update = TranscriptUpdate {
                                        text: transcript,
                                        timestamp: format_current_timestamp(), // Wall-clock for reference
                                        source: "Audio".to_string(),
                                        sequence_id,
                                        chunk_start_time: audio_start_time, // Legacy compatibility
                                        is_partial,
                                        confidence: confidence_opt.unwrap_or(0.85), // Default for providers without confidence
                                        // Recording-relative timestamps for sync
                                        audio_start_time,
                                        audio_end_time,
                                        duration: chunk_duration,
                                        speaker_id: live_speaker.speaker_id,
                                        speaker_label: live_speaker.speaker_label,
                                        speaker_profile_id: live_speaker.speaker_profile_id,
                                        speaker_source: live_speaker.speaker_source,
                                        speaker_color: live_speaker.speaker_color,
                                        is_overlap: live_speaker.is_overlap,
                                        candidate_speaker_ids: live_speaker.candidate_speaker_ids,
                                        diarization_status: live_speaker.diarization_status,
                                        diarization_method: live_speaker.diarization_method,
                                        diarization_confidence: live_speaker.confidence,
                                    };

                                    if app_clone.emit("transcript-update", &update).is_ok() {
                                        stats.emitted += 1;
                                        #[cfg(debug_assertions)]
                                        if should_log_transcript_snippet(stats.emitted) {
                                            debug!(
                                                "accepted transcript result #{}: {}",
                                                stats.emitted,
                                                crate::utils::log_snippet(&update.text, 200)
                                            );
                                        }
                                    } else {
                                        stats.record_failure(WorkerFailureCategory::TranscriptUpdateEmit);
                                    }
                                } else {
                                    stats.empty += 1;
                                }
                            }
                            Err(TranscriptionError::AudioTooShort { .. }) => {
                                stats.too_short += 1;
                            }
                            Err(TranscriptionError::ModelNotLoaded) => {
                                stats.record_failure(WorkerFailureCategory::ModelNotLoaded);
                            }
                            Err(error @ TranscriptionError::EngineFailed(_)) => {
                                stats.record_failure(WorkerFailureCategory::EngineFailed);
                                warn!("Transcription engine failed: {}", error);
                                let _ =
                                    app_clone.emit("transcription-warning", error.to_string());
                            }
                            Err(error @ TranscriptionError::UnsupportedLanguage(_)) => {
                                stats.record_failure(WorkerFailureCategory::UnsupportedLanguage);
                                let _ =
                                    app_clone.emit("transcription-warning", error.to_string());
                            }
                        }
                    }

                    stats.total_processing_duration += processing_started.elapsed();
                    stats.completed += 1;
                    let completed = chunks_completed_clone.fetch_add(1, Ordering::SeqCst) + 1;
                    let progress_percentage = if queued > 0 {
                        (completed as f64 / queued as f64 * 100.0) as u32
                    } else {
                        100
                    };
                    let _ = app_clone.emit("transcription-progress", serde_json::json!({
                        "worker_id": worker_id,
                        "chunks_completed": completed,
                        "chunks_queued": queued,
                        "progress_percentage": progress_percentage,
                        "message": format!("Worker {} processing... ({}/{})", worker_id, completed, queued)
                    }));

                    if last_summary.elapsed() >= Duration::from_secs(60) {
                        emit_worker_log_snapshot(stats.snapshot_and_reset(), false);
                        last_summary = Instant::now();
                    }
                }

                emit_worker_log_snapshot(stats.snapshot_and_reset(), true);
            }));
        }

        let mut receiver = transcription_receiver;
        while let Some(chunk) = receiver.recv().await {
            chunks_queued.fetch_add(1, Ordering::SeqCst);
            if work_sender.send(chunk).is_err() {
                error!("Failed to send transcription chunk to worker");
                break;
            }
        }
        drop(work_sender);

        let total_chunks_queued = chunks_queued.load(Ordering::SeqCst);
        let _ = app.emit("transcription-queue-complete", serde_json::json!({
            "total_chunks": total_chunks_queued,
            "message": format!("{} chunks queued for processing - waiting for completion", total_chunks_queued)
        }));

        // Keep the result while the final count check and its event still run.
        let worker_result = wait_for_transcription_workers(worker_handles).await;

        const MAX_VERIFICATION_ATTEMPTS: u32 = 10;
        for _ in 0..MAX_VERIFICATION_ATTEMPTS {
            if chunks_queued.load(Ordering::SeqCst) == chunks_completed.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        let final_queued = chunks_queued.load(Ordering::SeqCst);
        let final_completed = chunks_completed.load(Ordering::SeqCst);
        if final_queued != final_completed {
            error!(
                "Transcription chunk loss detected queued={} completed={}",
                final_queued, final_completed
            );
            let _ = app.emit(
                "transcript-chunk-loss-detected",
                serde_json::json!({
                    "chunks_queued": final_queued,
                    "chunks_completed": final_completed,
                    "chunks_lost": final_queued - final_completed,
                    "message": "Some transcript chunks may have been lost during shutdown"
                }),
            );
        }

        worker_result?;
        if final_queued != final_completed {
            return Err(format!(
                "Transcription ended with {final_queued} chunks queued and {final_completed} completed"
            ));
        }
        info!(
            "Transcription worker task completed queued={} completed={}",
            final_queued, final_completed
        );
        Ok(())
    });
    (task, init_receiver)
}

/// Transcribe audio chunk using the appropriate provider (Whisper, Parakeet, or trait-based)
/// Returns: (text, confidence Option, is_partial)
pub(crate) async fn transcribe_chunk_with_provider<R: Runtime>(
    engine: &TranscriptionEngine,
    chunk: AudioChunk,
    app: &AppHandle<R>,
) -> std::result::Result<(String, Option<f32>, bool), TranscriptionError> {
    let speech_samples = if chunk.sample_rate != 16000 {
        crate::audio::audio_processing::resample_audio(&chunk.data, chunk.sample_rate, 16000)
    } else {
        chunk.data
    };

    if speech_samples.is_empty() {
        return Err(TranscriptionError::AudioTooShort {
            samples: 0,
            minimum: 1600,
        });
    }

    match engine {
        TranscriptionEngine::Whisper(whisper_engine) => {
            let language = crate::get_language_preference_internal();
            match whisper_engine
                .transcribe_audio_with_confidence(speech_samples, language)
                .await
            {
                Ok((text, confidence, is_partial)) => {
                    Ok((text.trim().to_string(), Some(confidence), is_partial))
                }
                Err(error) => {
                    let transcription_error = TranscriptionError::EngineFailed(error.to_string());
                    let _ = app.emit(
                        "transcription-error",
                        &serde_json::json!({
                            "error": transcription_error.to_string(),
                            "userMessage": format!("Transcription failed: {}", transcription_error),
                            "actionable": false,
                            "phase": "active"
                        }),
                    );
                    Err(transcription_error)
                }
            }
        }
        TranscriptionEngine::Parakeet(parakeet_engine) => {
            match parakeet_engine.transcribe_audio(speech_samples).await {
                Ok(text) => Ok((text.trim().to_string(), None, false)),
                Err(error) => {
                    let transcription_error = TranscriptionError::EngineFailed(error.to_string());
                    let _ = app.emit(
                        "transcription-error",
                        &serde_json::json!({
                            "error": transcription_error.to_string(),
                            "userMessage": format!("Transcription failed: {}", transcription_error),
                            "actionable": false,
                            "phase": "active"
                        }),
                    );
                    Err(transcription_error)
                }
            }
        }
        TranscriptionEngine::Provider(provider) => {
            let language = crate::get_language_preference_internal();
            match provider.transcribe(speech_samples, language).await {
                Ok(result) => Ok((
                    result.text.trim().to_string(),
                    result.confidence,
                    result.is_partial,
                )),
                Err(error) => {
                    let _ = app.emit(
                        "transcription-error",
                        &serde_json::json!({
                            "error": error.to_string(),
                            "userMessage": format!("Transcription failed: {}", error),
                            "actionable": false,
                            "phase": "active"
                        }),
                    );
                    Err(error)
                }
            }
        }
    }
}

/// Format current timestamp (wall-clock time)
fn format_current_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();

    let hours = (now.as_secs() / 3600) % 24;
    let minutes = (now.as_secs() / 60) % 60;
    let seconds = now.as_secs() % 60;

    format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
}

/// Format recording-relative time as [MM:SS]
#[allow(dead_code)]
fn format_recording_time(seconds: f64) -> String {
    let total_seconds = seconds.floor() as u64;
    let minutes = total_seconds / 60;
    let secs = total_seconds % 60;

    format!("[{:02}:{:02}]", minutes, secs)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::audio::recording_state::{AudioChunk, DeviceType, SourceActivity};

        #[tokio::test]
        async fn inner_worker_panic_reaches_the_outer_task_after_all_workers_finish() {
            let (finished_sender, finished_receiver) = tokio::sync::oneshot::channel();
            let failed = tokio::spawn(async { panic!("Injected inner worker panic") });
            let remaining = tokio::spawn(async move {
                tokio::task::yield_now().await;
                finished_sender.send(()).unwrap();
            });
            let supervisor = tokio::spawn(async move {
                wait_for_transcription_workers(vec![failed, remaining]).await
            });
            let error = supervisor.await.unwrap().unwrap_err();
            assert!(error.contains("Injected inner worker panic"));
            assert!(finished_receiver.await.is_ok());
        }

        #[tokio::test]
        async fn completed_workers_return_success() {
            assert!(wait_for_transcription_workers(vec![tokio::spawn(async {})]).await.is_ok());
        }

        #[test]
        fn keeps_short_acknowledgements() {
            assert!(should_emit_transcript("Yes"));
            assert!(should_emit_transcript("ok"));
        }

        #[test]
        fn drops_empty_and_whitespace_only() {
            assert!(!should_emit_transcript(""));
            assert!(!should_emit_transcript("   "));
        }

        /// Build a chunk with only the fields timing resolution reads.
        fn chunk(start: f64, end: Option<f64>, samples: usize, sample_rate: u32) -> AudioChunk {
            AudioChunk {
                data: vec![0.0; samples],
                sample_rate,
                timestamp: start,
                end_timestamp: end,
                chunk_id: 1,
                device_type: DeviceType::Microphone,
                source_activity: None,
            }
        }

        /// A valid VAD end wins over the sample-derived duration.
        #[test]
        fn timing_prefers_vad_boundaries() {
            let timing = resolve_transcript_timing(&chunk(12.25, Some(14.75), 16_000, 16_000));
            assert_eq!(timing, (12.25, 14.75, 2.5));
        }

        /// Producers without a VAD end fall back to samples divided by sample rate.
        #[test]
        fn timing_falls_back_to_sample_duration_without_vad_end() {
            let timing = resolve_transcript_timing(&chunk(3.0, None, 24_000, 16_000));
            assert_eq!(timing, (3.0, 4.5, 1.5));
        }

        /// An end before the start is ignored in favour of the sample duration.
        #[test]
        fn timing_rejects_an_end_before_the_start() {
            let timing = resolve_transcript_timing(&chunk(8.0, Some(7.0), 8_000, 16_000));
            assert_eq!(timing, (8.0, 8.5, 0.5));
        }

        #[test]
        fn silent_system_audio_does_not_identify_the_microphone_speaker() {
            let activity = SourceActivity {
                microphone_active: true,
                system_active: false,
                system_available: true,
                overlap_detected: false,
            };
            let assignment = assign_live_speaker(Some(&activity), &[], &mut None);
            assert_eq!(assignment.speaker_id.as_deref(), Some("unknown-speaker"));
            assert_eq!(assignment.speaker_label.as_deref(), Some("Unknown speaker"));
            assert_eq!(assignment.speaker_source.as_deref(), Some("microphone"));
            assert_eq!(assignment.diarization_status.as_deref(), Some("needs_review"));
        }

        #[test]
        fn simultaneous_sources_are_never_forced_to_one_speaker() {
            let activity = SourceActivity {
                microphone_active: true,
                system_active: true,
                system_available: true,
                overlap_detected: true,
            };
            let assignment = assign_live_speaker(Some(&activity), &[], &mut None);
            assert_eq!(assignment.speaker_label.as_deref(), Some("Multiple speakers"));
            assert_eq!(assignment.is_overlap, Some(true));
            assert_eq!(assignment.speaker_id, None);
            assert_eq!(assignment.candidate_speaker_ids, None);
        }

        #[test]
        fn microphone_only_room_audio_is_not_assumed_to_be_you() {
            let activity = SourceActivity {
                microphone_active: true,
                system_active: false,
                system_available: false,
                overlap_detected: false,
            };
            let assignment = assign_live_speaker(Some(&activity), &[], &mut None);
            assert_eq!(assignment.speaker_label.as_deref(), Some("Unknown speaker"));
        }

        #[test]
        fn alternating_sources_do_not_claim_simultaneous_overlap() {
            let activity = SourceActivity {
                microphone_active: true,
                system_active: true,
                system_available: true,
                overlap_detected: false,
            };
            let assignment = assign_live_speaker(Some(&activity), &[], &mut None);
            assert_eq!(assignment.is_overlap, Some(false));
            assert_eq!(
                assignment.diarization_status.as_deref(),
                Some("needs_review")
            );
        }

        #[test]
        fn logging_worker_sampling_and_diagnostics_are_bounded() {
            for accepted_count in [1, 2, 3, 4, 5, 25, 50] {
                assert!(should_log_transcript_snippet(accepted_count));
            }
            for accepted_count in [6, 24, 26, 49] {
                assert!(!should_log_transcript_snippet(accepted_count));
            }

            let mut stats = WorkerLogStats::default();
            for category in [
                WorkerFailureCategory::ModelNotLoaded,
                WorkerFailureCategory::EngineFailed,
                WorkerFailureCategory::UnsupportedLanguage,
                WorkerFailureCategory::SpeechDetectedEmit,
                WorkerFailureCategory::TranscriptUpdateEmit,
            ] {
                stats.record_failure(category);
            }
            let snapshot = stats.snapshot_and_reset();
            assert_eq!(snapshot.failed, 5);
            assert_eq!(snapshot.model_not_loaded_failures, 1);
            assert_eq!(snapshot.engine_failures, 1);
            assert_eq!(snapshot.unsupported_language_failures, 1);
            assert_eq!(snapshot.speech_detected_emit_failures, 1);
            assert_eq!(snapshot.transcript_update_emit_failures, 1);
            assert_eq!(stats.failed, 0);
            assert_eq!(stats.model_not_loaded_failures, 0);
            assert_eq!(stats.engine_failures, 0);
            assert_eq!(stats.unsupported_language_failures, 0);
            assert_eq!(stats.speech_detected_emit_failures, 0);
            assert_eq!(stats.transcript_update_emit_failures, 0);
        }
    }
