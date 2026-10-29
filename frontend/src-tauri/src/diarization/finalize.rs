use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::{AtomicBool, Ordering},
};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::Connection;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tokio::{io::AsyncWriteExt, process::Command};
use uuid::Uuid;

use crate::{audio::decoder::decode_audio_file, database::models::Transcript, state::AppState};

static CANCEL_REQUESTED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartFinalDiarizationRequest {
    pub meeting_id: String,
    #[serde(default)]
    pub overwrite_manual_edits: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FinalDiarizationResult {
    pub run_id: String,
    pub status: String,
    pub assigned_coverage: f64,
    pub transcription_success_rate: f64,
    pub transcript_count: usize,
    pub quality_flags: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SidecarRequest {
    action: String,
    meeting_id: String,
    model_cache_path: String,
    output_path: String,
    sources: Vec<SidecarSource>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SidecarSource {
    kind: String,
    path: String,
    forced_speaker_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SidecarInterval {
    speaker_id: String,
    source: String,
    start: f64,
    end: f64,
    confidence: f64,
    #[serde(default)]
    is_overlap: bool,
    #[serde(default)]
    candidate_speaker_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SidecarResponse {
    ok: bool,
    intervals: Vec<SidecarInterval>,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FinalWindow {
    source: String,
    start: f64,
    end: f64,
    speaker_id: Option<String>,
    speaker_profile_id: Option<String>,
    candidate_speaker_ids: Vec<String>,
    confidence: f64,
    is_overlap: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FinalTranscript {
    id: String,
    text: String,
    start: f64,
    end: f64,
    speaker_id: Option<String>,
    speaker_profile_id: Option<String>,
    speaker_label: String,
    source: String,
    is_overlap: bool,
    candidates: Vec<String>,
    confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub(crate) struct CoreTranscriptBackup {
    id: String,
    meeting_id: String,
    transcript: String,
    timestamp: String,
    summary: Option<String>,
    action_items: Option<String>,
    key_points: Option<String>,
    audio_start_time: Option<f64>,
    audio_end_time: Option<f64>,
    duration: Option<f64>,
    speaker: Option<String>,
}

pub fn cancel() {
    CANCEL_REQUESTED.store(true, Ordering::SeqCst);
}

pub fn fluid_model_cache<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf> {
    // The validated branch directory, so preparing or deleting these models
    // can never touch official Meetily storage.
    Ok(crate::storage_safety::branch_app_data_dir(app)
        .map_err(|error| anyhow!(error))?
        .join("diarization/fluid-audio-0.12.4"))
}

/// Whether post-call refinement can run here. It uses a bundled Swift helper
/// that needs an Apple Silicon Mac running macOS 14.4 or later; elsewhere the
/// live labels are kept.
pub fn refinement_supported() -> bool {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        macos_version_at_least(14, 4) && resolve_sidecar_binary().is_ok()
    }
    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    {
        false
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn macos_version_at_least(major: u32, minor: u32) -> bool {
    static VERSION: std::sync::OnceLock<Option<(u32, u32)>> = std::sync::OnceLock::new();
    let version = VERSION.get_or_init(|| {
        let output = std::process::Command::new("/usr/bin/sw_vers")
            .arg("-productVersion")
            .output()
            .ok()?;
        parse_macos_version(&String::from_utf8_lossy(&output.stdout))
    });
    version.is_some_and(|version| version >= (major, minor))
}

/// Parse `sw_vers -productVersion` output such as `14.4.1` or `15`.
#[cfg_attr(not(all(target_os = "macos", target_arch = "aarch64")), allow(dead_code))]
fn parse_macos_version(text: &str) -> Option<(u32, u32)> {
    let mut parts = text.trim().split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().map(str::parse).transpose().ok()?.unwrap_or(0);
    Some((major, minor))
}

pub fn offline_models_present<R: Runtime>(app: &AppHandle<R>) -> bool {
    fn contains_nonempty_model(path: &Path) -> bool {
        let Ok(entries) = std::fs::read_dir(path) else {
            return false;
        };
        entries.filter_map(Result::ok).any(|entry| {
            let path = entry.path();
            if path.is_dir() {
                contains_nonempty_model(&path)
            } else {
                path.metadata()
                    .map(|metadata| metadata.len() > 0)
                    .unwrap_or(false)
                    && path
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| {
                            matches!(extension, "mlmodelc" | "mlmodel" | "bin" | "npy")
                        })
            }
        })
    }
    fluid_model_cache(app)
        .map(|path| contains_nonempty_model(&path))
        .unwrap_or(false)
}

pub async fn prepare_offline_models<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    let cache = fluid_model_cache(app)?;
    let request = SidecarRequest {
        action: "prepare".to_string(),
        meeting_id: "model-preparation".to_string(),
        model_cache_path: cache.to_string_lossy().into_owned(),
        output_path: cache
            .join("prepare-result.json")
            .to_string_lossy()
            .into_owned(),
        sources: Vec::new(),
    };
    let response = invoke_sidecar(&request).await?;
    if response.ok {
        Ok(())
    } else {
        Err(anyhow!(
            "Offline model preparation failed: {}",
            response
                .error
                .unwrap_or_else(|| "unknown error".to_string())
        ))
    }
}

pub fn delete_offline_models<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    let cache = fluid_model_cache(app)?;
    if cache.exists() {
        std::fs::remove_dir_all(cache)?;
    }
    Ok(())
}

pub async fn run<R: Runtime>(
    app: AppHandle<R>,
    request: StartFinalDiarizationRequest,
) -> Result<FinalDiarizationResult> {
    let _processing = super::operations::try_processing_slot().map_err(|error| anyhow!(error))?;
    let _meeting =
        super::operations::try_meeting_edit(&request.meeting_id).map_err(|error| anyhow!(error))?;
    CANCEL_REQUESTED.store(false, Ordering::SeqCst);
    let state = app.state::<AppState>();
    let core_pool = state.db_manager.pool();
    let feature_pool = state.speaker_store.pool();
    let settings = crate::database::repositories::diarization::DiarizationRepository::get_settings(
        feature_pool,
    )
    .await?;
    if settings.enabled == 0 || settings.mode == "off" {
        return Err(anyhow!("Final speaker refinement is disabled"));
    }

    let manual_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM transcript_speakers WHERE meeting_id = ? AND diarization_method = 'manual'",
    )
    .bind(&request.meeting_id)
    .fetch_one(feature_pool)
    .await?;
    if manual_count > 0 && !request.overwrite_manual_edits {
        return Err(anyhow!(
            "This meeting has manual speaker edits. Confirm before replacing them."
        ));
    }

    let folder_path: Option<String> =
        sqlx::query_scalar("SELECT folder_path FROM meetings WHERE id = ?")
            .bind(&request.meeting_id)
            .fetch_optional(core_pool)
            .await?
            .flatten();
    let folder =
        PathBuf::from(folder_path.ok_or_else(|| anyhow!("This meeting has no retained audio"))?);
    let (sources, quality_flags) = discover_sources(&folder)?;
    let live_transcripts = load_transcripts(core_pool, &request.meeting_id).await?;
    let live_ids = live_transcripts
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    let live_speakers =
        crate::database::repositories::diarization::DiarizationRepository::get_transcript_metadata(
            feature_pool,
            &live_ids,
        )
        .await?;
    let live_backup = load_core_backup(core_pool, &request.meeting_id).await?;
    let backup = serde_json::to_string(&live_backup)?;
    let run_id = format!("diarization-run-{}", Uuid::new_v4());

    sqlx::query(
        "INSERT INTO diarization_runs (
            id, meeting_id, status, method, progress, quality_flags, transcript_backup,
            live_rows_hash
         ) VALUES (?, ?, 'prepared', 'fluid_audio_offline_0.12.4', 1, ?, ?, ?)",
    )
    .bind(&run_id)
    .bind(&request.meeting_id)
    .bind(serde_json::to_string(&quality_flags)?)
    .bind(&backup)
    .bind(hash_json(&backup))
    .execute(feature_pool)
    .await?;
    set_status(
        feature_pool,
        &request.meeting_id,
        &run_id,
        "processing",
        1,
        None,
        &quality_flags,
        None,
        None,
    )
    .await?;
    emit_progress(&app, &request.meeting_id, "preparing_models", 5, None);

    let run_result = run_inner(
        &app,
        &request.meeting_id,
        &run_id,
        &folder,
        sources,
        &live_transcripts,
        &live_speakers,
        quality_flags.clone(),
    )
    .await;

    match run_result {
        Ok(result) => Ok(result),
        Err(error) => {
            let cancelled = CANCEL_REQUESTED.load(Ordering::SeqCst);
            let status = if cancelled {
                "failed"
            } else {
                "fallback_to_live"
            };
            let display_status = if cancelled { "cancelled" } else { status };
            sqlx::query(
                "UPDATE diarization_runs SET status = ?, error = ?, completed_at = CURRENT_TIMESTAMP
                 WHERE id = ?",
            )
            .bind(status)
            .bind(error.to_string())
            .bind(&run_id)
            .execute(feature_pool)
            .await?;
            set_status(
                feature_pool,
                &request.meeting_id,
                &run_id,
                display_status,
                100,
                Some(&error.to_string()),
                &quality_flags,
                None,
                None,
            )
            .await?;
            emit_progress(
                &app,
                &request.meeting_id,
                display_status,
                100,
                Some(error.to_string()),
            );
            Err(error)
        }
    }
}

async fn run_inner<R: Runtime>(
    app: &AppHandle<R>,
    meeting_id: &str,
    run_id: &str,
    folder: &Path,
    sources: Vec<SidecarSource>,
    live_transcripts: &[Transcript],
    live_speakers: &[crate::database::repositories::diarization::TranscriptSpeakerMetadata],
    mut quality_flags: Vec<String>,
) -> Result<FinalDiarizationResult> {
    check_cancelled()?;
    let cache = fluid_model_cache(app)?;
    let diarization_dir = folder.join(".diarization");
    std::fs::create_dir_all(&diarization_dir)?;
    let output_path = diarization_dir.join("final-intervals.json");
    let sidecar_request = SidecarRequest {
        action: "process".to_string(),
        meeting_id: meeting_id.to_string(),
        model_cache_path: cache.to_string_lossy().into_owned(),
        output_path: output_path.to_string_lossy().into_owned(),
        sources,
    };
    let response = invoke_sidecar(&sidecar_request).await?;
    if !response.ok {
        return Err(anyhow!(
            "Offline diarization failed: {}",
            response
                .error
                .unwrap_or_else(|| "unknown sidecar error".to_string())
        ));
    }
    if response.intervals.is_empty() {
        return Err(anyhow!("Offline diarization found no speech"));
    }

    emit_progress(app, meeting_id, "rebuilding_windows", 45, None);
    let mut windows = rebuild_windows(&response.intervals);
    stabilize_speaker_ids(&mut windows, live_speakers);
    let assigned_coverage = assignment_coverage(&windows, live_transcripts);
    if windows.iter().any(|window| window.is_overlap) {
        quality_flags.push("overlap_detected".to_string());
    }

    check_cancelled()?;
    let audio = load_source_audio(&sidecar_request.sources).await?;
    let profile_labels = recognize_final_profiles(app, &mut windows, &audio).await;
    let labels = speaker_labels(&windows, live_speakers, &profile_labels);
    // Speaker identification must not rewrite, remove, or renumber transcript text.
    let transcripts = label_existing_transcripts(live_transcripts, &windows, &labels);
    let transcription_success_rate = 1.0; // All existing transcript rows are retained.
    if !passes_quality_gates(assigned_coverage, transcription_success_rate) {
        return Err(anyhow!(
            "Speaker analysis did not cover enough of the existing transcript"
        ));
    }
    check_cancelled()?;

    emit_progress(app, meeting_id, "saving", 95, None);
    publish_labels_atomically(
        app.state::<AppState>().db_manager.pool(),
        app.state::<AppState>().speaker_store.pool(),
        meeting_id,
        run_id,
        &windows,
        &transcripts,
        live_transcripts,
        &quality_flags,
        assigned_coverage,
        transcription_success_rate,
    )
    .await?;
    // Keep source stems for Enhance and for recovery when mixed audio is missing.
    emit_progress(app, meeting_id, "completed", 100, None);

    Ok(FinalDiarizationResult {
        run_id: run_id.to_string(),
        status: "completed".to_string(),
        assigned_coverage,
        transcription_success_rate,
        transcript_count: transcripts.len(),
        quality_flags,
    })
}

/// Speaker output staged before Enhance changes any saved transcript rows.
pub(crate) struct EnhancementSpeakerAnalysis {
    windows: Vec<FinalWindow>,
    labels: HashMap<String, String>,
    quality_flags: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct EnhancementSpeakerLabels {
    windows: Vec<FinalWindow>,
    transcripts: Vec<FinalTranscript>,
    quality_flags: Vec<String>,
    coverage: f64,
}

pub(crate) async fn prepare_enhancement<R: Runtime>(
    app: &AppHandle<R>,
    meeting_id: &str,
    folder: &Path,
    cancelled: &AtomicBool,
) -> Result<EnhancementSpeakerAnalysis> {
    check_cancelled_flag(cancelled)?;
    let state = app.state::<AppState>();
    let old_rows = load_transcripts(state.db_manager.pool(), meeting_id).await?;
    let ids = old_rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    let old_speakers =
        crate::database::repositories::diarization::DiarizationRepository::get_transcript_metadata(
            state.speaker_store.pool(),
            &ids,
        )
        .await?;
    let (sources, mut quality_flags) = discover_sources(folder)?;
    let directory = folder.join(".diarization");
    std::fs::create_dir_all(&directory)?;
    let request = SidecarRequest {
        action: "process".into(),
        meeting_id: meeting_id.into(),
        model_cache_path: fluid_model_cache(app)?.to_string_lossy().into_owned(),
        output_path: directory
            .join("final-intervals.json")
            .to_string_lossy()
            .into_owned(),
        sources,
    };
    check_cancelled_flag(cancelled)?;
    let response = invoke_sidecar_with_cancellation(&request, cancelled).await?;
    if !response.ok || response.intervals.is_empty() {
        return Err(anyhow!(
            "Speaker analysis failed: {}",
            response.error.unwrap_or_else(|| "no speech found".into())
        ));
    }
    let mut windows = rebuild_windows(&response.intervals);
    stabilize_speaker_ids(&mut windows, &old_speakers);
    if windows.iter().any(|window| window.is_overlap) {
        quality_flags.push("overlap_detected".into());
    }
    check_cancelled_flag(cancelled)?;
    let audio = load_source_audio(&request.sources).await?;
    check_cancelled_flag(cancelled)?;
    let names = recognize_final_profiles(app, &mut windows, &audio).await;
    check_cancelled_flag(cancelled)?;
    let labels = speaker_labels(&windows, &old_speakers, &names);
    Ok(EnhancementSpeakerAnalysis {
        windows,
        labels,
        quality_flags,
    })
}

impl EnhancementSpeakerAnalysis {
    pub(crate) fn speaker_key(&self, start: f64, end: f64) -> Option<String> {
        self.windows
            .iter()
            .filter_map(|window| {
                let overlap = (end.min(window.end) - start.max(window.start)).max(0.0);
                (overlap > 0.0).then(|| {
                    (
                        overlap,
                        if window.is_overlap {
                            format!("overlap:{}", window.candidate_speaker_ids.join(","))
                        } else {
                            window.speaker_id.clone().unwrap_or_default()
                        },
                    )
                })
            })
            .max_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)))
            .map(|(_, key)| key)
    }

    pub(crate) fn turn_boundaries(&self) -> Vec<f64> {
        // Windows from different tracks can cover the same audio time. Only
        // compare consecutive windows within one track, and do not infer a
        // turn across an ambiguous same-source overlap window.
        let mut by_source: BTreeMap<&str, Vec<&FinalWindow>> = BTreeMap::new();
        for window in &self.windows {
            by_source.entry(&window.source).or_default().push(window);
        }
        let mut cuts = Vec::new();
        for windows in by_source.values_mut() {
            windows.sort_by(|a, b| a.start.total_cmp(&b.start));
            for pair in windows.windows(2) {
                if let (Some(before), Some(after)) = (&pair[0].speaker_id, &pair[1].speaker_id) {
                    if before != after && pair[0].end <= pair[1].start {
                        cuts.push((pair[0].end + pair[1].start) / 2.0);
                    }
                }
            }
        }
        cuts.sort_by(f64::total_cmp);
        cuts.dedup_by(|a, b| (*a - *b).abs() < 0.000001);
        cuts
    }

    pub(crate) fn label(self, rows: &[Transcript]) -> Result<EnhancementSpeakerLabels> {
        let coverage = assignment_coverage(&self.windows, rows);
        if !passes_quality_gates(coverage, 1.0) {
            return Err(anyhow!("Speaker analysis covered {:.1}% of the new transcript; at least 75% is required. The saved transcript was kept.", coverage * 100.0));
        }
        let transcripts = label_existing_transcripts(rows, &self.windows, &self.labels);
        Ok(EnhancementSpeakerLabels {
            windows: self.windows,
            transcripts,
            quality_flags: self.quality_flags,
            coverage,
        })
    }
}

impl EnhancementSpeakerLabels {
    pub(crate) async fn write(
        &self,
        connection: &mut sqlx::SqliteConnection,
        meeting_id: &str,
    ) -> Result<()> {
        let run_id = format!("diarization-run-{}", Uuid::new_v4());
        sqlx::query("INSERT INTO main.diarization_runs (id, meeting_id, status, method, progress)
            VALUES (?, ?, 'prepared', 'fluid_audio_offline_0.12.4', 95)")
            .bind(&run_id).bind(meeting_id).execute(&mut *connection).await?;
        write_speaker_rows(
            connection,
            "main",
            meeting_id,
            &run_id,
            &self.windows,
            &self.transcripts,
        )
        .await?;
        finish_speaker_run(
            connection,
            "main",
            meeting_id,
            &run_id,
            &self.quality_flags,
            self.coverage,
            1.0,
        )
        .await
    }
}

fn label_existing_transcripts(
    live: &[Transcript],
    windows: &[FinalWindow],
    labels: &HashMap<String, String>,
) -> Vec<FinalTranscript> {
    live.iter()
        .map(|row| {
            let start = row.audio_start_time.unwrap_or(0.0);
            let end = row.audio_end_time.unwrap_or(start);
            let duration = (end - start).max(0.0);
            let matching = windows
                .iter()
                .filter(|window| window.end > start && window.start < end)
                .collect::<Vec<_>>();
            let mut speakers: HashMap<&str, (f64, &FinalWindow)> = HashMap::new();
            let mut overlap_ranges = Vec::new();
            let mut candidates = BTreeSet::new();
            for window in &matching {
                let overlap = (end.min(window.end) - start.max(window.start)).max(0.0);
                if window.is_overlap {
                    overlap_ranges.push((start.max(window.start), end.min(window.end)));
                    candidates.extend(window.candidate_speaker_ids.iter().cloned());
                }
                if let Some(id) = window.speaker_id.as_deref() {
                    speakers.entry(id).or_insert((0.0, window)).0 += overlap;
                }
            }
            overlap_ranges.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut overlap_seconds = 0.0;
            let mut cursor = start;
            for (a, b) in overlap_ranges {
                overlap_seconds += (b - cursor.max(a)).max(0.0);
                cursor = cursor.max(b);
            }
            let is_overlap = duration > 0.0 && overlap_seconds / duration >= 0.5;
            let best = speakers
                .into_iter()
                .max_by(|a, b| a.1 .0.total_cmp(&b.1 .0).then_with(|| b.0.cmp(a.0)))
                .filter(|(_, (seconds, _))| duration > 0.0 && seconds / duration >= 0.5);
            let window = best.as_ref().map(|(_, (_, window))| *window);
            let speaker_id = if is_overlap {
                None
            } else {
                best.as_ref().map(|(id, _)| (*id).to_string())
            };
            let speaker_label = if is_overlap {
                "Multiple speakers".to_string()
            } else {
                speaker_id
                    .as_ref()
                    .and_then(|id| labels.get(id))
                    .cloned()
                    .unwrap_or_else(|| "Unknown speaker".to_string())
            };
            let clean_for_enrollment = duration > 0.0
                && !matching.is_empty()
                && matching.iter().all(|window| {
                    !window.is_overlap && window.speaker_id.as_ref() == speaker_id.as_ref()
                })
                && best
                    .as_ref()
                    .map(|(_, (seconds, _))| seconds / duration >= 0.95)
                    .unwrap_or(false);
            let confidence = if clean_for_enrollment {
                matching
                    .iter()
                    .map(|window| window.confidence)
                    .fold(1.0, f64::min)
            } else {
                0.0
            };
            FinalTranscript {
                id: row.id.clone(),
                text: row.transcript.clone(),
                start,
                end,
                speaker_id,
                speaker_profile_id: if is_overlap {
                    None
                } else {
                    window.and_then(|w| w.speaker_profile_id.clone())
                },
                speaker_label,
                source: window
                    .map(|w| w.source.clone())
                    .unwrap_or_else(|| "mixed".to_string()),
                is_overlap,
                candidates: if is_overlap {
                    candidates.into_iter().collect()
                } else {
                    Vec::new()
                },
                confidence,
            }
        })
        .collect()
}

fn passes_quality_gates(assigned_coverage: f64, transcription_success_rate: f64) -> bool {
    assigned_coverage >= 0.75 && transcription_success_rate >= 0.95
}

fn discover_sources(folder: &Path) -> Result<(Vec<SidecarSource>, Vec<String>)> {
    let mic = folder.join(".diarization/microphone.flac");
    let system = folder.join(".diarization/system.flac");
    if mic.exists() && system.exists() {
        return Ok((
            vec![
                SidecarSource {
                    kind: "microphone".to_string(),
                    path: mic.to_string_lossy().into_owned(),
                    // A microphone can capture a room. The presence of a
                    // system stem (which can contain only silence) does not
                    // establish that exactly one person used the microphone.
                    forced_speaker_id: None,
                },
                SidecarSource {
                    kind: "system".to_string(),
                    path: system.to_string_lossy().into_owned(),
                    forced_speaker_id: None,
                },
            ],
            Vec::new(),
        ));
    }
    if mic.exists() {
        return Ok((
            vec![SidecarSource {
                kind: "microphone".to_string(),
                path: mic.to_string_lossy().into_owned(),
                forced_speaker_id: None,
            }],
            vec!["single_source".to_string()],
        ));
    }
    let mixed = find_meeting_audio(folder)?;
    Ok((
        vec![SidecarSource {
            kind: "mixed".to_string(),
            path: mixed.to_string_lossy().into_owned(),
            forced_speaker_id: None,
        }],
        vec!["mixed_audio".to_string()],
    ))
}

pub(super) fn find_meeting_audio(folder: &Path) -> Result<PathBuf> {
    let preferred = folder.join("audio.mp4");
    if preferred.exists() {
        return Ok(preferred);
    }
    for entry in std::fs::read_dir(folder)? {
        let path = entry?.path();
        let supported = path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| {
                crate::audio::constants::AUDIO_EXTENSIONS
                    .contains(&extension.to_ascii_lowercase().as_str())
            })
            .unwrap_or(false);
        if path.is_file() && supported {
            return Ok(path);
        }
    }
    Err(anyhow!("This meeting has no retained audio"))
}

async fn invoke_sidecar(request: &SidecarRequest) -> Result<SidecarResponse> {
    invoke_sidecar_with_cancellation(request, &CANCEL_REQUESTED).await
}

async fn invoke_sidecar_with_cancellation(
    request: &SidecarRequest,
    cancelled: &AtomicBool,
) -> Result<SidecarResponse> {
    if request.action == "process" { check_cancelled_flag(cancelled)?; }
    let binary = resolve_sidecar_binary()?;
    invoke_sidecar_process(&binary, request, cancelled).await
}

async fn invoke_sidecar_process(
    binary: &Path,
    request: &SidecarRequest,
    cancelled: &AtomicBool,
) -> Result<SidecarResponse> {
    if request.action == "process" { check_cancelled_flag(cancelled)?; }
    let mut child = Command::new(binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("Unable to launch {}", binary.display()))?;
    let payload = serde_json::to_vec(request)?;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("Sidecar stdin unavailable"))?
        .write_all(&payload)
        .await?;
    let output = if request.action == "process" {
        tokio::select! {
            output = child.wait_with_output() => output?,
            _ = async {
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    if cancelled.load(Ordering::SeqCst) { break; }
                }
            } => return Err(anyhow!("Final speaker refinement was cancelled")),
        }
    } else {
        child.wait_with_output().await?
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json = stdout
        .lines()
        .rev()
        .find(|line| line.trim_start().starts_with('{'))
        .ok_or_else(|| {
            anyhow!(
                "Sidecar returned no JSON: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        })?;
    let response: SidecarResponse = serde_json::from_str(json)?;
    if !output.status.success() && response.ok {
        return Err(anyhow!("Sidecar exited unexpectedly"));
    }
    Ok(response)
}

fn resolve_sidecar_binary() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("MEETILY_DIARIZATION_SIDECAR") {
        let path = PathBuf::from(path);
        if path.exists() {
            return Ok(path);
        }
    }
    let mut candidates = Vec::new();
    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            candidates.push(directory.join("meetily-diarization"));
            candidates.push(directory.join("meetily-diarization-aarch64-apple-darwin"));
        }
    }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("binaries/meetily-diarization-aarch64-apple-darwin"),
    );
    candidates
        .into_iter()
        .find(|path| path.exists())
        .ok_or_else(|| anyhow!("Bundled diarization sidecar is missing"))
}

