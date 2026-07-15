use serde::{Deserialize, Serialize};
use tauri::Manager;
use tauri::{AppHandle, Runtime, State};

use crate::{
    database::{
        models::Transcript,
        repositories::diarization::{
            diarization_status_to_database_value, DiarizationRepository, MeetingSpeaker,
        },
        repositories::speaker_profile::SpeakerProfilesRepository,
    },
    diarization::{
        alignment::assign_speaker_to_transcript,
        types::{DiarizationStatus, SpeakerSegment, TranscriptWindow},
    },
    state::AppState,
};

pub use super::finalize::StartFinalDiarizationRequest;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiarizationModelStatus {
    pub installed: bool,
    pub embedding_installed: bool,
    pub offline_installed: bool,
    pub filename: String,
    pub version: String,
    pub size_bytes: u64,
}

#[tauri::command]
pub async fn get_diarization_model_status<R: Runtime>(
    app: AppHandle<R>,
) -> Result<DiarizationModelStatus, String> {
    let embedding_installed = crate::diarization::models::is_embedding_model_present(&app);
    let offline_installed = super::finalize::offline_models_present(&app);
    Ok(DiarizationModelStatus {
        installed: embedding_installed && offline_installed,
        embedding_installed,
        offline_installed,
        filename: crate::diarization::models::EMBEDDING_MODEL_FILENAME.to_string(),
        version: crate::diarization::models::EMBEDDING_MODEL_VERSION.to_string(),
        size_bytes: crate::diarization::models::EMBEDDING_MODEL_SIZE,
    })
}

