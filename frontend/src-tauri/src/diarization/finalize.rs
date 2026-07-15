use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::{AtomicBool, Ordering},
};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::Connection;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tokio::{io::AsyncWriteExt, process::Command};
use uuid::Uuid;

use crate::{
    audio::{
        decoder::decode_audio_file,
        recording_state::{AudioChunk, DeviceType},
        transcription::{
            engine::{get_or_init_transcription_engine, validate_transcription_model_ready},
            worker::transcribe_chunk_with_provider,
        },
    },
    database::models::Transcript,
    state::AppState,
};

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

#[derive(Debug, Clone)]
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

#[derive(Debug)]
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

pub fn cancel() {
    CANCEL_REQUESTED.store(true, Ordering::SeqCst);
}

pub fn fluid_model_cache<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf> {
    Ok(app
        .path()
        .app_data_dir()?
        .join("diarization/fluid-audio-0.12.4"))
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
                path.metadata().map(|metadata| metadata.len() > 0).unwrap_or(false)
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
        output_path: cache.join("prepare-result.json").to_string_lossy().into_owned(),
        sources: Vec::new(),
    };
    let response = invoke_sidecar(&request).await?;
    if response.ok {
        Ok(())
    } else {
        Err(anyhow!(
            "Offline model preparation failed: {}",
            response.error.unwrap_or_else(|| "unknown error".to_string())
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
    CANCEL_REQUESTED.store(false, Ordering::SeqCst);
    let state = app.state::<AppState>();
    let pool = state.db_manager.pool();
    let settings =
        crate::database::repositories::diarization::DiarizationRepository::get_settings(pool)
            .await?;
    if settings.enabled == 0 || settings.post_call_refinement_enabled == 0 {
        return Err(anyhow!("Final speaker refinement is disabled"));
    }

    let manual_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM transcripts WHERE meeting_id = ? AND diarization_method = 'manual'",
    )
    .bind(&request.meeting_id)
    .fetch_one(pool)
    .await?;
    if manual_count > 0 && !request.overwrite_manual_edits {
        return Err(anyhow!(
            "This meeting has manual speaker edits. Confirm before replacing them."
        ));
    }

    let folder_path: Option<String> =
        sqlx::query_scalar("SELECT folder_path FROM meetings WHERE id = ?")
            .bind(&request.meeting_id)
            .fetch_optional(pool)
            .await?
            .flatten();
    let folder =
        PathBuf::from(folder_path.ok_or_else(|| anyhow!("This meeting has no retained audio"))?);
    let (sources, quality_flags) = discover_sources(&folder)?;
    let live_transcripts = load_transcripts(pool, &request.meeting_id).await?;
    let backup = serde_json::to_string(&live_transcripts)?;
    let run_id = format!("diarization-run-{}", Uuid::new_v4());

    sqlx::query(
        "INSERT INTO diarization_runs (
            id, meeting_id, status, method, progress, quality_flags, transcript_backup
         ) VALUES (?, ?, 'running', 'fluid_audio_offline_0.12.4', 1, ?, ?)",
    )
    .bind(&run_id)
    .bind(&request.meeting_id)
    .bind(serde_json::to_string(&quality_flags)?)
    .bind(backup)
    .execute(pool)
    .await?;
    set_status(
        pool,
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
        quality_flags.clone(),
    )
    .await;

    match run_result {
        Ok(result) => Ok(result),
        Err(error) => {
            let cancelled = CANCEL_REQUESTED.load(Ordering::SeqCst);
            let status = if cancelled {
                "cancelled"
            } else {
                "fallback_to_live"
            };
            sqlx::query(
                "UPDATE diarization_runs SET status = ?, error = ?, completed_at = CURRENT_TIMESTAMP
                 WHERE id = ?",
            )
            .bind(status)
            .bind(error.to_string())
            .bind(&run_id)
            .execute(pool)
            .await?;
            set_status(
                pool,
                &request.meeting_id,
                &run_id,
                status,
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
                status,
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
    stabilize_speaker_ids(&mut windows, live_transcripts);
    let assigned_coverage = assignment_coverage(&windows, live_transcripts);
    if windows.iter().any(|window| window.is_overlap) {
        quality_flags.push("overlap_detected".to_string());
    }

    check_cancelled()?;
    let audio = load_source_audio(&sidecar_request.sources).await?;
    let profile_labels = recognize_final_profiles(app, &mut windows, &audio).await;
    validate_transcription_model_ready(app)
        .await
        .map_err(|error| anyhow!("Unable to load the selected transcription model: {error}"))?;
    let engine = get_or_init_transcription_engine(app)
        .await
        .map_err(|error| anyhow!(error))?;
    let labels = speaker_labels(&windows, live_transcripts, &profile_labels);
    let mut transcripts = Vec::new();
    let mut attempted = 0usize;
    let mut successful = 0usize;
    for (index, window) in windows.iter().enumerate() {
        check_cancelled()?;
        attempted += 1;
        let Some(samples) = slice_window(&audio, window) else {
            continue;
        };
        let chunk = AudioChunk {
            data: samples,
            sample_rate: 16_000,
            timestamp: window.start,
            end_timestamp: Some(window.end),
            chunk_id: index as u64,
            device_type: DeviceType::Microphone,
            source_activity: None,
        };
        match transcribe_chunk_with_provider(&engine, chunk, app).await {
            Ok((text, _, _)) if !text.trim().is_empty() => {
                successful += 1;
                let speaker_label = if window.speaker_id.is_none() {
                    "Multiple speakers".to_string()
                } else {
                    labels
                        .get(window.speaker_id.as_deref().unwrap_or_default())
                        .cloned()
                        .unwrap_or_else(|| "Unknown speaker".to_string())
                };
                transcripts.push(FinalTranscript {
                    id: format!("transcript-{}", Uuid::new_v4()),
                    text,
                    start: window.start,
                    end: window.end,
                    speaker_id: window.speaker_id.clone(),
                    speaker_profile_id: window.speaker_profile_id.clone(),
                    speaker_label,
                    source: window.source.clone(),
                    is_overlap: window.is_overlap,
                    candidates: window.candidate_speaker_ids.clone(),
                    confidence: window.confidence,
                });
            }
            Ok(_) | Err(_) => {}
        }
        let progress = 50 + (((index + 1) as f64 / windows.len() as f64) * 40.0) as u32;
        emit_progress(app, meeting_id, "transcribing_windows", progress, None);
    }
    let transcription_success_rate = if attempted == 0 {
        0.0
    } else {
        successful as f64 / attempted as f64
    };

    if !passes_quality_gates(assigned_coverage, transcription_success_rate) {
        quality_flags.push("quality_gate_failed".to_string());
        return Err(anyhow!(
            "Final result did not meet quality gates (coverage {:.0}%, transcription {:.0}%)",
            assigned_coverage * 100.0,
            transcription_success_rate * 100.0
        ));
    }

    emit_progress(app, meeting_id, "saving", 95, None);
    replace_atomically(
        app.state::<AppState>().db_manager.pool(),
        meeting_id,
        run_id,
        &windows,
        &transcripts,
        &quality_flags,
        assigned_coverage,
        transcription_success_rate,
    )
    .await?;
    cleanup_temporary_stems(folder);
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

fn cleanup_temporary_stems(folder: &Path) {
    if folder.join("audio.mp4").exists() {
        return;
    }
    for stem in [
        folder.join(".diarization/microphone.flac"),
        folder.join(".diarization/system.flac"),
    ] {
        if let Err(error) = std::fs::remove_file(&stem) {
            if error.kind() != std::io::ErrorKind::NotFound {
                log::warn!("Failed to remove temporary diarization stem {}: {error}", stem.display());
            }
        }
    }
}

fn passes_quality_gates(assigned_coverage: f64, transcription_success_rate: f64) -> bool {
    assigned_coverage >= 0.80 && transcription_success_rate >= 0.95
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
                    forced_speaker_id: Some("local-you".to_string()),
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

fn find_meeting_audio(folder: &Path) -> Result<PathBuf> {
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
    let binary = resolve_sidecar_binary()?;
    let mut child = Command::new(&binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Unable to launch {}", binary.display()))?;
    let payload = serde_json::to_vec(request)?;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("Sidecar stdin unavailable"))?
        .write_all(&payload)
        .await?;
    let output = child.wait_with_output().await?;
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
    let mut by_source: BTreeMap<&str, Vec<&SidecarInterval>> = BTreeMap::new();
    for interval in intervals {
        if interval.end - interval.start >= 0.25 {
            by_source
                .entry(&interval.source)
                .or_default()
                .push(interval);
        }
    }
    let mut windows = Vec::new();
    for (source, source_intervals) in by_source {
        let mut boundaries: Vec<f64> = source_intervals
            .iter()
            .flat_map(|interval| [interval.start, interval.end])
            .collect();
        boundaries.sort_by(|a, b| a.total_cmp(b));
        boundaries.dedup_by(|a, b| (*a - *b).abs() < 0.001);
        for pair in boundaries.windows(2) {
            let (start, end) = (pair[0], pair[1]);
            if end - start < 0.25 {
                continue;
            }
            let midpoint = (start + end) / 2.0;
            let active: Vec<_> = source_intervals
                .iter()
                .filter(|interval| interval.start <= midpoint && interval.end >= midpoint)
                .collect();
            if active.is_empty() {
                continue;
            }
            let candidates: BTreeSet<String> = active
                .iter()
                .map(|interval| interval.speaker_id.clone())
                .collect();
            let speaker_id = if candidates.len() == 1 {
                candidates.first().cloned()
            } else {
                None
            };
            windows.push(FinalWindow {
                source: source.to_string(),
                start,
                end,
                speaker_id,
                speaker_profile_id: None,
                candidate_speaker_ids: candidates.into_iter().collect(),
                confidence: active
                    .iter()
                    .map(|interval| interval.confidence)
                    .sum::<f64>()
                    / active.len() as f64,
                is_overlap: active.len() > 1
                    || active.iter().any(|interval| {
                        interval.is_overlap || !interval.candidate_speaker_ids.is_empty()
                    }),
            });
        }
    }
    windows.sort_by(|a, b| a.start.total_cmp(&b.start));
    for index in 0..windows.len() {
        if windows.iter().enumerate().any(|(other_index, other)| {
            index != other_index
                && windows[index].source != other.source
                && other.end > windows[index].start
                && other.start < windows[index].end
        }) {
            windows[index].is_overlap = true;
        }
    }
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

fn stabilize_speaker_ids(windows: &mut [FinalWindow], live: &[Transcript]) {
    let mut mapping: HashMap<String, (String, f64)> = HashMap::new();
    for window in windows.iter() {
        let Some(final_id) = &window.speaker_id else {
            continue;
        };
        if final_id == "local-you" {
            continue;
        }
        for transcript in live {
            let Some(provisional_id) = transcript.speaker_id.as_ref() else {
                continue;
            };
            if matches!(
                provisional_id.as_str(),
                "multiple-speakers" | "unknown-speaker"
            ) {
                continue;
            }
            let start = transcript.audio_start_time.unwrap_or(0.0).max(window.start);
            let end = transcript.audio_end_time.unwrap_or(start).min(window.end);
            let overlap = (end - start).max(0.0);
            if overlap > mapping.get(final_id).map(|entry| entry.1).unwrap_or(0.0) {
                mapping.insert(final_id.clone(), (provisional_id.clone(), overlap));
            }
        }
    }
    for window in windows {
        if let Some(id) = &window.speaker_id {
            if let Some((stable, _)) = mapping.get(id) {
                window.speaker_id = Some(stable.clone());
            }
        }
        for candidate in &mut window.candidate_speaker_ids {
            if let Some((stable, _)) = mapping.get(candidate) {
                *candidate = stable.clone();
            }
        }
    }
}

fn speaker_labels(
    windows: &[FinalWindow],
    live: &[Transcript],
    profile_labels: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut labels = HashMap::new();
    labels.insert("local-you".to_string(), "You".to_string());
    labels.extend(profile_labels.clone());
    for transcript in live {
        if let (Some(id), Some(label)) = (&transcript.speaker_id, &transcript.speaker_label) {
            if !label.trim().is_empty()
                && !matches!(label.as_str(), "Multiple speakers" | "Unknown speaker")
            {
                labels.insert(id.clone(), label.clone());
            }
        }
    }
    let mut next = 1;
    for window in windows {
        if let Some(id) = &window.speaker_id {
            labels.entry(id.clone()).or_insert_with(|| {
                let label = format!("Speaker {next}");
                next += 1;
                label
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
    let profiles = crate::database::repositories::speaker_profile::SpeakerProfilesRepository::list(
        state.db_manager.pool(),
    )
    .await
    .unwrap_or_default()
    .into_iter()
    .filter(|profile| {
        profile.model_version == crate::diarization::models::EMBEDDING_MODEL_VERSION
    })
    .collect::<Vec<_>>();
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
        let mut matches = profiles
            .iter()
            .map(|profile| {
                (
                    profile,
                    crate::diarization::clustering::cosine_similarity(
                        &embedding,
                        &profile.embedding,
                    ),
                )
            })
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| right.1.total_cmp(&left.1));
        if let Some((best, best_score)) = matches.first() {
            let second_score = matches.get(1).map(|entry| entry.1).unwrap_or(-1.0);
            if *best_score >= crate::diarization::clustering::PROFILE_MATCH_THRESHOLD
                && *best_score - second_score
                    >= crate::diarization::clustering::PROFILE_MATCH_MARGIN
            {
                names.insert(speaker_id.clone(), best.name.clone());
                matches_by_speaker.insert(speaker_id, best.id.clone());
            }
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
            windows
                .iter()
                .map(|window| (end.min(window.end) - start.max(window.start)).max(0.0))
                .fold(0.0, f64::max)
        })
        .sum();
    (covered / voiced).clamp(0.0, 1.0)
}

async fn load_transcripts(pool: &sqlx::SqlitePool, meeting_id: &str) -> Result<Vec<Transcript>> {
    Ok(sqlx::query_as::<_, Transcript>(
        "SELECT id, meeting_id, transcript, timestamp, summary, action_items, key_points,
                audio_start_time, audio_end_time, duration, speaker_id, speaker_label,
                speaker_profile_id, speaker_source, speaker_color, is_overlap,
                candidate_speaker_ids, diarization_status, diarization_method,
                diarization_confidence
         FROM transcripts WHERE meeting_id = ? ORDER BY audio_start_time, id",
    )
    .bind(meeting_id)
    .fetch_all(pool)
    .await?)
}

#[allow(clippy::too_many_arguments)]
async fn replace_atomically(
    pool: &sqlx::SqlitePool,
    meeting_id: &str,
    run_id: &str,
    windows: &[FinalWindow],
    transcripts: &[FinalTranscript],
    quality_flags: &[String],
    coverage: f64,
    success_rate: f64,
) -> Result<()> {
    let mut connection = pool.acquire().await?;
    let mut transaction = connection.begin().await?;
    sqlx::query("DELETE FROM speaker_segments WHERE meeting_id = ?")
        .bind(meeting_id)
        .execute(&mut *transaction)
        .await?;
    for window in windows {
        sqlx::query(
            "INSERT INTO speaker_segments (
                id, meeting_id, source, start_time, end_time, speaker_id, speaker_label,
                speaker_profile_id, confidence, is_overlap, diarization_status, diarization_method,
                candidate_speaker_ids
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'final', 'fluid_audio_offline_0.12.4', ?)",
        )
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
        .execute(&mut *transaction)
        .await?;
    }
    sqlx::query("DELETE FROM transcripts WHERE meeting_id = ?")
        .bind(meeting_id)
        .execute(&mut *transaction)
        .await?;
    for transcript in transcripts {
        sqlx::query(
            "INSERT INTO transcripts (
                id, meeting_id, transcript, timestamp, audio_start_time, audio_end_time,
                duration, speaker_id, speaker_label, speaker_source, speaker_color,
                speaker_profile_id, is_overlap, candidate_speaker_ids, diarization_status,
                diarization_method, diarization_confidence
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'final',
                       'fluid_audio_offline_0.12.4', ?)",
        )
        .bind(&transcript.id)
        .bind(meeting_id)
        .bind(&transcript.text)
        .bind(format_timestamp(transcript.start))
        .bind(transcript.start)
        .bind(transcript.end)
        .bind(transcript.end - transcript.start)
        .bind(&transcript.speaker_id)
        .bind(&transcript.speaker_label)
        .bind(&transcript.source)
        .bind(if transcript.is_overlap {
            "#f97316"
        } else {
            "#3b82f6"
        })
        .bind(&transcript.speaker_profile_id)
        .bind(transcript.is_overlap as i64)
        .bind(serde_json::to_string(&transcript.candidates)?)
        .bind(transcript.confidence)
        .execute(&mut *transaction)
        .await?;
    }
    sqlx::query(
        "UPDATE diarization_runs SET status = 'completed', progress = 100,
            assigned_coverage = ?, transcription_success_rate = ?, completed_at = CURRENT_TIMESTAMP
         WHERE id = ?",
    )
    .bind(coverage)
    .bind(success_rate)
    .bind(run_id)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "INSERT INTO meeting_diarization_status (
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
            processed_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP",
    )
    .bind(meeting_id)
    .bind(serde_json::to_string(quality_flags)?)
    .bind(run_id)
    .bind(coverage)
    .bind(success_rate)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
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
    if CANCEL_REQUESTED.load(Ordering::SeqCst) {
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(windows.len(), 2);
        assert!(windows.iter().all(|window| window.is_overlap));
    }

    #[test]
    fn final_quality_gates_are_inclusive_at_thresholds() {
        assert!(passes_quality_gates(0.80, 0.95));
        assert!(!passes_quality_gates(0.799, 1.0));
        assert!(!passes_quality_gates(1.0, 0.949));
    }
}