fn rebuild_windows(intervals: &[SidecarInterval]) -> Vec<FinalWindow> {
    let valid = intervals
        .iter()
        .filter(|interval| {
            interval.start.is_finite() && interval.end.is_finite() && interval.end > interval.start
        })
        .collect::<Vec<_>>();
    let mut boundaries = valid
        .iter()
        .flat_map(|interval| [interval.start, interval.end])
        .collect::<Vec<_>>();
    boundaries.sort_by(f64::total_cmp);
    boundaries.dedup_by(|a, b| (*a - *b).abs() < 0.000001);
    let mut windows = Vec::new();
    for pair in boundaries.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let midpoint = (start + end) / 2.0;
        let active = valid
            .iter()
            .copied()
            .filter(|interval| interval.start <= midpoint && interval.end > midpoint)
            .collect::<Vec<_>>();
        let all_speakers = active
            .iter()
            .map(|interval| interval.speaker_id.clone())
            .collect::<BTreeSet<_>>();
        let mut by_source: BTreeMap<&str, Vec<&SidecarInterval>> = BTreeMap::new();
        for interval in &active {
            by_source
                .entry(interval.source.as_str())
                .or_default()
                .push(*interval);
        }
        for (source, source_intervals) in by_source {
            let candidates = source_intervals
                .iter()
                .map(|interval| interval.speaker_id.clone())
                .collect::<BTreeSet<_>>();
            windows.push(FinalWindow {
                source: source.to_string(),
                start,
                end,
                speaker_id: if candidates.len() == 1 {
                    candidates.first().cloned()
                } else {
                    None
                },
                speaker_profile_id: None,
                candidate_speaker_ids: all_speakers.iter().cloned().collect(),
                confidence: source_intervals
                    .iter()
                    .map(|interval| interval.confidence)
                    .sum::<f64>()
                    / source_intervals.len() as f64,
                is_overlap: all_speakers.len() > 1,
            });
        }
    }
    windows.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.source.cmp(&b.source)));
    merge_adjacent_windows(windows)
}