#[tauri::command]
pub async fn download_diarization_model<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    crate::diarization::models::download_embedding_model(&app).await?;
    super::finalize::prepare_offline_models(&app)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn delete_diarization_model<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    crate::diarization::models::delete_embedding_model(&app)?;
    super::finalize::delete_offline_models(&app).map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn start_final_diarization<R: Runtime>(
    app: AppHandle<R>,
    request: StartFinalDiarizationRequest,
) -> Result<super::finalize::FinalDiarizationResult, String> {
    super::finalize::run(app, request)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn retry_final_diarization<R: Runtime>(
    app: AppHandle<R>,
    request: StartFinalDiarizationRequest,
) -> Result<super::finalize::FinalDiarizationResult, String> {
    super::finalize::run(app, request)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn cancel_final_diarization() -> Result<(), String> {
    super::finalize::cancel();
    Ok(())
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct MeetingDiarizationStatusDto {
    pub meeting_id: String,
    pub current_status: String,
    pub progress: i64,
    pub method: Option<String>,
    pub failure_reason: Option<String>,
    pub quality_flags: Option<String>,
    pub assigned_coverage: Option<f64>,
    pub transcription_success_rate: Option<f64>,
}

#[tauri::command]
pub async fn get_meeting_diarization_status(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Option<MeetingDiarizationStatusDto>, String> {
    sqlx::query_as::<_, MeetingDiarizationStatusDto>(
        "SELECT meeting_id, current_status, progress, method, failure_reason,
                quality_flags, assigned_coverage, transcription_success_rate
         FROM meeting_diarization_status WHERE meeting_id = ?",
    )
    .bind(meeting_id)
    .fetch_optional(state.db_manager.pool())
    .await
    .map_err(|error| error.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiarizationSettingsDto {
    pub enabled: bool,
    pub mode: String,
    pub show_provisional_labels: bool,
    pub post_call_refinement_enabled: bool,
    pub overlap_handling: String,
    pub speaker_review_enabled: bool,
}

#[tauri::command]
pub async fn get_diarization_settings(
    state: State<'_, AppState>,
) -> Result<DiarizationSettingsDto, String> {
    let settings = DiarizationRepository::get_settings(state.db_manager.pool())
        .await
        .map_err(|error| error.to_string())?;

    Ok(DiarizationSettingsDto {
        enabled: settings.enabled != 0,
        mode: settings.mode,
        show_provisional_labels: settings.show_provisional_labels != 0,
        post_call_refinement_enabled: settings.post_call_refinement_enabled != 0,
        overlap_handling: settings.overlap_handling,
        speaker_review_enabled: settings.speaker_review_enabled != 0,
    })
}

pub(crate) async fn diarization_enabled_for_app<R: Runtime>(app: &AppHandle<R>) -> bool {
    let state = app.state::<AppState>();
    DiarizationRepository::get_settings(state.db_manager.pool())
        .await
        .map(|settings| settings.enabled != 0)
        .unwrap_or(false)
}

#[tauri::command]
pub async fn save_diarization_settings(
    state: State<'_, AppState>,
    settings: DiarizationSettingsDto,
) -> Result<(), String> {
    DiarizationRepository::save_settings(
        state.db_manager.pool(),
        settings.enabled,
        &settings.mode,
        settings.show_provisional_labels,
        settings.post_call_refinement_enabled,
        &settings.overlap_handling,
        settings.speaker_review_enabled,
    )
    .await
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn get_meeting_speakers(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Vec<MeetingSpeaker>, String> {
    if meeting_id.trim().is_empty() {
        return Err("Meeting id cannot be empty".to_string());
    }

    DiarizationRepository::get_meeting_speakers(state.db_manager.pool(), &meeting_id)
        .await
        .map_err(|error| error.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameSpeakerRequest {
    pub meeting_id: String,
    pub speaker_id: String,
    pub label: String,
}

#[tauri::command]
pub async fn rename_speaker(
    state: State<'_, AppState>,
    request: RenameSpeakerRequest,
) -> Result<(), String> {
    let RenameSpeakerRequest {
        meeting_id,
        speaker_id,
        label,
    } = request;
    let label = normalize_speaker_label(&label)?;

    if meeting_id.trim().is_empty() {
        return Err("Meeting id cannot be empty".to_string());
    }

    if speaker_id.trim().is_empty() {
        return Err("Speaker id cannot be empty".to_string());
    }

    let updated_count = DiarizationRepository::rename_speaker(
        state.db_manager.pool(),
        &meeting_id,
        &speaker_id,
        &label,
    )
    .await
    .map_err(|error| error.to_string())?;

    if updated_count == 0 {
        return Err("Speaker not found for meeting".to_string());
    }

    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeSpeakersRequest {
    pub meeting_id: String,
    pub source_speaker_ids: Vec<String>,
    pub target_speaker_id: String,
    pub target_label: String,
}

#[tauri::command]
pub async fn merge_speakers(
    state: State<'_, AppState>,
    request: MergeSpeakersRequest,
) -> Result<u64, String> {
    let label = normalize_speaker_label(&request.target_label)?;
    DiarizationRepository::merge_speakers(
        state.db_manager.pool(),
        &request.meeting_id,
        &request.source_speaker_ids,
        &request.target_speaker_id,
        &label,
    )
    .await
    .map_err(|error| error.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReassignTranscriptsRequest {
    pub meeting_id: String,
    pub transcript_ids: Vec<String>,
    pub speaker_id: Option<String>,
    pub speaker_label: Option<String>,
    #[serde(default)]
    pub is_overlap: bool,
    #[serde(default)]
    pub candidate_speaker_ids: Vec<String>,
}

#[tauri::command]
pub async fn reassign_transcripts(
    state: State<'_, AppState>,
    request: ReassignTranscriptsRequest,
) -> Result<u64, String> {
    if request.transcript_ids.is_empty() {
        return Err("Select at least one transcript segment".to_string());
    }
    DiarizationRepository::reassign_transcripts(
        state.db_manager.pool(),
        &request.meeting_id,
        &request.transcript_ids,
        request.speaker_id.as_deref(),
        request.speaker_label.as_deref(),
        request.is_overlap,
        &request.candidate_speaker_ids,
    )
    .await
    .map_err(|error| error.to_string())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeakerProfileDto {
    pub id: String,
    pub name: String,
    pub model_version: String,
    pub sample_count: i64,
    pub speech_seconds: f64,
}

#[tauri::command]
pub async fn list_speaker_profiles(
    state: State<'_, AppState>,
) -> Result<Vec<SpeakerProfileDto>, String> {
    SpeakerProfilesRepository::list(state.db_manager.pool())
        .await
        .map(|profiles| {
            profiles
                .into_iter()
                .map(|profile| SpeakerProfileDto {
                    id: profile.id,
                    name: profile.name,
                    model_version: profile.model_version,
                    sample_count: profile.sample_count,
                    speech_seconds: profile.speech_seconds,
                })
                .collect()
        })
        .map_err(|error| error.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSpeakerProfileRequest {
    pub profile_id: String,
    pub name: String,
}

#[tauri::command]
pub async fn update_speaker_profile(
    state: State<'_, AppState>,
    request: UpdateSpeakerProfileRequest,
) -> Result<(), String> {
    let name = normalize_speaker_label(&request.name)?;
    SpeakerProfilesRepository::rename(state.db_manager.pool(), &request.profile_id, &name)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn delete_speaker_profile(
    state: State<'_, AppState>,
    profile_id: String,
) -> Result<(), String> {
    SpeakerProfilesRepository::delete(state.db_manager.pool(), &profile_id)
        .await
        .map_err(|error| error.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RememberSpeakerRequest {
    pub meeting_id: String,
    pub speaker_id: String,
    pub name: String,
}

#[tauri::command]
pub async fn remember_speaker<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    request: RememberSpeakerRequest,
) -> Result<String, String> {
    const MIN_PROFILE_SPEECH_SECONDS: f64 = 8.0;
    let name = normalize_speaker_label(&request.name)?;
    let model_path = crate::diarization::models::embedding_model_path(&app)?;
    crate::diarization::models::validate_model_file(&model_path)?;

    let folder_path: Option<String> =
        sqlx::query_scalar("SELECT folder_path FROM meetings WHERE id = ?")
            .bind(&request.meeting_id)
            .fetch_optional(state.db_manager.pool())
            .await
            .map_err(|error| error.to_string())?
            .flatten();
    let folder = std::path::PathBuf::from(
        folder_path.ok_or_else(|| "This meeting has no retained audio folder".to_string())?,
    );

    let segments: Vec<(String, f64, f64, Option<f64>, i64)> = sqlx::query_as(
        "SELECT source, start_time, end_time, confidence, is_overlap
         FROM speaker_segments
         WHERE meeting_id = ? AND speaker_id = ?
         ORDER BY start_time",
    )
    .bind(&request.meeting_id)
    .bind(&request.speaker_id)
    .fetch_all(state.db_manager.pool())
    .await
    .map_err(|error| error.to_string())?;

    let clean_segments: Vec<_> = segments
        .into_iter()
        .filter(|(_, start, end, confidence, overlap)| {
            *overlap == 0 && end > start && confidence.unwrap_or(0.0) >= 0.8
        })
        .collect();
    let speech_seconds: f64 = clean_segments
        .iter()
        .map(|(_, start, end, _, _)| end - start)
        .sum();
    if speech_seconds < MIN_PROFILE_SPEECH_SECONDS {
        return Err(format!(
            "At least {MIN_PROFILE_SPEECH_SECONDS:.0} seconds of clean, non-overlapping speech is required"
        ));
    }

    let source = clean_segments
        .first()
        .map(|segment| segment.0.as_str())
        .unwrap_or("mixed");
    let preferred_path = match source {
        "microphone" => folder.join(".diarization/microphone.flac"),
        "system" => folder.join(".diarization/system.flac"),
        _ => folder.join("audio.mp4"),
    };
    let audio_path = if preferred_path.exists() {
        preferred_path
    } else {
        folder.join("audio.mp4")
    };
    if !audio_path.exists() {
        return Err("The audio needed to remember this voice is unavailable".to_string());
    }

    let sample_count = clean_segments.len() as i64;
    let embedding = tokio::task::spawn_blocking(move || -> Result<Vec<f32>, String> {
        let decoded = crate::audio::decoder::decode_audio_file(&audio_path)
            .map_err(|error| error.to_string())?;
        let audio = decoded.to_whisper_format();
        let mut selected = Vec::new();
        for (_, start, end, _, _) in clean_segments {
            let start_sample = (start.max(0.0) * 16_000.0) as usize;
            let end_sample = (end.max(start) * 16_000.0) as usize;
            if start_sample < audio.len() {
                selected.extend_from_slice(&audio[start_sample..end_sample.min(audio.len())]);
            }
        }
        let mut extractor = crate::diarization::embedding::EmbeddingExtractor::new(&model_path)
            .map_err(|error| error.to_string())?;
        extractor
            .compute(&selected)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())??;

    let profile_id = SpeakerProfilesRepository::create(
        state.db_manager.pool(),
        &name,
        &embedding,
        crate::diarization::models::EMBEDDING_MODEL_VERSION,
        sample_count,
        speech_seconds,
    )
    .await
    .map_err(|error| error.to_string())?;

    sqlx::query(
        "UPDATE transcripts SET speaker_profile_id = ?, speaker_label = ?
         WHERE meeting_id = ? AND speaker_id = ?",
    )
    .bind(&profile_id)
    .bind(&name)
    .bind(&request.meeting_id)
    .bind(&request.speaker_id)
    .execute(state.db_manager.pool())
    .await
    .map_err(|error| error.to_string())?;

    Ok(profile_id)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyDiarizationRequest {
    pub meeting_id: String,
    pub method: String,
    pub segments: Vec<SpeakerSegment>,
}

#[tauri::command]
pub async fn apply_diarization_segments(
    state: State<'_, AppState>,
    request: ApplyDiarizationRequest,
) -> Result<(), String> {
    let pool = state.db_manager.pool();
    let ApplyDiarizationRequest {
        meeting_id,
        method,
        segments,
    } = request;
    let segments = segments_for_meeting(segments, &meeting_id);

    let mut transaction = pool.begin().await.map_err(|error| error.to_string())?;

    replace_speaker_segments(&mut transaction, &meeting_id, &method, &segments)
        .await
        .map_err(|error| error.to_string())?;
    upsert_meeting_diarization_status(&mut transaction, &meeting_id, &method, &segments)
        .await
        .map_err(|error| error.to_string())?;

    let transcripts = sqlx::query_as::<_, Transcript>(
        "SELECT * FROM transcripts WHERE meeting_id = ? ORDER BY audio_start_time ASC",
    )
    .bind(&meeting_id)
    .fetch_all(&mut *transaction)
    .await
    .map_err(|error| error.to_string())?;

    for transcript in transcripts {
        let window = TranscriptWindow {
            transcript_id: transcript.id.clone(),
            audio_start_time: transcript.audio_start_time,
            audio_end_time: transcript.audio_end_time,
        };

        let assignment = assign_speaker_to_transcript(&window, &segments, 0.1, &method);

        DiarizationRepository::update_transcript_assignment(
            &mut *transaction,
            &transcript.id,
            &assignment,
        )
        .await
        .map_err(|error| error.to_string())?;
    }

    transaction
        .commit()
        .await
        .map_err(|error| error.to_string())?;

    Ok(())
}

fn segments_for_meeting(segments: Vec<SpeakerSegment>, meeting_id: &str) -> Vec<SpeakerSegment> {
    segments
        .into_iter()
        .filter(|segment| segment.meeting_id.as_str() == meeting_id)
        .collect()
}

async fn replace_speaker_segments(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    meeting_id: &str,
    method: &str,
    segments: &[SpeakerSegment],
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM speaker_segments WHERE meeting_id = ?")
        .bind(meeting_id)
        .execute(&mut **transaction)
        .await?;

    for (index, segment) in segments.iter().enumerate() {
        let segment_id = format!("{meeting_id}:{method}:{index}");
        let segment_status = diarization_status_to_database_value(segment.diarization_status);
        let segment_method = segment.diarization_method.as_deref().unwrap_or(method);

        sqlx::query(
            "INSERT INTO speaker_segments (
                id, meeting_id, source, start_time, end_time,
                speaker_id, speaker_label, speaker_profile_id,
                candidate_speaker_ids, confidence, is_overlap,
                diarization_status, diarization_method
             )
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(segment_id)
        .bind(meeting_id)
        .bind(&segment.source)
        .bind(segment.start_time)
        .bind(segment.end_time)
        .bind(&segment.speaker_id)
        .bind(&segment.speaker_label)
        .bind(&segment.speaker_profile_id)
        .bind(serde_json::to_string(&segment.candidate_speaker_ids).ok())
        .bind(segment.confidence)
        .bind(segment.is_overlap as i64)
        .bind(segment_status)
        .bind(segment_method)
        .execute(&mut **transaction)
        .await?;
    }

    Ok(())
}

async fn upsert_meeting_diarization_status(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    meeting_id: &str,
    method: &str,
    segments: &[SpeakerSegment],
) -> Result<(), sqlx::Error> {
    let current_status = meeting_status_from_segments(segments);
    let method = method.to_ascii_lowercase();
    let live_enabled = current_status == "provisional"
        || current_status == "fallback_to_live"
        || method.contains("live")
        || method.contains("online");
    let post_call_enabled =
        current_status == "final" || method.contains("post") || method.contains("offline");

    sqlx::query(
        "INSERT INTO meeting_diarization_status (
            meeting_id, enabled, live_enabled, post_call_enabled,
            current_status, quality_flags, processed_at, updated_at
         )
         VALUES (?, 1, ?, ?, ?, NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
         ON CONFLICT(meeting_id) DO UPDATE SET
            enabled = 1,
            live_enabled = excluded.live_enabled,
            post_call_enabled = excluded.post_call_enabled,
            current_status = excluded.current_status,
            quality_flags = NULL,
            processed_at = CURRENT_TIMESTAMP,
            updated_at = CURRENT_TIMESTAMP",
    )
    .bind(meeting_id)
    .bind(live_enabled as i64)
    .bind(post_call_enabled as i64)
    .bind(current_status)
    .execute(&mut **transaction)
    .await?;

    Ok(())
}

fn meeting_status_from_segments(segments: &[SpeakerSegment]) -> &'static str {
    if segments.is_empty() {
        return "needs_review";
    }

    if segments
        .iter()
        .any(|segment| segment.diarization_status == DiarizationStatus::Failed)
    {
        return "failed";
    }

    if segments
        .iter()
        .any(|segment| segment.diarization_status == DiarizationStatus::NeedsReview)
    {
        return "needs_review";
    }

    if segments
        .iter()
        .any(|segment| segment.diarization_status == DiarizationStatus::FallbackToLive)
    {
        return "fallback_to_live";
    }

    if segments
        .iter()
        .any(|segment| segment.diarization_status == DiarizationStatus::Provisional)
    {
        return "provisional";
    }

    if segments
        .iter()
        .all(|segment| segment.diarization_status == DiarizationStatus::Final)
    {
        return "final";
    }

    "none"
}

fn normalize_speaker_label(label: &str) -> Result<String, String> {
    let label = label.trim();

    if label.is_empty() {
        return Err("Speaker label cannot be empty".to_string());
    }

    Ok(label.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(meeting_id: &str, speaker_label: &str) -> SpeakerSegment {
        SpeakerSegment {
            meeting_id: meeting_id.to_string(),
            source: "unit_test".to_string(),
            start_time: 0.0,
            end_time: 1.0,
            speaker_id: Some(speaker_label.to_lowercase().replace(' ', "-")),
            speaker_label: Some(speaker_label.to_string()),
            speaker_profile_id: None,
            candidate_speaker_ids: Vec::new(),
            confidence: Some(0.8),
            is_overlap: false,
            diarization_status: DiarizationStatus::Provisional,
            diarization_method: Some("unit_test".to_string()),
        }
    }

    #[test]
    fn segments_for_meeting_excludes_segments_from_other_meetings() {
        let segments = segments_for_meeting(
            vec![
                segment("meeting-1", "Speaker 1"),
                segment("meeting-2", "Speaker 2"),
            ],
            "meeting-1",
        );

        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].meeting_id, "meeting-1");
        assert_eq!(segments[0].speaker_label.as_deref(), Some("Speaker 1"));
    }

    #[test]
    fn meeting_status_from_segments_returns_needs_review_for_empty_segments() {
        assert_eq!(meeting_status_from_segments(&[]), "needs_review");
    }

    #[test]
    fn meeting_status_from_segments_prefers_failed_over_final_segments() {
        let mut failed = segment("meeting-1", "Speaker 1");
        failed.diarization_status = DiarizationStatus::Failed;
        let mut final_segment = segment("meeting-1", "Speaker 2");
        final_segment.diarization_status = DiarizationStatus::Final;

        assert_eq!(
            meeting_status_from_segments(&[final_segment, failed]),
            "failed"
        );
    }

    #[test]
    fn normalize_speaker_label_trims_non_empty_label() {
        assert_eq!(
            normalize_speaker_label("  Alice Johnson  ").expect("label should be valid"),
            "Alice Johnson"
        );
    }

    #[test]
    fn normalize_speaker_label_rejects_blank_label() {
        assert_eq!(
            normalize_speaker_label(" \n\t ").expect_err("blank label should be rejected"),
            "Speaker label cannot be empty"
        );
    }
}
