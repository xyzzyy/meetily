use serde::{Deserialize, Serialize};
use tauri::Manager;
use tauri::{AppHandle, Runtime, State};

use crate::{
    database::{
        models::Transcript,
        repositories::diarization::{
            diarization_status_to_database_value, DiarizationRepository, MeetingSpeaker,
            TranscriptCorrectionWindow, TranscriptSpeakerMetadata,
        },
        repositories::speaker_profile::{SampleRemoval, SampleSource, SpeakerProfilesRepository},
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
    /// Whether post-call refinement can run on this machine.
    pub refinement_supported: bool,
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
    let refinement_supported = super::finalize::refinement_supported();
    Ok(DiarizationModelStatus {
        // Live labels only need the embedding model. The offline models matter
        // only where post-call refinement can run.
        installed: embedding_installed && (offline_installed || !refinement_supported),
        embedding_installed,
        offline_installed,
        refinement_supported,
        filename: crate::diarization::models::EMBEDDING_MODEL_FILENAME.to_string(),
        version: crate::diarization::models::EMBEDDING_MODEL_VERSION.to_string(),
        size_bytes: crate::diarization::models::EMBEDDING_MODEL_SIZE,
    })
}

#[tauri::command]
pub async fn download_diarization_model<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    let _operation = super::operations::try_processing_slot()?;
    crate::diarization::models::download_embedding_model(&app).await?;
    if !super::finalize::refinement_supported() {
        return Ok(());
    }
    super::finalize::prepare_offline_models(&app)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn delete_diarization_model<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    let _operation = super::operations::try_processing_slot()?;
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
    .fetch_optional(state.speaker_store.pool())
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
    let settings = DiarizationRepository::get_settings(state.speaker_store.pool())
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
    DiarizationRepository::get_settings(state.speaker_store.pool())
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
        state.speaker_store.pool(),
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

    DiarizationRepository::get_meeting_speakers(state.speaker_store.pool(), &meeting_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn get_transcript_speaker_metadata(
    state: State<'_, AppState>,
    transcript_ids: Vec<String>,
) -> Result<Vec<TranscriptSpeakerMetadata>, String> {
    if transcript_ids.len() > 2_000 {
        return Err("At most 2,000 transcript IDs may be requested at once".to_string());
    }
    DiarizationRepository::get_transcript_metadata(state.speaker_store.pool(), &transcript_ids)
        .await
        .map_err(|error| error.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeakerColorRequest {
    pub meeting_id: String,
    pub speaker_id: String,
    pub color: String,
}

#[tauri::command]
pub async fn set_speaker_color(
    state: State<'_, AppState>,
    request: SpeakerColorRequest,
) -> Result<(), String> {
    if request.meeting_id.trim().is_empty() || request.speaker_id.trim().is_empty() {
        return Err("Meeting and speaker IDs are required".into());
    }
    if !super::colors::PALETTE
        .iter()
        .any(|choice| choice.value == request.color)
    {
        return Err("Choose a color from the speaker palette".into());
    }
    let _edit = super::operations::try_meeting_edit(&request.meeting_id)?;
    let speakers = DiarizationRepository::get_meeting_speakers(
        state.speaker_store.pool(),
        &request.meeting_id,
    )
    .await
    .map_err(|error| error.to_string())?;
    if speakers.iter().any(|speaker| {
        speaker.speaker_id != request.speaker_id
            && speaker.speaker_color.as_deref() == Some(request.color.as_str())
    }) {
        return Err("Another speaker already uses this color".into());
    }
    let updated = DiarizationRepository::set_speaker_color(
        state.speaker_store.pool(),
        &request.meeting_id,
        &request.speaker_id,
        &request.color,
    )
    .await
    .map_err(|error| error.to_string())?;
    if updated == 0 {
        return Err("Speaker not found for meeting".into());
    }
    Ok(())
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
    let _edit = super::operations::try_meeting_edit(&request.meeting_id)?;
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
        state.speaker_store.pool(),
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
    let _edit = super::operations::try_meeting_edit(&request.meeting_id)?;
    let label = normalize_speaker_label(&request.target_label)?;
    DiarizationRepository::merge_speakers(
        state.speaker_store.pool(),
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
    let _edit = super::operations::try_meeting_edit(&request.meeting_id)?;
    if request.transcript_ids.is_empty() {
        return Err("Select at least one transcript segment".to_string());
    }
    if request.transcript_ids.len() > 2_000 {
        return Err("Select at most 2,000 transcript rows".to_string());
    }
    let requested = request
        .transcript_ids
        .iter()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    let rows = sqlx::query_as::<_, Transcript>(
        "SELECT * FROM transcripts WHERE meeting_id = ? ORDER BY audio_start_time, id",
    )
    .bind(&request.meeting_id)
    .fetch_all(state.db_manager.pool())
    .await
    .map_err(|error| error.to_string())?;
    let windows = rows
        .into_iter()
        .filter(|row| requested.contains(&row.id))
        .map(|row| TranscriptCorrectionWindow {
            transcript_id: row.id,
            audio_start_time: row.audio_start_time,
            audio_end_time: row.audio_end_time,
            speaker_source: None, // Preserve the existing channel; missing metadata uses mixed audio.
        })
        .collect::<Vec<_>>();
    if windows.len() != requested.len() {
        return Err(
            "Some selected rows no longer exist in this meeting. Reload the transcript."
                .to_string(),
        );
    }
    let label = request
        .speaker_label
        .as_deref()
        .map(normalize_speaker_label)
        .transpose()?;
    if !request.is_overlap
        && (request
            .speaker_id
            .as_ref()
            .map_or(true, |id| id.trim().is_empty())
            || label.is_none())
    {
        return Err("Choose a speaker or enter a new speaker name".to_string());
    }
    DiarizationRepository::reassign_transcripts(
        state.speaker_store.pool(),
        &request.meeting_id,
        &windows,
        request.speaker_id.as_deref(),
        label.as_deref(),
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
    /// How many meeting speakers make up this voice.
    pub recording_count: i64,
}

#[tauri::command]
pub async fn list_speaker_profiles(
    state: State<'_, AppState>,
) -> Result<Vec<SpeakerProfileDto>, String> {
    let pool = state.speaker_store.pool();
    let profiles = SpeakerProfilesRepository::list(pool)
        .await
        .map_err(|error| error.to_string())?;
    let counts: std::collections::HashMap<String, i64> = sqlx::query_as::<_, (String, i64)>(
        "SELECT profile_id, COUNT(*) FROM speaker_profile_samples GROUP BY profile_id",
    )
    .fetch_all(pool)
    .await
    .map_err(|error| error.to_string())?
    .into_iter()
    .collect();
    Ok(profiles
        .into_iter()
        .map(|profile| SpeakerProfileDto {
            recording_count: counts.get(&profile.id).copied().unwrap_or(0),
            id: profile.id,
            name: profile.name,
            model_version: profile.model_version,
            sample_count: profile.sample_count,
            speech_seconds: profile.speech_seconds,
        })
        .collect())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeakerProfileSampleDto {
    pub meeting_id: String,
    pub speaker_id: String,
    /// None when the meeting has since been deleted.
    pub meeting_title: Option<String>,
    pub meeting_created_at: Option<String>,
    /// The speaker's name in that meeting.
    pub speaker_label: Option<String>,
    pub speech_seconds: f64,
    /// False for contributions that can't be separated from the blend.
    pub removable: bool,
}

/// List the meeting speakers that make up a remembered voice.
#[tauri::command]
pub async fn list_speaker_profile_samples(
    state: State<'_, AppState>,
    profile_id: String,
) -> Result<Vec<SpeakerProfileSampleDto>, String> {
    let samples = SpeakerProfilesRepository::list_samples(state.speaker_store.pool(), &profile_id)
        .await
        .map_err(|error| error.to_string())?;
    let mut result = Vec::with_capacity(samples.len());
    for sample in samples {
        let meeting: Option<(String, String)> =
            sqlx::query_as("SELECT title, created_at FROM meetings WHERE id = ?")
                .bind(&sample.meeting_id)
                .fetch_optional(state.db_manager.pool())
                .await
                .map_err(|error| error.to_string())?;
        let speaker_label: Option<String> = sqlx::query_scalar(
            "SELECT speaker_label FROM transcript_speakers
             WHERE meeting_id = ? AND speaker_id = ? AND speaker_label IS NOT NULL LIMIT 1",
        )
        .bind(&sample.meeting_id)
        .bind(&sample.speaker_id)
        .fetch_optional(state.speaker_store.pool())
        .await
        .map_err(|error| error.to_string())?;
        let (meeting_title, meeting_created_at) = meeting.unzip();
        result.push(SpeakerProfileSampleDto {
            meeting_id: sample.meeting_id,
            speaker_id: sample.speaker_id,
            meeting_title,
            meeting_created_at,
            speaker_label,
            speech_seconds: sample.speech_seconds,
            removable: sample.removable,
        });
    }
    Ok(result)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveSpeakerProfileSampleRequest {
    pub profile_id: String,
    pub meeting_id: String,
    pub speaker_id: String,
}

/// Remove one meeting's speech from a remembered voice. The voice is rebuilt
/// from its other samples, or forgotten if none remain. Returns
/// `"recalculated"` or `"forgotten"`.
#[tauri::command]
pub async fn remove_speaker_profile_sample(
    state: State<'_, AppState>,
    request: RemoveSpeakerProfileSampleRequest,
) -> Result<String, String> {
    let _edit = super::operations::try_meeting_edit(&request.meeting_id)?;
    let outcome = SpeakerProfilesRepository::remove_sample(
        state.speaker_store.pool(),
        &request.profile_id,
        SampleSource {
            meeting_id: &request.meeting_id,
            speaker_id: &request.speaker_id,
        },
    )
    .await
    .map_err(|error| match error {
        sqlx::Error::RowNotFound => "That recording is no longer part of this voice".to_string(),
        other => other.to_string(),
    })?;
    Ok(match outcome {
        SampleRemoval::Recalculated => "recalculated",
        SampleRemoval::Forgotten => "forgotten",
    }
    .to_string())
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
    SpeakerProfilesRepository::rename(state.speaker_store.pool(), &request.profile_id, &name)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn delete_speaker_profile(
    state: State<'_, AppState>,
    profile_id: String,
) -> Result<(), String> {
    SpeakerProfilesRepository::delete(state.speaker_store.pool(), &profile_id)
        .await
        .map_err(|error| error.to_string())
}

/// Minimum clean, non-overlapping speech needed to learn or extend a voice.
const MIN_PROFILE_SPEECH_SECONDS: f64 = 8.0;

/// Compute a voiceprint from one meeting speaker's clean speech.
///
/// Returns `None` when the speaker has less than
/// [`MIN_PROFILE_SPEECH_SECONDS`] of usable speech.
async fn collect_speaker_voice<R: Runtime>(
    app: &AppHandle<R>,
    state: &State<'_, AppState>,
    meeting_id: &str,
    speaker_id: &str,
) -> Result<Option<(Vec<f32>, f64, i64)>, String> {
    let model_path = crate::diarization::models::embedding_model_path(app)?;
    crate::diarization::models::validate_model_file(&model_path)?;

    let folder_path: Option<String> =
        sqlx::query_scalar("SELECT folder_path FROM meetings WHERE id = ?")
            .bind(meeting_id)
            .fetch_optional(state.db_manager.pool())
            .await
            .map_err(|error| error.to_string())?
            .flatten();
    let folder = std::path::PathBuf::from(
        folder_path.ok_or_else(|| "This meeting has no retained audio folder".to_string())?,
    );

    let current_ids: std::collections::HashSet<String> =
        sqlx::query_scalar("SELECT id FROM transcripts WHERE meeting_id = ?")
            .bind(meeting_id)
            .fetch_all(state.db_manager.pool())
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .collect();
    let segments: Vec<(String, String, f64, f64, Option<f64>, i64)> = sqlx::query_as(
        "SELECT transcript_id, COALESCE(speaker_source, 'mixed'), audio_start_time,
                audio_end_time, diarization_confidence, is_overlap
         FROM transcript_speakers
         WHERE meeting_id = ? AND speaker_id = ? AND published = 1
           AND audio_start_time IS NOT NULL AND audio_end_time IS NOT NULL
         ORDER BY audio_start_time",
    )
    .bind(meeting_id)
    .bind(speaker_id)
    .fetch_all(state.speaker_store.pool())
    .await
    .map_err(|error| error.to_string())?;
    let clean_segments = segments
        .into_iter()
        .filter(|(id, _, start, end, confidence, overlap)| {
            current_ids.contains(id)
                && *overlap == 0
                && start.is_finite()
                && end.is_finite()
                && end > start
                && confidence.unwrap_or(0.0) >= 0.8
        })
        .collect::<Vec<_>>();
    let fallback_audio = super::finalize::find_meeting_audio(&folder).ok();
    tokio::task::spawn_blocking(move || -> Result<Option<(Vec<f32>, f64, i64)>, String> {
        let mut ranges = std::collections::BTreeMap::<std::path::PathBuf, Vec<(f64, f64)>>::new();
        for (_, source, start, end, _, _) in clean_segments {
            let stem = match source.as_str() {
                "microphone" => Some(folder.join(".diarization/microphone.flac")),
                "system" => Some(folder.join(".diarization/system.flac")),
                _ => None,
            };
            let path = stem.filter(|path| path.exists()).or_else(|| fallback_audio.clone())
                .ok_or_else(|| "The audio needed to remember this voice is unavailable".to_string())?;
            ranges.entry(path).or_default().push((start, end));
        }
        let mut selected = Vec::new();
        let mut sample_count = 0i64;
        let max_samples = 30 * 16_000;
        for (path, mut intervals) in ranges {
            let audio = crate::audio::decoder::decode_audio_file(&path)
                .map_err(|error| error.to_string())?.to_whisper_format();
            intervals.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut previous_end = 0usize;
            for (start, end) in intervals {
                let start = ((start.max(0.0) * 16_000.0) as usize).max(previous_end);
                let end = ((end.max(0.0) * 16_000.0) as usize).min(audio.len());
                previous_end = previous_end.max(end);
                if start >= end || selected.len() >= max_samples { continue; }
                let end = end.min(start + max_samples - selected.len());
                selected.extend_from_slice(&audio[start..end]);
                sample_count += 1;
            }
            if selected.len() >= max_samples { break; }
        }
        let speech_seconds = selected.len() as f64 / 16_000.0;
        if speech_seconds < MIN_PROFILE_SPEECH_SECONDS {
            return Ok(None);
        }
        let mut extractor = crate::diarization::embedding::EmbeddingExtractor::new(&model_path)
            .map_err(|error| error.to_string())?;
        let embedding = extractor.compute(&selected).map_err(|error| error.to_string())?;
        Ok(Some((embedding, speech_seconds, sample_count)))
    }).await.map_err(|error| error.to_string())?
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributeSpeakerRequest {
    pub meeting_id: String,
    pub speaker_id: String,
    /// An existing remembered voice to attribute this speaker to.
    #[serde(default)]
    pub profile_id: Option<String>,
    /// A typed name. Matches an existing remembered voice by name, otherwise
    /// remembers a new one.
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AttributionOutcome {
    /// A new remembered voice was created from this speaker.
    Created,
    /// This speaker's speech was added to an existing remembered voice.
    Added,
    /// This speaker had already contributed to that voice; only the name was applied.
    AlreadyIncluded,
    /// The voice couldn't be learned (see `voice_note`); only the name was applied.
    NamedOnly,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributeSpeakerResponse {
    pub profile_id: Option<String>,
    pub name: String,
    pub outcome: AttributionOutcome,
    /// Why the voice wasn't learned, when only the name was applied.
    pub voice_note: Option<String>,
}

/// A reason voices can't be learned at all right now, if there is one.
fn voice_learning_blocker() -> Option<String> {
    let executable = match std::env::current_exe() {
        Ok(executable) => executable,
        Err(error) => return Some(format!("Unable to locate the running Meetily app: {error}.")),
    };
    is_running_from_mounted_disk_image(&executable).then(|| {
        "Move Meetily to Applications and eject the installer disk image to remember voices.".to_string()
    })
}

/// Attribute a meeting speaker to a person.
///
/// Picking an existing remembered voice (or typing its name) labels the
/// speaker and adds their speech to that voice. Typing a new name remembers a
/// new voice. A speaker with too little clean speech is still labelled.
#[tauri::command]
pub async fn attribute_speaker<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    request: AttributeSpeakerRequest,
) -> Result<AttributeSpeakerResponse, String> {
    let _edit = super::operations::try_meeting_edit(&request.meeting_id)?;
    let pool = state.speaker_store.pool();

    let existing = match (&request.profile_id, &request.name) {
        (Some(profile_id), _) => Some(
            SpeakerProfilesRepository::get(pool, profile_id)
                .await
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "That remembered voice no longer exists".to_string())?,
        ),
        (None, Some(name)) => {
            let name = normalize_speaker_label(name)?;
            SpeakerProfilesRepository::find_by_name(pool, &name)
                .await
                .map_err(|error| error.to_string())?
        }
        (None, None) => return Err("Choose a remembered voice or type a name".to_string()),
    };
    let name = match (&existing, &request.name) {
        (Some(profile), _) => profile.name.clone(),
        (None, Some(name)) => normalize_speaker_label(name)?,
        (None, None) => unreachable!("handled above"),
    };

    // Naming must always work, so any reason the voice can't be learned only
    // skips the voiceprint; it's reported back instead of failing the request.
    let (voice, voice_note) = match voice_learning_blocker() {
        Some(reason) => (None, Some(reason)),
        None => match collect_speaker_voice(&app, &state, &request.meeting_id, &request.speaker_id).await {
            Ok(Some(voice)) => (Some(voice), None),
            Ok(None) => (
                None,
                Some(format!(
                    "Not enough clear speech to learn this voice ({MIN_PROFILE_SPEECH_SECONDS:.0} seconds needed)."
                )),
            ),
            Err(error) => (None, Some(format!("The voice wasn't learned: {error}."))),
        },
    };
    let source = || SampleSource {
        meeting_id: &request.meeting_id,
        speaker_id: &request.speaker_id,
    };
    let model_version = crate::diarization::models::EMBEDDING_MODEL_VERSION;
    let (profile_id, outcome) = match (existing, voice) {
        (Some(profile), Some((embedding, seconds, samples))) => {
            let added = SpeakerProfilesRepository::add_sample(
                pool, &profile.id, &embedding, model_version, samples, seconds, source(),
            )
            .await
            .map_err(|error| error.to_string())?;
            let outcome = if added { AttributionOutcome::Added } else { AttributionOutcome::AlreadyIncluded };
            (Some(profile.id), outcome)
        }
        (Some(profile), None) => (Some(profile.id), AttributionOutcome::NamedOnly),
        (None, Some((embedding, seconds, samples))) => {
            let id = SpeakerProfilesRepository::create_from_sample(
                pool, &name, &embedding, model_version, samples, seconds, source(),
            )
            .await
            .map_err(|error| error.to_string())?;
            (Some(id), AttributionOutcome::Created)
        }
        (None, None) => (None, AttributionOutcome::NamedOnly),
    };

    let updated = DiarizationRepository::rename_speaker(
        pool,
        &request.meeting_id,
        &request.speaker_id,
        &name,
    )
    .await
    .map_err(|error| error.to_string())?;
    if updated == 0 {
        return Err("Speaker not found for meeting".to_string());
    }
    for table in ["transcript_speakers", "speaker_segments"] {
        sqlx::query(&format!(
            "UPDATE {table} SET speaker_profile_id = ? WHERE meeting_id = ? AND speaker_id = ?"
        ))
        .bind(&profile_id)
        .bind(&request.meeting_id)
        .bind(&request.speaker_id)
        .execute(pool)
        .await
        .map_err(|error| error.to_string())?;
    }

    Ok(AttributeSpeakerResponse { profile_id, name, outcome, voice_note })
}

fn is_running_from_mounted_disk_image(executable: &std::path::Path) -> bool {
    executable.starts_with("/Volumes/")
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
    let _edit = super::operations::try_meeting_edit(&request.meeting_id)?;
    let core_pool = state.db_manager.pool();
    let feature_pool = state.speaker_store.pool();
    let ApplyDiarizationRequest {
        meeting_id,
        method,
        segments,
    } = request;
    let segments = segments_for_meeting(segments, &meeting_id);

    let transcripts = sqlx::query_as::<_, Transcript>(
        "SELECT * FROM transcripts WHERE meeting_id = ? ORDER BY audio_start_time ASC",
    )
    .bind(&meeting_id)
    .fetch_all(core_pool)
    .await
    .map_err(|error| error.to_string())?;

    let mut transaction = feature_pool
        .begin()
        .await
        .map_err(|error| error.to_string())?;

    replace_speaker_segments(&mut transaction, &meeting_id, &method, &segments)
        .await
        .map_err(|error| error.to_string())?;
    upsert_meeting_diarization_status(&mut transaction, &meeting_id, &method, &segments)
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
            &meeting_id,
            transcript.audio_start_time,
            transcript.audio_end_time,
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

    #[test]
    fn detects_apps_running_from_a_mounted_disk_image() {
        assert!(is_running_from_mounted_disk_image(std::path::Path::new(
            "/Volumes/meetily/meetily.app/Contents/MacOS/meetily"
        )));
        assert!(!is_running_from_mounted_disk_image(std::path::Path::new(
            "/Applications/meetily.app/Contents/MacOS/meetily"
        )));
    }

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