fn merge_adjacent_windows(windows: Vec<FinalWindow>) -> Vec<FinalWindow> {
    let mut output: Vec<FinalWindow> = Vec::new();
    for window in windows {
        if let Some(last) = output.last_mut() {
            if last.source == window.source
                && last.speaker_id == window.speaker_id
                && last.speaker_profile_id == window.speaker_profile_id
                && last.candidate_speaker_ids == window.candidate_speaker_ids
                && last.is_overlap == window.is_overlap
                && window.start - last.end <= 0.3
            {
                last.end = last.end.max(window.end);
                last.confidence = last.confidence.max(window.confidence);
                continue;
            }
        }
        output.push(window);
    }
    output
}

fn stabilize_speaker_ids(
    windows: &mut [FinalWindow],
    live: &[crate::database::repositories::diarization::TranscriptSpeakerMetadata],
) {
    // Reuse each old ID at most once. A previous run can have merged several
    // voices; mapping every new voice to its best old row repeats that merge.
    let mut weights: HashMap<(String, String), f64> = HashMap::new();
    let final_ids: BTreeSet<String> = windows
        .iter()
        .flat_map(|window| {
            window
                .speaker_id
                .iter()
                .chain(&window.candidate_speaker_ids)
        })
        .cloned()
        .collect();
    let mut reserved = final_ids.clone();
    reserved.extend(live.iter().filter_map(|row| row.speaker_id.clone()));
    for window in windows.iter().filter(|window| !window.is_overlap) {
        let Some(final_id) = &window.speaker_id else {
            continue;
        };
        if final_id == "local-you" {
            continue;
        }
        for transcript in live.iter().filter(|row| row.is_overlap == 0) {
            let Some(provisional_id) = transcript.speaker_id.as_ref() else {
                continue;
            };
            if matches!(
                provisional_id.as_str(),
                "local-you" | "multiple-speakers" | "unknown-speaker"
            ) {
                continue;
            }
            let start = transcript.audio_start_time.unwrap_or(0.0).max(window.start);
            let end = transcript.audio_end_time.unwrap_or(start).min(window.end);
            let overlap = (end - start).max(0.0);
            if overlap > 0.0 {
                *weights
                    .entry((final_id.clone(), provisional_id.clone()))
                    .or_default() += overlap;
            }
        }
    }
    // Use the strongest total time match first, with a stable tie break.
    let mut ranked = weights.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut mapping = HashMap::new();
    let mut used = BTreeSet::from(["local-you".to_string()]);
    for ((final_id, old_id), _) in ranked {
        if !mapping.contains_key(&final_id) && used.insert(old_id.clone()) {
            mapping.insert(final_id, old_id);
        }
    }
    // Unmatched voices need distinct IDs too: their raw ID can already be the
    // target of another match or belong to an old, incorrectly merged voice.
    let old_ids: BTreeSet<_> = live
        .iter()
        .filter_map(|row| row.speaker_id.as_ref())
        .collect();
    let mut next = 1;
    for id in final_ids {
        if id == "local-you" || mapping.contains_key(&id) {
            continue;
        }
        let target = if !used.contains(&id) && !old_ids.contains(&id) {
            id.clone()
        } else {
            loop {
                let candidate = format!("final-speaker-{next}");
                next += 1;
                if reserved.insert(candidate.clone()) {
                    break candidate;
                }
            }
        };
        used.insert(target.clone());
        mapping.insert(id, target);
    }
    for window in windows {
        if let Some(id) = &window.speaker_id {
            if let Some(stable) = mapping.get(id) {
                window.speaker_id = Some(stable.clone());
            }
        }
        for candidate in &mut window.candidate_speaker_ids {
            if let Some(stable) = mapping.get(candidate) {
                *candidate = stable.clone();
            }
        }
    }
}

fn speaker_labels(
    windows: &[FinalWindow],
    live: &[crate::database::repositories::diarization::TranscriptSpeakerMetadata],
    profile_labels: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut labels = HashMap::new();
    labels.insert("local-you".to_string(), "You".to_string());
    let mut manual_labels = HashMap::new();
    for transcript in live {
        if let (Some(id), Some(label)) = (&transcript.speaker_id, &transcript.speaker_label) {
            if !label.trim().is_empty()
                && !matches!(label.as_str(), "Multiple speakers" | "Unknown speaker")
            {
                labels.insert(id.clone(), label.clone());
                if transcript.diarization_method.as_deref() == Some("manual") {
                    manual_labels.insert(id.clone(), label.clone());
                }
            }
        }
    }
    // A strong post-call match replaces automatic live labels. A name set by
    // the user takes precedence over either automatic pass.
    labels.extend(profile_labels.clone());
    labels.extend(manual_labels);
    let mut used_labels: BTreeSet<String> = labels.values().cloned().collect();
    let mut next = 1;
    for window in windows {
        if let Some(id) = &window.speaker_id {
            labels.entry(id.clone()).or_insert_with(|| loop {
                let label = format!("Speaker {next}");
                next += 1;
                if used_labels.insert(label.clone()) {
                    break label;
                }
            });
        }
    }
    labels
}

async fn recognize_final_profiles<R: Runtime>(
    app: &AppHandle<R>,
    windows: &mut [FinalWindow],
    audio: &HashMap<String, Vec<f32>>,
) -> HashMap<String, String> {
    let state = app.state::<AppState>();
    let profiles = crate::database::repositories::speaker_profile::SpeakerProfilesRepository::recognition_profiles(
        state.speaker_store.pool(), crate::diarization::models::EMBEDDING_MODEL_VERSION,
    ).await.unwrap_or_default();
    if profiles.is_empty() {
        return HashMap::new();
    }
    let Ok(model_path) = crate::diarization::models::embedding_model_path(app) else {
        return HashMap::new();
    };
    if crate::diarization::models::validate_model_file(&model_path).is_err() {
        return HashMap::new();
    }
    let Ok(mut extractor) = crate::diarization::embedding::EmbeddingExtractor::new(&model_path)
    else {
        return HashMap::new();
    };

    let speaker_ids: BTreeSet<String> = windows
        .iter()
        .filter(|window| !window.is_overlap)
        .filter_map(|window| window.speaker_id.clone())
        .filter(|speaker_id| speaker_id != "local-you")
        .collect();
    let mut names = HashMap::new();
    let mut matches_by_speaker = HashMap::new();
    for speaker_id in speaker_ids {
        let mut samples = Vec::new();
        for window in windows.iter().filter(|window| {
            !window.is_overlap && window.speaker_id.as_deref() == Some(speaker_id.as_str())
        }) {
            if let Some(window_samples) = slice_window(audio, window) {
                samples.extend(window_samples);
                if samples.len() >= 30 * 16_000 {
                    break;
                }
            }
        }
        if samples.len() < 16_000 {
            continue;
        }
        let Ok(embedding) = extractor.compute(&samples) else {
            continue;
        };
        if let Some((best, _)) = super::profile_matching::best_profile(&embedding, &profiles, None)
        {
            names.insert(speaker_id.clone(), profiles[best].name.clone());
            matches_by_speaker.insert(speaker_id, profiles[best].id.clone());
        }
    }
    for window in windows {
        if let Some(id) = &window.speaker_id {
            if let Some(profile_id) = matches_by_speaker.get(id) {
                window.speaker_profile_id = Some(profile_id.clone());
            }
        }
    }
    names
}

async fn load_source_audio(sources: &[SidecarSource]) -> Result<HashMap<String, Vec<f32>>> {
    let mut audio = HashMap::new();
    for source in sources {
        let path = PathBuf::from(&source.path);
        let decoded = tokio::task::spawn_blocking(move || decode_audio_file(&path)).await??;
        audio.insert(source.kind.clone(), decoded.to_whisper_format());
    }
    Ok(audio)
}

fn slice_window(audio: &HashMap<String, Vec<f32>>, window: &FinalWindow) -> Option<Vec<f32>> {
    let samples = audio.get(&window.source)?;
    let start = (window.start.max(0.0) * 16_000.0) as usize;
    let end = (window.end.max(window.start) * 16_000.0) as usize;
    if end <= start || start >= samples.len() {
        return None;
    }
    Some(samples[start..end.min(samples.len())].to_vec())
}

fn assignment_coverage(windows: &[FinalWindow], live: &[Transcript]) -> f64 {
    if live.is_empty() {
        return (!windows.is_empty()) as u8 as f64;
    }
    let voiced: f64 = live
        .iter()
        .map(|transcript| {
            transcript.duration.unwrap_or_else(|| {
                transcript.audio_end_time.unwrap_or(0.0)
                    - transcript.audio_start_time.unwrap_or(0.0)
            })
        })
        .map(|duration| duration.max(0.0))
        .sum();
    if voiced <= 0.0 {
        return 0.0;
    }
    let covered: f64 = live
        .iter()
        .map(|transcript| {
            let start = transcript.audio_start_time.unwrap_or(0.0);
            let end = transcript.audio_end_time.unwrap_or(start);
            let mut ranges = windows
                .iter()
                .map(|window| (start.max(window.start), end.min(window.end)))
                .filter(|(start, end)| end > start)
                .collect::<Vec<_>>();
            ranges.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut covered = 0.0;
            let mut cursor = start;
            for (range_start, range_end) in ranges {
                covered += (range_end - cursor.max(range_start)).max(0.0);
                cursor = cursor.max(range_end);
            }
            covered
        })
        .sum();
    (covered / voiced).clamp(0.0, 1.0)
}

pub(crate) async fn load_transcripts(
    pool: &sqlx::SqlitePool,
    meeting_id: &str,
) -> Result<Vec<Transcript>> {
    Ok(sqlx::query_as::<_, Transcript>(
        "SELECT id, meeting_id, transcript, timestamp, summary, action_items, key_points,
                audio_start_time, audio_end_time, duration
         FROM transcripts WHERE meeting_id = ? ORDER BY audio_start_time, id",
    )
    .bind(meeting_id)
    .fetch_all(pool)
    .await?)
}

async fn load_core_backup(
    pool: &sqlx::SqlitePool,
    meeting_id: &str,
) -> Result<Vec<CoreTranscriptBackup>> {
    Ok(sqlx::query_as::<_, CoreTranscriptBackup>(
        "SELECT id, meeting_id, transcript, timestamp, summary, action_items, key_points,
                audio_start_time, audio_end_time, duration, speaker
         FROM transcripts WHERE meeting_id = ? ORDER BY audio_start_time, id",
    )
    .bind(meeting_id)
    .fetch_all(pool)
    .await?)
}

#[allow(clippy::too_many_arguments)]
async fn publish_labels_atomically(
    core_pool: &sqlx::SqlitePool,
    feature_pool: &sqlx::SqlitePool,
    meeting_id: &str,
    run_id: &str,
    windows: &[FinalWindow],
    transcripts: &[FinalTranscript],
    original_transcripts: &[Transcript],
    quality_flags: &[String],
    coverage: f64,
    success_rate: f64,
) -> Result<()> {
    // Abort if another transcript operation changed the meeting while analysis ran.
    // This function never writes to the core transcript database.
    let current = load_transcripts(core_pool, meeting_id).await?;
    if serde_json::to_string(&current)? != serde_json::to_string(original_transcripts)? {
        return Err(anyhow!(
            "The transcript changed during speaker analysis. Run speaker identification again."
        ));
    }
    let mut feature_connection = feature_pool.acquire().await?;
    let mut feature_transaction = feature_connection.begin().await?;
    write_speaker_rows(
        &mut feature_transaction,
        "main",
        meeting_id,
        run_id,
        windows,
        transcripts,
    )
    .await?;
    finish_speaker_run(
        &mut feature_transaction,
        "main",
        meeting_id,
        run_id,
        quality_flags,
        coverage,
        success_rate,
    )
    .await?;
    feature_transaction.commit().await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn write_speaker_rows(
    connection: &mut sqlx::SqliteConnection,
    schema: &str,
    meeting_id: &str,
    run_id: &str,
    windows: &[FinalWindow],
    transcripts: &[FinalTranscript],
) -> Result<()> {
    assert!(matches!(schema, "main" | "speaker_feature"));
    sqlx::query(&format!(
        "DELETE FROM {schema}.speaker_segments WHERE meeting_id = ?"
    ))
    .bind(meeting_id)
    .execute(&mut *connection)
    .await?;
    for window in windows {
        sqlx::query(&format!(
            "INSERT INTO {schema}.speaker_segments (
                id, meeting_id, source, start_time, end_time, speaker_id, speaker_label,
                speaker_profile_id, confidence, is_overlap, diarization_status,
                diarization_method, candidate_speaker_ids
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'final',
                       'fluid_audio_offline_0.12.4', ?)"
        ))
        .bind(format!("speaker-segment-{}", Uuid::new_v4()))
        .bind(meeting_id)
        .bind(&window.source)
        .bind(window.start)
        .bind(window.end)
        .bind(&window.speaker_id)
        .bind(if window.speaker_id.is_none() {
            Some("Multiple speakers")
        } else {
            None::<&str>
        })
        .bind(&window.speaker_profile_id)
        .bind(window.confidence)
        .bind(window.is_overlap as i64)
        .bind(serde_json::to_string(&window.candidate_speaker_ids)?)
        .execute(&mut *connection)
        .await?;
    }
    sqlx::query(&format!(
        "DELETE FROM {schema}.transcript_speakers WHERE meeting_id = ?"
    ))
    .bind(meeting_id)
    .execute(&mut *connection)
    .await?;
    for transcript in transcripts {
        sqlx::query(&format!(
            "INSERT INTO {schema}.transcript_speakers (
                transcript_id, meeting_id, audio_start_time, audio_end_time,
                speaker_id, speaker_label, speaker_profile_id, speaker_source,
                speaker_color, is_overlap, candidate_speaker_ids,
                diarization_status, diarization_method, diarization_confidence,
                run_id, published
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'final',
                       'fluid_audio_offline_0.12.4', ?, ?, 1)"
        ))
        .bind(&transcript.id)
        .bind(meeting_id)
        .bind(transcript.start)
        .bind(transcript.end)
        .bind(&transcript.speaker_id)
        .bind(&transcript.speaker_label)
        .bind(&transcript.speaker_profile_id)
        .bind(&transcript.source)
        .bind(if transcript.is_overlap {
            "#f97316"
        } else {
            "#3b82f6"
        })
        .bind(transcript.is_overlap as i64)
        .bind(serde_json::to_string(&transcript.candidates)?)
        .bind(transcript.confidence)
        .bind(run_id)
        .execute(&mut *connection)
        .await?;
    }
    Ok(())
}

async fn finish_speaker_run(
    connection: &mut sqlx::SqliteConnection,
    schema: &str,
    meeting_id: &str,
    run_id: &str,
    quality_flags: &[String],
    coverage: f64,
    success_rate: f64,
) -> Result<()> {
    assert!(matches!(schema, "main" | "speaker_feature"));
    sqlx::query(&format!(
        "UPDATE {schema}.diarization_runs SET status = 'applied', progress = 100,
            assigned_coverage = ?, transcription_success_rate = ?, completed_at = CURRENT_TIMESTAMP
         WHERE id = ?"
    ))
    .bind(coverage)
    .bind(success_rate)
    .bind(run_id)
    .execute(&mut *connection)
    .await?;
    sqlx::query(&format!(
        "INSERT INTO {schema}.meeting_diarization_status (
            meeting_id, enabled, live_enabled, post_call_enabled, current_status,
            quality_flags, run_id, progress, method, assigned_coverage,
            transcription_success_rate, processed_at, updated_at
         ) VALUES (?, 1, 1, 1, 'completed', ?, ?, 100, 'fluid_audio_offline_0.12.4',
                   ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
         ON CONFLICT(meeting_id) DO UPDATE SET
            current_status = 'completed', quality_flags = excluded.quality_flags,
            run_id = excluded.run_id, progress = 100, method = excluded.method,
            failure_reason = NULL, assigned_coverage = excluded.assigned_coverage,
            transcription_success_rate = excluded.transcription_success_rate,
            processed_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP"
    ))
    .bind(meeting_id)
    .bind(serde_json::to_string(quality_flags)?)
    .bind(run_id)
    .bind(coverage)
    .bind(success_rate)
    .execute(&mut *connection)
    .await?;
    Ok(())
}

async fn set_status(
    pool: &sqlx::SqlitePool,
    meeting_id: &str,
    run_id: &str,
    status: &str,
    progress: i64,
    failure_reason: Option<&str>,
    quality_flags: &[String],
    coverage: Option<f64>,
    success_rate: Option<f64>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO meeting_diarization_status (
            meeting_id, enabled, live_enabled, post_call_enabled, current_status,
            quality_flags, run_id, progress, method, failure_reason,
            assigned_coverage, transcription_success_rate, updated_at
         ) VALUES (?, 1, 1, 1, ?, ?, ?, ?, 'fluid_audio_offline_0.12.4', ?, ?, ?, CURRENT_TIMESTAMP)
         ON CONFLICT(meeting_id) DO UPDATE SET
            current_status = excluded.current_status, quality_flags = excluded.quality_flags,
            run_id = excluded.run_id, progress = excluded.progress, method = excluded.method,
            failure_reason = excluded.failure_reason,
            assigned_coverage = excluded.assigned_coverage,
            transcription_success_rate = excluded.transcription_success_rate,
            updated_at = CURRENT_TIMESTAMP",
    )
    .bind(meeting_id)
    .bind(status)
    .bind(serde_json::to_string(quality_flags)?)
    .bind(run_id)
    .bind(progress)
    .bind(failure_reason)
    .bind(coverage)
    .bind(success_rate)
    .execute(pool)
    .await?;
    Ok(())
}

fn emit_progress<R: Runtime>(
    app: &AppHandle<R>,
    meeting_id: &str,
    status: &str,
    progress: u32,
    error: Option<String>,
) {
    let _ = app.emit(
        "diarization-progress",
        serde_json::json!({
            "meetingId": meeting_id,
            "status": status,
            "progress": progress,
            "error": error,
        }),
    );
}

fn check_cancelled() -> Result<()> {
    check_cancelled_flag(&CANCEL_REQUESTED)
}

fn check_cancelled_flag(cancelled: &AtomicBool) -> Result<()> {
    if cancelled.load(Ordering::SeqCst) {
        Err(anyhow!("Final speaker refinement was cancelled"))
    } else {
        Ok(())
    }
}

fn format_timestamp(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    format!(
        "{:02}:{:02}:{:02}",
        total / 3600,
        (total % 3600) / 60,
        total % 60
    )
}

fn hash_json(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

#[derive(sqlx::FromRow)]
struct InterruptedRun {
    id: String,
    meeting_id: String,
    transcript_backup: String,
    proposed_transcripts: String,
    proposed_speaker_metadata: String,
    live_rows_hash: String,
    final_rows_hash: String,
    quality_flags: Option<String>,
    assigned_coverage: Option<f64>,
    transcription_success_rate: Option<f64>,
}

/// Reconcile the only cross-database transition before transcript UI data is
/// served. The core database is compared with both staged hashes; no guess is
/// made when neither hash matches.
pub async fn reconcile_interrupted_runs(
    core_pool: &sqlx::SqlitePool,
    feature_pool: &sqlx::SqlitePool,
) -> Result<()> {
    crate::audio::enhancement::reconcile_publications(core_pool, feature_pool).await?;
    let prepared: Vec<(String, String)> =
        sqlx::query_as("SELECT id, meeting_id FROM diarization_runs WHERE status = 'prepared'")
            .fetch_all(feature_pool)
            .await?;
    for (run_id, meeting_id) in prepared {
        if !is_current_recovery_run(feature_pool, &meeting_id, &run_id).await? {
            mark_stale_run(feature_pool, &run_id).await?;
            continue;
        }
        sqlx::query(
            "UPDATE diarization_runs SET status = 'fallback_to_live',
                error = 'Interrupted before transcript apply',
                completed_at = CURRENT_TIMESTAMP WHERE id = ?",
        )
        .bind(&run_id)
        .execute(feature_pool)
        .await?;
        set_status(
            feature_pool,
            &meeting_id,
            &run_id,
            "fallback_to_live",
            100,
            Some("Final speaker refinement was interrupted; the live transcript was retained."),
            &[],
            None,
            None,
        )
        .await?;
    }

    let runs = sqlx::query_as::<_, InterruptedRun>(
        "SELECT id, meeting_id, transcript_backup, proposed_transcripts,
                proposed_speaker_metadata, live_rows_hash, final_rows_hash,
                quality_flags, assigned_coverage, transcription_success_rate
         FROM diarization_runs
         WHERE status IN ('applying', 'recovery_required', 'fallback_to_live', 'failed')
           AND transcript_backup IS NOT NULL
           AND proposed_transcripts IS NOT NULL
           AND proposed_speaker_metadata IS NOT NULL
           AND live_rows_hash IS NOT NULL
           AND final_rows_hash IS NOT NULL",
    )
    .fetch_all(feature_pool)
    .await?;

    for run in runs {
        if !is_current_recovery_run(feature_pool, &run.meeting_id, &run.id).await? {
            mark_stale_run(feature_pool, &run.id).await?;
            continue;
        }
        let actual = load_core_backup(core_pool, &run.meeting_id).await?;
        let actual_hash = hash_json(&serde_json::to_string(&actual)?);
        if actual_hash == run.final_rows_hash {
            let manual_count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM transcript_speakers WHERE meeting_id = ? AND published = 1 AND diarization_method = 'manual'",
            ).bind(&run.meeting_id).fetch_one(feature_pool).await?;
            if manual_count > 0 {
                mark_stale_run(feature_pool, &run.id).await?;
                set_status(feature_pool, &run.meeting_id, &run.id, "needs_review", 100,
                    Some("Manual speaker corrections were retained. Run speaker identification again only if you want to replace them."),
                    &[], None, None).await?;
                continue;
            }
            let transcripts: Vec<FinalTranscript> =
                serde_json::from_str(&run.proposed_transcripts)?;
            let windows: Vec<FinalWindow> = serde_json::from_str(&run.proposed_speaker_metadata)?;
            publish_recovered_metadata(feature_pool, &run, &windows, &transcripts).await?;
        } else if actual_hash == run.live_rows_hash {
            sqlx::query(
                "UPDATE diarization_runs SET status = 'fallback_to_live',
                    error = 'Interrupted before core transcript replacement',
                    final_rows_hash = NULL, completed_at = CURRENT_TIMESTAMP WHERE id = ?",
            )
            .bind(&run.id)
            .execute(feature_pool)
            .await?;
            set_status(
                feature_pool,
                &run.meeting_id,
                &run.id,
                "fallback_to_live",
                100,
                Some("Final speaker refinement was interrupted; the live transcript was retained."),
                &quality_flags_from_json(run.quality_flags.as_deref()),
                None,
                None,
            )
            .await?;
        } else {
            // A later edit or retranscription can legitimately match neither old hash.
            // Keep current text and retain the backup for explicit recovery.
            sqlx::query(
                "UPDATE diarization_runs SET status = 'needs_review',
                    error = 'Current transcript differs from both staged versions; current text retained',
                    completed_at = CURRENT_TIMESTAMP WHERE id = ?",
            )
            .bind(&run.id)
            .execute(feature_pool)
            .await?;
            set_status(
                feature_pool, &run.meeting_id, &run.id, "needs_review", 100,
                Some("An older speaker update does not match the current transcript. Current text was retained; identify speakers again to update labels."),
                &quality_flags_from_json(run.quality_flags.as_deref()), None, None,
            ).await?;
        }
    }
    Ok(())
}

async fn is_current_recovery_run(
    pool: &sqlx::SqlitePool,
    meeting_id: &str,
    run_id: &str,
) -> Result<bool> {
    let current: Option<String> =
        sqlx::query_scalar("SELECT run_id FROM meeting_diarization_status WHERE meeting_id = ?")
            .bind(meeting_id)
            .fetch_optional(pool)
            .await?
            .flatten();
    if let Some(current) = current {
        return Ok(current == run_id);
    }
    let latest: Option<String> = sqlx::query_scalar(
        "SELECT id FROM diarization_runs WHERE meeting_id = ? ORDER BY started_at DESC, rowid DESC LIMIT 1",
    ).bind(meeting_id).fetch_optional(pool).await?;
    Ok(latest.as_deref() == Some(run_id))
}

async fn mark_stale_run(pool: &sqlx::SqlitePool, run_id: &str) -> Result<()> {
    sqlx::query(
        "UPDATE diarization_runs SET status = 'needs_review',
        error = 'A newer run or manual correction takes precedence; current data retained',
        completed_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(run_id)
    .execute(pool)
    .await?;
    Ok(())
}

async fn publish_recovered_metadata(
    pool: &sqlx::SqlitePool,
    run: &InterruptedRun,
    windows: &[FinalWindow],
    transcripts: &[FinalTranscript],
) -> Result<()> {
    let mut transaction = pool.begin().await?;
    sqlx::query("DELETE FROM speaker_segments WHERE meeting_id = ?")
        .bind(&run.meeting_id)
        .execute(&mut *transaction)
        .await?;
    for window in windows {
        sqlx::query(
            "INSERT INTO speaker_segments (
                id, meeting_id, source, start_time, end_time, speaker_id,
                speaker_profile_id, confidence, is_overlap, diarization_status,
                diarization_method, candidate_speaker_ids
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'final',
                       'fluid_audio_offline_0.12.4', ?)",
        )
        .bind(format!("speaker-segment-{}", Uuid::new_v4()))
        .bind(&run.meeting_id)
        .bind(&window.source)
        .bind(window.start)
        .bind(window.end)
        .bind(&window.speaker_id)
        .bind(&window.speaker_profile_id)
        .bind(window.confidence)
        .bind(window.is_overlap as i64)
        .bind(serde_json::to_string(&window.candidate_speaker_ids)?)
        .execute(&mut *transaction)
        .await?;
    }
    sqlx::query("DELETE FROM transcript_speakers WHERE meeting_id = ?")
        .bind(&run.meeting_id)
        .execute(&mut *transaction)
        .await?;
    for transcript in transcripts {
        sqlx::query(
            "INSERT INTO transcript_speakers (
                transcript_id, meeting_id, audio_start_time, audio_end_time, speaker_id,
                speaker_label, speaker_profile_id, speaker_source, speaker_color, is_overlap,
                candidate_speaker_ids, diarization_status, diarization_method,
                diarization_confidence, run_id, published
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'final',
                       'fluid_audio_offline_0.12.4', ?, ?, 1)",
        )
        .bind(&transcript.id)
        .bind(&run.meeting_id)
        .bind(transcript.start)
        .bind(transcript.end)
        .bind(&transcript.speaker_id)
        .bind(&transcript.speaker_label)
        .bind(&transcript.speaker_profile_id)
        .bind(&transcript.source)
        .bind(if transcript.is_overlap {
            "#f97316"
        } else {
            "#3b82f6"
        })
        .bind(transcript.is_overlap as i64)
        .bind(serde_json::to_string(&transcript.candidates)?)
        .bind(transcript.confidence)
        .bind(&run.id)
        .execute(&mut *transaction)
        .await?;
    }
    sqlx::query(
        "UPDATE diarization_runs SET status = 'applied', progress = 100,
            completed_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(&run.id)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    set_status(
        pool,
        &run.meeting_id,
        &run.id,
        "completed",
        100,
        None,
        &quality_flags_from_json(run.quality_flags.as_deref()),
        run.assigned_coverage,
        run.transcription_success_rate,
    )
    .await
}

fn quality_flags_from_json(value: Option<&str>) -> Vec<String> {
    value
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::parse_macos_version;

    #[test]
    fn microphone_stems_do_not_imply_one_local_speaker() {
        let directory = tempfile::tempdir().unwrap();
        let stems = directory.path().join(".diarization");
        std::fs::create_dir(&stems).unwrap();
        std::fs::write(stems.join("microphone.flac"), []).unwrap();
        std::fs::write(stems.join("system.flac"), []).unwrap();
        let (sources, _) = discover_sources(directory.path()).unwrap();
        assert_eq!(sources.len(), 2);
        assert!(sources.iter().all(|source| source.forced_speaker_id.is_none()));
    }

    #[test]
    fn parses_macos_versions_for_the_refinement_check() {
        assert_eq!(parse_macos_version("14.4.1\n"), Some((14, 4)));
        assert_eq!(parse_macos_version("15"), Some((15, 0)));
        assert_eq!(parse_macos_version("13.6"), Some((13, 6)));
        assert_eq!(parse_macos_version(""), None);
        assert!(parse_macos_version("14.3").unwrap() < (14, 4));
        assert!(parse_macos_version("26.0").unwrap() >= (14, 4));
    }

    use super::*;

    fn speaker_row(
        id: &str,
        label: &str,
        start: f64,
        end: f64,
    ) -> crate::database::repositories::diarization::TranscriptSpeakerMetadata {
        crate::database::repositories::diarization::TranscriptSpeakerMetadata {
            transcript_id: format!("row-{start}"),
            meeting_id: "m1".into(),
            audio_start_time: Some(start),
            audio_end_time: Some(end),
            speaker_id: Some(id.into()),
            speaker_label: Some(label.into()),
            speaker_profile_id: None,
            speaker_source: Some("mixed".into()),
            speaker_color: None,
            is_overlap: 0,
            candidate_speaker_ids: None,
            diarization_status: "final".into(),
            diarization_method: None,
            diarization_confidence: Some(0.9),
        }
    }

    fn speaker_window(id: &str, start: f64, end: f64) -> FinalWindow {
        FinalWindow {
            source: "mixed".into(),
            start,
            end,
            speaker_id: Some(id.into()),
            speaker_profile_id: None,
            candidate_speaker_ids: vec![id.into()],
            confidence: 0.9,
            is_overlap: false,
        }
    }

    #[test]
    fn enhancement_checks_fresh_rows_instead_of_old_chunk_times() {
        let windows = vec![speaker_window("a", 1.0, 2.0), speaker_window("b", 15.0, 16.0)];
        let old = vec![live_row("old", 0.0, 88.0)];
        assert!(!passes_quality_gates(assignment_coverage(&windows, &old), 1.0));
        let fresh = vec![live_row("fresh-a", 1.25, 1.41), live_row("fresh-b", 15.5, 15.66)];
        let analysis = EnhancementSpeakerAnalysis {
            windows,
            labels: HashMap::from([("a".into(), "Alice".into()), ("b".into(), "Bob".into())]),
            quality_flags: vec![],
        };
        let result = analysis.label(&fresh).unwrap();
        assert_eq!(result.transcripts[0].speaker_label, "Alice");
        assert_eq!(result.transcripts[1].speaker_label, "Bob");
        assert_eq!(result.coverage, 1.0);
    }

    #[test]
    fn remembered_name_replaces_live_label_in_final_transcript() {
        let live = vec![speaker_row("old-a", "Speaker 1", 0.0, 10.0)];
        let mut windows = vec![speaker_window("new-a", 0.0, 10.0)];
        stabilize_speaker_ids(&mut windows, &live);
        windows[0].speaker_profile_id = Some("alice-profile".into());
        let names = HashMap::from([("old-a".into(), "Alice".into())]);
        let labels = speaker_labels(&windows, &live, &names);
        let rows = label_existing_transcripts(&[live_row("t1", 0.0, 10.0)], &windows, &labels);
        assert_eq!(rows[0].speaker_label, "Alice");
        assert_eq!(rows[0].speaker_profile_id.as_deref(), Some("alice-profile"));
        assert_eq!(rows[0].speaker_id.as_deref(), Some("old-a"));
    }

    #[test]
    fn manual_name_wins_over_profile_match_and_later_automatic_rows() {
        let mut manual = speaker_row("old-a", "User name", 0.0, 5.0);
        manual.diarization_method = Some("manual".into());
        let live = vec![manual, speaker_row("old-a", "Speaker 1", 5.0, 10.0)];
        let windows = vec![speaker_window("old-a", 0.0, 10.0)];
        let names = HashMap::from([("old-a".into(), "Alice".into())]);
        let labels = speaker_labels(&windows, &live, &names);
        assert_eq!(labels["old-a"], "User name");
    }

    #[test]
    fn rerun_keeps_nine_voices_when_old_result_has_two() {
        let mut windows = (0..9)
            .map(|i| {
                speaker_window(
                    &format!("mixed-S{i}"),
                    i as f64 * 10.0,
                    (i + 1) as f64 * 10.0,
                )
            })
            .collect::<Vec<_>>();
        let live = vec![
            speaker_row("old-a", "Speaker 1", 0.0, 80.0),
            speaker_row("old-b", "Speaker 2", 80.0, 90.0),
        ];
        stabilize_speaker_ids(&mut windows, &live);
        let ids: BTreeSet<_> = windows
            .iter()
            .filter_map(|w| w.speaker_id.as_ref())
            .collect();
        assert_eq!(ids.len(), 9);
        let labels = speaker_labels(&windows, &live, &HashMap::new());
        let displayed: BTreeSet<_> = ids.iter().map(|id| &labels[*id]).collect();
        assert_eq!(displayed.len(), 9);
        let rows = (0..9)
            .map(|i| live_row(&format!("t{i}"), i as f64 * 10.0, (i + 1) as f64 * 10.0))
            .collect::<Vec<_>>();
        let published = label_existing_transcripts(&rows, &windows, &labels);
        assert_eq!(
            published
                .iter()
                .filter_map(|row| row.speaker_id.as_ref())
                .collect::<BTreeSet<_>>()
                .len(),
            9
        );
        assert_eq!(
            published
                .iter()
                .map(|row| &row.speaker_label)
                .collect::<BTreeSet<_>>()
                .len(),
            9
        );
        for (old, new) in rows.iter().zip(&published) {
            assert_eq!(old.id, new.id);
            assert_eq!(old.transcript, new.text);
        }
        for window in &windows {
            assert_eq!(
                window.candidate_speaker_ids,
                vec![window.speaker_id.clone().unwrap()]
            );
        }
    }

    #[test]
    fn stabilization_uses_total_time_and_does_not_collide_with_raw_ids() {
        let mut windows = vec![
            speaker_window("new-a", 0.0, 6.0),
            speaker_window("new-a", 10.0, 16.0),
            speaker_window("new-b", 20.0, 30.0),
            speaker_window("old-a", 40.0, 45.0),
        ];
        let live = vec![
            speaker_row("old-a", "Alice", 0.0, 6.0),
            speaker_row("old-a", "Alice", 10.0, 16.0),
            speaker_row("old-a", "Alice", 20.0, 30.0),
        ];
        stabilize_speaker_ids(&mut windows, &live);
        assert_eq!(windows[0].speaker_id.as_deref(), Some("old-a"));
        assert_eq!(windows[1].speaker_id, windows[0].speaker_id);
        assert_ne!(windows[2].speaker_id, windows[0].speaker_id);
        assert_ne!(windows[3].speaker_id, windows[0].speaker_id);
        assert_ne!(windows[3].speaker_id, windows[2].speaker_id);
    }

    #[test]
    fn stabilization_keeps_you_and_existing_names_on_repeat_runs() {
        let live = vec![
            speaker_row("old-a", "Alice", 0.0, 10.0),
            speaker_row("old-b", "Bob", 10.0, 20.0),
            speaker_row("local-you", "You", 20.0, 40.0),
        ];
        let mut windows = vec![
            speaker_window("new-b", 0.0, 10.0),
            speaker_window("new-a", 10.0, 20.0),
            speaker_window("local-you", 20.0, 30.0),
            speaker_window("new-c", 30.0, 40.0),
        ];
        stabilize_speaker_ids(&mut windows, &live);
        assert_eq!(windows[0].speaker_id.as_deref(), Some("old-a"));
        assert_eq!(windows[1].speaker_id.as_deref(), Some("old-b"));
        assert_eq!(windows[2].speaker_id.as_deref(), Some("local-you"));
        assert_ne!(windows[3].speaker_id.as_deref(), Some("local-you"));
        let labels = speaker_labels(&windows, &live, &HashMap::new());
        assert_eq!(labels["old-a"], "Alice");
        assert_eq!(labels["old-b"], "Bob");
        let ids = windows
            .iter()
            .map(|w| w.speaker_id.clone())
            .collect::<Vec<_>>();
        stabilize_speaker_ids(&mut windows, &live);
        assert_eq!(
            windows
                .iter()
                .map(|w| w.speaker_id.clone())
                .collect::<Vec<_>>(),
            ids
        );
    }

    #[test]
    fn rebuilds_remote_overlap_as_multiple_speakers() {
        let intervals = vec![
            SidecarInterval {
                speaker_id: "system-a".into(),
                source: "system".into(),
                start: 0.0,
                end: 2.0,
                confidence: 0.9,
                is_overlap: true,
                candidate_speaker_ids: vec!["system-b".into()],
            },
            SidecarInterval {
                speaker_id: "system-b".into(),
                source: "system".into(),
                start: 1.0,
                end: 3.0,
                confidence: 0.8,
                is_overlap: true,
                candidate_speaker_ids: vec!["system-a".into()],
            },
        ];
        let windows = rebuild_windows(&intervals);
        assert!(windows.iter().any(|window| {
            window.start == 1.0
                && window.end == 2.0
                && window.speaker_id.is_none()
                && window.candidate_speaker_ids.len() == 2
        }));
    }

    #[test]
    fn keeps_cross_source_overlap_as_separate_windows() {
        let intervals = vec![
            SidecarInterval {
                speaker_id: "local-you".into(),
                source: "microphone".into(),
                start: 0.0,
                end: 2.0,
                confidence: 1.0,
                is_overlap: false,
                candidate_speaker_ids: vec![],
            },
            SidecarInterval {
                speaker_id: "system-a".into(),
                source: "system".into(),
                start: 1.0,
                end: 3.0,
                confidence: 0.9,
                is_overlap: false,
                candidate_speaker_ids: vec![],
            },
        ];
        let windows = rebuild_windows(&intervals);
        assert_eq!(windows.len(), 4);
        assert!(windows
            .iter()
            .filter(|window| window.start == 1.0 && window.end == 2.0)
            .all(|window| window.is_overlap));
        assert!(windows
            .iter()
            .filter(|window| window.end <= 1.0 || window.start >= 2.0)
            .all(|window| !window.is_overlap));
    }

    fn enhancement_analysis(intervals: &[SidecarInterval]) -> EnhancementSpeakerAnalysis {
        EnhancementSpeakerAnalysis { windows: rebuild_windows(intervals), labels: HashMap::new(), quality_flags: vec![] }
    }

    fn source_interval(source: &str, speaker: &str, start: f64, end: f64) -> SidecarInterval {
        SidecarInterval { source: source.into(), speaker_id: speaker.into(), start, end,
            confidence: 1.0, is_overlap: false, candidate_speaker_ids: vec![] }
    }

    #[test]
    fn whisper_turns_ignore_cross_source_overlap() {
        let analysis = enhancement_analysis(&[
            source_interval("microphone", "you", 0.0, 2.0),
            source_interval("system", "remote", 1.0, 3.0),
        ]);
        assert!(analysis.turn_boundaries().is_empty(), "overlap is not a turn at 1.5 seconds");
    }

    #[test]
    fn whisper_turns_keep_real_changes_among_interleaved_sources() {
        let analysis = enhancement_analysis(&[
            source_interval("microphone", "you", 1.0, 5.0),
            source_interval("system", "alice", 0.0, 3.0),
            source_interval("system", "bob", 3.0, 6.0),
        ]);
        assert_eq!(analysis.turn_boundaries(), vec![3.0]);
    }

    #[test]
    fn whisper_turns_do_not_cut_through_ambiguous_same_source_overlap() {
        let analysis = enhancement_analysis(&[
            source_interval("system", "alice", 0.0, 2.0),
            source_interval("system", "bob", 1.0, 3.0),
        ]);
        assert!(analysis.turn_boundaries().is_empty());
    }

    fn process_request() -> SidecarRequest {
        SidecarRequest { action: "process".into(), meeting_id: "test".into(),
            model_cache_path: "unused".into(), output_path: "unused".into(), sources: vec![] }
    }

    #[tokio::test]
    async fn a_pending_enhance_cancel_is_preserved_before_sidecar_start() {
        let cancelled = AtomicBool::new(true);
        let result = invoke_sidecar_with_cancellation(&process_request(), &cancelled).await;
        assert!(result.unwrap_err().to_string().contains("cancelled"));
        assert!(cancelled.load(Ordering::SeqCst));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn enhancement_cancellation_stops_its_running_sidecar() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let binary = directory.path().join("fake-sidecar");
        let started = directory.path().join("fake-sidecar.started");
        std::fs::write(&binary, "#!/bin/sh\ncat > /dev/null\n: > \"$0.started\"\nexec /bin/sleep 30\n").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let cancelled = AtomicBool::new(false);
        let request = process_request();
        let cancel = async {
            while !started.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            cancelled.store(true, Ordering::SeqCst);
        };
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(invoke_sidecar_process(&binary, &request, &cancelled), cancel)
        }).await.expect("cancellation must not wait for the 30-second helper");
        assert!(result.unwrap_err().to_string().contains("cancelled"));
        assert!(cancelled.load(Ordering::SeqCst));
    }

    #[test]
    fn final_quality_gates_are_inclusive_at_thresholds() {
        assert!(passes_quality_gates(0.75, 0.95));
        assert!(!passes_quality_gates(0.749, 1.0));
        assert!(!passes_quality_gates(1.0, 0.949));
    }

    #[tokio::test]
    async fn interrupted_apply_with_final_core_rows_publishes_metadata() {
        use sqlx::sqlite::SqlitePoolOptions;
        for (interrupted_status, has_manual_correction) in [
            ("applying", false),
            ("fallback_to_live", false),
            ("failed", false),
            ("recovery_required", false),
            ("applying", true),
            ("fallback_to_live", true),
            ("failed", true),
            ("recovery_required", true),
        ] {
            let core = SqlitePoolOptions::new()
                .max_connections(1)
                .connect("sqlite::memory:")
                .await
                .unwrap();
            sqlx::migrate!("./migrations").run(&core).await.unwrap();
            sqlx::query(
            "INSERT INTO meetings (id, title, created_at, updated_at) VALUES ('m1', 'Test', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        ).execute(&core).await.unwrap();
            sqlx::query(
            "INSERT INTO transcripts (id, meeting_id, transcript, timestamp, audio_start_time, audio_end_time, duration)
             VALUES ('final-1', 'm1', 'final text', '00:00:01', 1.0, 2.0, 1.0)",
        ).execute(&core).await.unwrap();

            let feature = SqlitePoolOptions::new()
                .max_connections(1)
                .connect("sqlite::memory:")
                .await
                .unwrap();
            sqlx::migrate!("./speaker-migrations")
                .run(&feature)
                .await
                .unwrap();
            let final_transcripts = vec![FinalTranscript {
                id: "final-1".to_string(),
                text: "final text".to_string(),
                start: 1.0,
                end: 2.0,
                speaker_id: Some("speaker-1".to_string()),
                speaker_profile_id: None,
                speaker_label: "Alice".to_string(),
                source: "system".to_string(),
                is_overlap: false,
                candidates: Vec::new(),
                confidence: 0.9,
            }];
            let windows = vec![FinalWindow {
                source: "system".to_string(),
                start: 1.0,
                end: 2.0,
                speaker_id: Some("speaker-1".to_string()),
                speaker_profile_id: None,
                candidate_speaker_ids: Vec::new(),
                confidence: 0.9,
                is_overlap: false,
            }];
            let actual = load_core_backup(&core, "m1").await.unwrap();
            let live_backup = serde_json::to_string(&Vec::<CoreTranscriptBackup>::new()).unwrap();
            sqlx::query(
                "INSERT INTO diarization_runs (
                id, meeting_id, status, method, transcript_backup, proposed_transcripts,
                proposed_speaker_metadata, live_rows_hash, final_rows_hash
             ) VALUES ('r1', 'm1', ?, 'test', ?, ?, ?, ?, ?)",
            )
            .bind(interrupted_status)
            .bind(&live_backup)
            .bind(serde_json::to_string(&final_transcripts).unwrap())
            .bind(serde_json::to_string(&windows).unwrap())
            .bind(hash_json(&live_backup))
            .bind(hash_json(&serde_json::to_string(&actual).unwrap()))
            .execute(&feature)
            .await
            .unwrap();

            if has_manual_correction {
                sqlx::query("INSERT INTO transcript_speakers (transcript_id, meeting_id, speaker_id, speaker_label, diarization_method, published) VALUES ('final-1', 'm1', 'corrected', 'User correction', 'manual', 1)")
                    .execute(&feature).await.unwrap();
            }
            reconcile_interrupted_runs(&core, &feature).await.unwrap();

            let status: String =
                sqlx::query_scalar("SELECT status FROM diarization_runs WHERE id='r1'")
                    .fetch_one(&feature)
                    .await
                    .unwrap();
            assert_eq!(
                status,
                if has_manual_correction {
                    "needs_review"
                } else {
                    "applied"
                }
            );
            let label: String = sqlx::query_scalar(
            "SELECT speaker_label FROM transcript_speakers WHERE transcript_id='final-1' AND published=1",
        ).fetch_one(&feature).await.unwrap();
            assert_eq!(
                label,
                if has_manual_correction {
                    "User correction"
                } else {
                    "Alice"
                }
            );
        }
    }
    fn live_row(id: &str, start: f64, end: f64) -> Transcript {
        Transcript {
            id: id.to_string(),
            meeting_id: "m1".to_string(),
            transcript: format!("Original words for {id}"),
            timestamp: "wall-clock".to_string(),
            summary: Some("Keep summary".to_string()),
            action_items: None,
            key_points: None,
            audio_start_time: Some(start),
            audio_end_time: Some(end),
            duration: Some(end - start),
        }
    }

    #[test]
    fn speaker_labels_keep_unmatched_words_and_transcript_ids() {
        let rows = vec![live_row("t1", 0.0, 10.0), live_row("t2", 10.0, 20.0)];
        let windows = rebuild_windows(&[SidecarInterval {
            speaker_id: "a".into(),
            source: "system".into(),
            start: 0.0,
            end: 10.0,
            confidence: 0.9,
            is_overlap: false,
            candidate_speaker_ids: vec![],
        }]);
        let labels = HashMap::from([("a".to_string(), "Alice".to_string())]);
        let result = label_existing_transcripts(&rows, &windows, &labels);
        assert_eq!(result.len(), rows.len());
        for (old, new) in rows.iter().zip(&result) {
            assert_eq!(old.id, new.id);
            assert_eq!(old.transcript, new.text);
        }
        assert_eq!(result[0].speaker_label, "Alice");
        assert_eq!(result[1].speaker_label, "Unknown speaker");
    }

    #[test]
    fn a_short_interruption_does_not_mark_the_whole_turn_as_overlap() {
        let windows = rebuild_windows(&[
            SidecarInterval {
                speaker_id: "a".into(),
                source: "system".into(),
                start: 0.0,
                end: 60.0,
                confidence: 1.0,
                is_overlap: true,
                candidate_speaker_ids: vec!["b".into()],
            },
            SidecarInterval {
                speaker_id: "b".into(),
                source: "system".into(),
                start: 10.0,
                end: 11.0,
                confidence: 1.0,
                is_overlap: true,
                candidate_speaker_ids: vec!["a".into()],
            },
        ]);
        assert_eq!(windows.len(), 3);
        assert!(!windows[0].is_overlap);
        assert!(windows[1].is_overlap);
        assert!(!windows[2].is_overlap);
        let labels = HashMap::from([("a".to_string(), "Alice".to_string())]);
        let result = label_existing_transcripts(&[live_row("t1", 0.0, 60.0)], &windows, &labels);
        assert!(!result[0].is_overlap);
        assert_eq!(result[0].speaker_label, "Alice");
    }

    #[test]
    fn coverage_counts_all_slices_without_counting_sources_twice() {
        let windows = rebuild_windows(&[
            SidecarInterval {
                speaker_id: "a".into(),
                source: "microphone".into(),
                start: 0.0,
                end: 10.0,
                confidence: 1.0,
                is_overlap: false,
                candidate_speaker_ids: vec![],
            },
            SidecarInterval {
                speaker_id: "b".into(),
                source: "system".into(),
                start: 4.0,
                end: 6.0,
                confidence: 1.0,
                is_overlap: false,
                candidate_speaker_ids: vec![],
            },
        ]);
        assert_eq!(
            assignment_coverage(&windows, &[live_row("t1", 0.0, 10.0)]),
            1.0
        );
    }

    #[tokio::test]
    async fn metadata_publish_failure_never_changes_transcript_text() {
        use sqlx::sqlite::SqlitePoolOptions;
        let core = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&core).await.unwrap();
        sqlx::query("INSERT INTO meetings(id,title,created_at,updated_at) VALUES ('m1','Test',CURRENT_TIMESTAMP,CURRENT_TIMESTAMP)")
            .execute(&core).await.unwrap();
        sqlx::query("INSERT INTO transcripts(id,meeting_id,transcript,timestamp,summary,audio_start_time,audio_end_time,duration) VALUES ('t1','m1','Keep these words','original time','Keep summary',0,10,10)")
            .execute(&core).await.unwrap();
        let feature = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./speaker-migrations")
            .run(&feature)
            .await
            .unwrap();
        sqlx::query("INSERT INTO diarization_runs(id,meeting_id,status,method) VALUES ('r1','m1','prepared','test')")
            .execute(&feature).await.unwrap();
        sqlx::query("CREATE TRIGGER fail_publish BEFORE INSERT ON speaker_segments BEGIN SELECT RAISE(ABORT, 'test failure'); END")
            .execute(&feature).await.unwrap();
        let before = load_core_backup(&core, "m1").await.unwrap();
        let original = load_transcripts(&core, "m1").await.unwrap();
        let windows = rebuild_windows(&[SidecarInterval {
            speaker_id: "a".into(),
            source: "mixed".into(),
            start: 0.0,
            end: 10.0,
            confidence: 1.0,
            is_overlap: false,
            candidate_speaker_ids: vec![],
        }]);
        let result = label_existing_transcripts(&original, &windows, &HashMap::new());
        assert!(publish_labels_atomically(
            &core,
            &feature,
            "m1",
            "r1",
            &windows,
            &result,
            &original,
            &[],
            1.0,
            1.0
        )
        .await
        .is_err());
        let after = load_core_backup(&core, "m1").await.unwrap();
        assert_eq!(
            serde_json::to_string(&before).unwrap(),
            serde_json::to_string(&after).unwrap()
        );
        sqlx::query("DROP TRIGGER fail_publish")
            .execute(&feature)
            .await
            .unwrap();
        publish_labels_atomically(
            &core,
            &feature,
            "m1",
            "r1",
            &windows,
            &result,
            &original,
            &[],
            1.0,
            1.0,
        )
        .await
        .unwrap();
        let after = load_core_backup(&core, "m1").await.unwrap();
        assert_eq!(
            serde_json::to_string(&before).unwrap(),
            serde_json::to_string(&after).unwrap()
        );
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM transcript_speakers WHERE transcript_id='t1'")
                .fetch_one(&feature)
                .await
                .unwrap();
        assert_eq!(count, 1);
    }
    #[tokio::test]
    async fn an_old_failed_run_never_restores_over_newer_transcript_text() {
        use sqlx::sqlite::SqlitePoolOptions;
        let core = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&core).await.unwrap();
        sqlx::query("INSERT INTO meetings(id,title,created_at,updated_at) VALUES ('m1','Test',CURRENT_TIMESTAMP,CURRENT_TIMESTAMP)")
            .execute(&core).await.unwrap();
        sqlx::query("INSERT INTO transcripts(id,meeting_id,transcript,timestamp) VALUES ('newer','m1','Newer text must stay','original time')")
            .execute(&core).await.unwrap();
        let feature = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./speaker-migrations")
            .run(&feature)
            .await
            .unwrap();
        sqlx::query("INSERT INTO diarization_runs(id,meeting_id,status,method,transcript_backup,proposed_transcripts,proposed_speaker_metadata,live_rows_hash,final_rows_hash) VALUES ('r1','m1','fallback_to_live','test','[]','[]','[]','old-live-hash','old-final-hash')")
            .execute(&feature).await.unwrap();
        let before = serde_json::to_string(&load_core_backup(&core, "m1").await.unwrap()).unwrap();
        reconcile_interrupted_runs(&core, &feature).await.unwrap();
        let after = serde_json::to_string(&load_core_backup(&core, "m1").await.unwrap()).unwrap();
        assert_eq!(before, after);
        let status: String =
            sqlx::query_scalar("SELECT status FROM diarization_runs WHERE id='r1'")
                .fetch_one(&feature)
                .await
                .unwrap();
        assert_eq!(status, "needs_review");
    }

    #[test]
    fn mixed_speaker_rows_are_not_clean_voice_samples() {
        let windows = rebuild_windows(&[
            SidecarInterval {
                speaker_id: "a".into(),
                source: "system".into(),
                start: 0.0,
                end: 6.0,
                confidence: 0.95,
                is_overlap: false,
                candidate_speaker_ids: vec![],
            },
            SidecarInterval {
                speaker_id: "b".into(),
                source: "system".into(),
                start: 6.0,
                end: 10.0,
                confidence: 0.95,
                is_overlap: false,
                candidate_speaker_ids: vec![],
            },
        ]);
        let result = label_existing_transcripts(
            &[live_row("t1", 0.0, 10.0)],
            &windows,
            &HashMap::from([("a".to_string(), "Alice".to_string())]),
        );
        assert_eq!(result[0].speaker_label, "Alice");
        assert!(result[0].confidence < 0.8);
    }

    #[tokio::test]
    async fn legacy_recovery_does_not_change_a_newer_run_status() {
        use sqlx::sqlite::SqlitePoolOptions;
        let core = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&core).await.unwrap();
        let feature = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./speaker-migrations")
            .run(&feature)
            .await
            .unwrap();
        sqlx::query("INSERT INTO diarization_runs(id,meeting_id,status,method) VALUES ('old','m1','prepared','test'),('new','m1','applied','test')")
            .execute(&feature).await.unwrap();
        sqlx::query("INSERT INTO meeting_diarization_status(meeting_id,run_id,current_status) VALUES ('m1','new','completed')")
            .execute(&feature).await.unwrap();
        reconcile_interrupted_runs(&core, &feature).await.unwrap();
        let status: String = sqlx::query_scalar(
            "SELECT current_status FROM meeting_diarization_status WHERE meeting_id='m1'",
        )
        .fetch_one(&feature)
        .await
        .unwrap();
        assert_eq!(status, "completed");
    }
}
