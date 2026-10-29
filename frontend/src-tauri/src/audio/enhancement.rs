//! Timing and publication for Enhance. Model input chunks do not define Parakeet rows.
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{anyhow, Result};
use sqlx::{Connection, SqliteConnection, SqlitePool};

use crate::{
    api::TranscriptSegment,
    database::models::Transcript,
    diarization::finalize::{EnhancementSpeakerAnalysis, EnhancementSpeakerLabels},
    parakeet_engine::model::TimestampedResult,
};

/// Return word spans on the recording timeline. Ends are bounded estimates from
/// token emissions, not uniformly distributed word times.
fn timed_words(
    result: &TimestampedResult,
    offset_ms: f64,
    duration_ms: f64,
) -> Option<Vec<(String, f64, f64)>> {
    if result.tokens.len() != result.timestamps.len() || result.tokens.is_empty() {
        return None;
    }
    let mut words: Vec<(String, f64, f64)> = Vec::new();
    let mut previous = 0.0;
    for (token, time) in result.tokens.iter().zip(&result.timestamps) {
        let time = *time as f64 * 1000.0;
        if !time.is_finite() || time < previous || time < 0.0 || time >= duration_ms {
            return None;
        }
        previous = time;
        let boundary = token.starts_with(char::is_whitespace);
        let piece = token.trim();
        if piece.is_empty() {
            continue;
        }
        if boundary || words.is_empty() {
            words.push((
                piece.to_string(),
                offset_ms + time,
                offset_ms + (time + 160.0).min(duration_ms),
            ));
        } else {
            let last = words.last_mut().unwrap();
            last.0.push_str(piece);
            last.2 = offset_ms + (time + 160.0).min(duration_ms);
        }
    }
    for i in 0..words.len().saturating_sub(1) {
        // Two short words may share a quantized token time. Keep a positive span.
        words[i].2 = words[i].2.min(words[i + 1].1.max(words[i].1 + 40.0));
    }
    let reconstructed = words
        .iter()
        .map(|word| word.0.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    (reconstructed == result.text.split_whitespace().collect::<Vec<_>>().join(" ")
        && !words.is_empty())
    .then_some(words)
}

pub(crate) fn parakeet_rows(
    result: &TimestampedResult,
    offset_ms: f64,
    duration_ms: f64,
    speakers: Option<&EnhancementSpeakerAnalysis>,
) -> Result<Vec<(String, f64, f64)>> {
    let words = timed_words(result, offset_ms, duration_ms).ok_or_else(|| {
        anyhow!(
            "The transcription model returned invalid token times. The saved transcript was kept."
        )
    })?;
    Ok(group_words(words, |start, end| {
        speakers.and_then(|analysis| analysis.speaker_key(start / 1000.0, end / 1000.0))
    }))
}

fn group_words(
    words: Vec<(String, f64, f64)>,
    speaker_key: impl Fn(f64, f64) -> Option<String>,
) -> Vec<(String, f64, f64)> {
    let mut rows: Vec<(String, f64, f64)> = Vec::new();
    let mut current_key = None;
    for (text, start, end) in words {
        let key = speaker_key(start, end);
        let split = rows
            .last()
            .map(|row| {
                key != current_key
                    || start - row.2 > 600.0
                    || end - row.1 > 6000.0
                    || row.0.ends_with(['.', '!', '?', '。', '！', '？'])
            })
            .unwrap_or(true);
        if split {
            rows.push((text, start, end));
        } else {
            let row = rows.last_mut().unwrap();
            row.0.push(' ');
            row.0.push_str(&text);
            row.2 = row.2.max(end);
        }
        current_key = key;
    }
    rows
}

/// The mixed recording is the fallback. Retained capture tracks stay separate
/// for Whisper so one input cannot mask speech on the other input.
pub(crate) struct EnhancementAudio {
    pub mixed: super::decoder::DecodedAudio,
    pub tracks: Vec<(String, super::decoder::DecodedAudio)>,
}

impl std::ops::Deref for EnhancementAudio {
    type Target = super::decoder::DecodedAudio;
    fn deref(&self) -> &Self::Target {
        &self.mixed
    }
}

#[derive(Clone)]
pub(crate) struct RecognitionInput {
    pub segment: super::vad::SpeechSegment,
    pub source: Option<String>,
}

impl std::ops::Deref for RecognitionInput {
    type Target = super::vad::SpeechSegment;
    fn deref(&self) -> &Self::Target {
        &self.segment
    }
}

impl RecognitionInput {
    pub(crate) fn mixed(segment: super::vad::SpeechSegment) -> Self {
        Self {
            segment,
            source: None,
        }
    }
}

/// Prefer the retained lossless capture when it still matches the recording.
/// Both tracks are required so choosing source audio cannot omit one side.
pub(crate) fn decode_enhancement_audio(path: &std::path::Path) -> Result<EnhancementAudio> {
    let primary = super::decoder::decode_audio_file(path)?;
    if !path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ["mp4", "m4a", "aac"].contains(&e.to_ascii_lowercase().as_str()))
    {
        return Ok(EnhancementAudio {
            mixed: primary,
            tracks: Vec::new(),
        });
    }
    let Some(folder) = path.parent() else {
        return Ok(EnhancementAudio {
            mixed: primary,
            tracks: Vec::new(),
        });
    };
    let microphone = folder.join(".diarization/microphone.flac");
    let system = folder.join(".diarization/system.flac");
    if !microphone.is_file() || !system.is_file() {
        return Ok(EnhancementAudio {
            mixed: primary,
            tracks: Vec::new(),
        });
    }
    let sources = (|| -> Result<EnhancementAudio> {
        let primary_time = std::fs::metadata(path)?.modified()?;
        for source in [&microphone, &system] {
            if primary_time
                .duration_since(std::fs::metadata(source)?.modified()?)
                .unwrap_or_default()
                > std::time::Duration::from_secs(5)
            {
                return Err(anyhow!(
                    "the recording changed after the retained source audio"
                ));
            }
        }
        let mic = super::decoder::decode_audio_file(&microphone)?;
        let sys = super::decoder::decode_audio_file(&system)?;
        if mic.sample_rate != 16000
            || sys.sample_rate != 16000
            || mic.channels != 1
            || sys.channels != 1
            || mic.samples.len() != sys.samples.len()
            || mic.samples.is_empty()
            || (mic.duration_seconds - primary.duration_seconds).abs() > 0.1
        {
            return Err(anyhow!("retained source tracks do not match the recording"));
        }
        let samples = mic
            .samples
            .iter()
            .zip(&sys.samples)
            .map(|(mic, sys)| (mic + sys).clamp(-1.0, 1.0))
            .collect();
        Ok(EnhancementAudio {
            mixed: super::decoder::DecodedAudio {
                samples,
                sample_rate: 16000,
                channels: 1,
                duration_seconds: mic.duration_seconds,
            },
            tracks: vec![("microphone".into(), mic), ("system".into(), sys)],
        })
    })();
    match sources {
        Ok(source) => {
            log::info!("Enhance is using the retained lossless microphone and system tracks");
            Ok(source)
        }
        Err(error) => {
            log::warn!("Enhance kept the mixed recording because {error}");
            Ok(EnhancementAudio {
                mixed: primary,
                tracks: Vec::new(),
            })
        }
    }
}

/// Reject very weak pickup only when its volume pattern follows the other
/// capture track. Level alone cannot distinguish quiet speech from pickup.
fn weak_cross_track_pickup(
    segment: &super::vad::SpeechSegment,
    other: &super::decoder::DecodedAudio,
) -> bool {
    const FRAME: usize = 160; // 10 ms at 16 kHz
    let from = (segment.start_timestamp_ms * 16.0).round() as usize;
    let to = (segment.end_timestamp_ms * 16.0).round() as usize;
    let Some(reference) = other.samples.get(from..to) else {
        return false;
    };
    let count = segment.samples.len().min(reference.len());
    if count < 16000 {
        return false;
    }
    let rms = |samples: &[f32]| {
        (samples.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
    };
    let level = rms(&segment.samples[..count]);
    if level >= 0.01 || level <= 0.0 || rms(&reference[..count]) < level * 10.0 {
        return false;
    }
    let envelope = |samples: &[f32]| samples.chunks_exact(FRAME).map(rms).collect::<Vec<_>>();
    let local = envelope(&segment.samples[..count]);
    let remote = envelope(&reference[..count]);
    for lag in -50i32..=50 {
        // Allow capture and playback delay up to 500 ms.
        let a = lag.max(0) as usize;
        let b = (-lag).max(0) as usize;
        let n = local
            .len()
            .saturating_sub(a)
            .min(remote.len().saturating_sub(b));
        if n < 100 {
            continue;
        }
        let mut x = 0.0;
        let mut y = 0.0;
        let mut xx = 0.0;
        let mut yy = 0.0;
        let mut xy = 0.0;
        for i in 0..n {
            let u = local[a + i];
            let v = remote[b + i];
            x += u;
            y += v;
            xx += u * u;
            yy += v * v;
            xy += u * v;
        }
        let n = n as f64;
        let variance = (xx - x * x / n) * (yy - y * y / n);
        if variance > 1e-20 && (xy - x * y / n) / variance.sqrt() >= 0.65 {
            return true;
        }
    }
    false
}

/// Detect speech on each retained input. A quiet microphone must not change
/// the words decoded from the system input. Keep both inputs on one timeline.
pub(crate) fn whisper_recognition_inputs(
    audio: &EnhancementAudio,
    pause_ms: u32,
    known_speech: &[Transcript],
    mut progress: impl FnMut(u32, usize) -> bool,
) -> Result<Vec<RecognitionInput>> {
    if audio.tracks.is_empty() {
        return Ok(
            whisper_speech_segments(&audio.mixed, pause_ms, known_speech, progress)?
                .into_iter()
                .map(RecognitionInput::mixed)
                .collect(),
        );
    }
    let mut inputs = Vec::new();
    for (index, (source, track)) in audio.tracks.iter().enumerate() {
        let spans = whisper_speech_segments(track, pause_ms, &[], |percent, count| {
            progress(
                (index as u32 * 100 + percent) / audio.tracks.len() as u32,
                count,
            )
        })?;
        let other = &audio.tracks[1 - index].1;
        inputs.extend(
            spans
                .into_iter()
                .filter(|segment| !weak_cross_track_pickup(segment, other))
                .map(|segment| RecognitionInput {
                    segment,
                    source: Some(source.clone()),
                }),
        );
    }
    // Quiet speech missed on both inputs still gets a recognition pass. Do not
    // copy all baseline regions onto both tracks: this would decode silence.
    let mixed = audio.mixed.to_whisper_format();
    let mut missed = Vec::new();
    for row in known_speech {
        let (Some(start), Some(end)) = (row.audio_start_time, row.audio_end_time) else {
            continue;
        };
        if !start.is_finite()
            || !end.is_finite()
            || start < 0.0
            || end <= start
            || !row.transcript.chars().any(char::is_alphanumeric)
            || inputs.iter().any(|input| {
                input.start_timestamp_ms < end * 1000.0 && input.end_timestamp_ms > start * 1000.0
            })
        {
            continue;
        }
        let from = ((start - 1.0).max(0.0) * 16000.0).round() as usize;
        let to = (((end + 1.0) * 16000.0).round() as usize).min(mixed.len());
        if let Some(samples) = mixed.get(from..to).filter(|samples| !samples.is_empty()) {
            missed.push(super::vad::SpeechSegment {
                samples: samples.to_vec(),
                start_timestamp_ms: from as f64 / 16.0,
                end_timestamp_ms: to as f64 / 16.0,
                confidence: 0.0,
            });
        }
    }
    inputs.extend(
        speech_context(&missed, &mixed)
            .into_iter()
            .map(RecognitionInput::mixed),
    );
    inputs.sort_by(|a, b| {
        a.start_timestamp_ms
            .total_cmp(&b.start_timestamp_ms)
            .then(a.source.cmp(&b.source))
    });
    Ok(inputs)
}

/// Reuse live conversion and frame boundaries for saved Whisper audio.
/// Keep the converted waveform so joined spans retain their original pauses.
pub(crate) fn whisper_speech_segments(
    decoded: &super::decoder::DecodedAudio,
    pause_ms: u32,
    known_speech: &[Transcript],
    mut progress: impl FnMut(u32, usize) -> bool,
) -> Result<Vec<super::vad::SpeechSegment>> {
    let mut mono = if decoded.channels > 1 {
        super::audio_processing::audio_to_mono(&decoded.samples, decoded.channels)
    } else {
        decoded.samples.clone()
    };
    // The live mixer bounds samples before VAD. Decoded float audio can
    // overshoot slightly or contain invalid values, so use the same bounds.
    for sample in &mut mono {
        *sample = if sample.is_finite() {
            sample.clamp(-1.0, 1.0)
        } else {
            0.0
        };
    }
    let mut processor = super::vad::ContinuousVadProcessor::new(decoded.sample_rate, pause_ms)?;
    let frame = (decoded.sample_rate as usize * super::constants::MIXING_WINDOW_MS / 1000).max(1);
    let count = mono.len().div_ceil(frame).max(1);
    let mut converted = Vec::new();
    let mut spans = Vec::new();
    for (index, window) in mono.chunks(frame).enumerate() {
        spans.extend(
            processor.process_audio_with_observer(window, |samples| {
                converted.extend_from_slice(samples)
            })?,
        );
        if (index % 100 == 0 || index + 1 == count)
            && !progress(((index + 1) * 100 / count) as u32, spans.len())
        {
            return Err(anyhow!(
                "Enhancement cancelled. The saved transcript was kept."
            ));
        }
    }
    spans.extend(processor.flush()?);
    for row in known_speech {
        let (Some(start), Some(end)) = (row.audio_start_time, row.audio_end_time) else {
            continue;
        };
        if !start.is_finite()
            || !end.is_finite()
            || start < 0.0
            || end <= start
            || !row.transcript.chars().any(char::is_alphanumeric)
        {
            continue;
        }
        if spans.iter().any(|span| {
            span.start_timestamp_ms < end * 1000.0 && span.end_timestamp_ms > start * 1000.0
        }) {
            continue;
        }
        // A live transcription is evidence that this region needs another
        // recognition pass, even if saved-audio VAD did not detect it.
        let from = ((start - 1.0).max(0.0) * 16000.0).round() as usize;
        let to = (((end + 1.0) * 16000.0).round() as usize).min(converted.len());
        if let Some(samples) = converted.get(from..to).filter(|s| !s.is_empty()) {
            spans.push(super::vad::SpeechSegment {
                samples: samples.to_vec(),
                start_timestamp_ms: from as f64 / 16.0,
                end_timestamp_ms: to as f64 / 16.0,
                confidence: 0.0,
            });
        }
    }
    spans.sort_by(|a, b| a.start_timestamp_ms.total_cmp(&b.start_timestamp_ms));
    let mut distinct: Vec<super::vad::SpeechSegment> = Vec::new();
    for span in spans {
        if let Some(previous) = distinct.last_mut() {
            if span.start_timestamp_ms <= previous.end_timestamp_ms {
                let end = previous.end_timestamp_ms.max(span.end_timestamp_ms);
                let from = (previous.start_timestamp_ms * 16.0).round() as usize;
                let to = ((end * 16.0).round() as usize).min(converted.len());
                previous.samples = converted[from..to].to_vec();
                previous.end_timestamp_ms = end;
                continue;
            }
        }
        distinct.push(span);
    }
    Ok(speech_context(&distinct, &converted))
}

/// Give short Whisper inputs nearby speech context. Keep the original audio
/// between spans, including pauses; do not concatenate isolated speech bursts.
pub(crate) fn speech_context(
    segments: &[super::vad::SpeechSegment],
    audio: &[f32],
) -> Vec<super::vad::SpeechSegment> {
    const MIN_CONTEXT_MS: f64 = 8_000.0;
    const MAX_GAP_MS: f64 = 4_000.0;
    let mut grouped: Vec<super::vad::SpeechSegment> = Vec::new();
    for segment in segments {
        if let Some(previous) = grouped.last_mut() {
            if previous.end_timestamp_ms - previous.start_timestamp_ms < MIN_CONTEXT_MS
                && segment.start_timestamp_ms - previous.end_timestamp_ms <= MAX_GAP_MS
            {
                let start = (previous.start_timestamp_ms.max(0.0) * 16.0).round() as usize;
                let end = (segment.end_timestamp_ms.max(0.0) * 16.0).round() as usize;
                if let Some(samples) = audio.get(start..end) {
                    previous.samples = samples.to_vec();
                    previous.end_timestamp_ms = segment.end_timestamp_ms;
                    previous.confidence = previous.confidence.min(segment.confidence);
                    continue;
                }
            }
        }
        grouped.push(segment.clone());
    }
    grouped
}

/// Bound long input at quiet points, independently of speaker estimates.
/// Remove any context overlap so Enhance cannot repeat words.
pub(crate) fn input_chunks(
    segment: &super::vad::SpeechSegment,
    max_samples: usize,
) -> Vec<super::vad::SpeechSegment> {
    let duration = segment.end_timestamp_ms - segment.start_timestamp_ms;
    if segment.samples.is_empty() || duration <= 0.0 {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut cursor = segment.start_timestamp_ms;
    for mut chunk in super::common::split_segment_at_silence(segment, max_samples) {
        if chunk.start_timestamp_ms < cursor {
            let trim = ((cursor - chunk.start_timestamp_ms)
                / (chunk.end_timestamp_ms - chunk.start_timestamp_ms)
                * chunk.samples.len() as f64)
                .round() as usize;
            chunk.samples.drain(..trim.min(chunk.samples.len()));
            chunk.start_timestamp_ms = cursor;
        }
        cursor = chunk.end_timestamp_ms;
        if !chunk.samples.is_empty() {
            chunks.push(chunk);
        }
    }
    chunks
}

pub(crate) fn transcript_rows(meeting_id: &str, segments: &[TranscriptSegment]) -> Vec<Transcript> {
    segments
        .iter()
        .map(|segment| Transcript {
            id: segment.id.clone(),
            meeting_id: meeting_id.to_string(),
            transcript: segment.text.clone(),
            timestamp: segment.timestamp.clone(),
            summary: None,
            action_items: None,
            key_points: None,
            audio_start_time: segment.audio_start_time,
            audio_end_time: segment.audio_end_time,
            duration: segment.duration,
        })
        .collect()
}

// Separate WAL databases cannot share a crash-atomic commit. Persist the
// publication intent first, then commit core rows, then speaker data. Startup
// resolves the intent before it serves transcript data. FULL synchronization
// makes each step durable before the next database is changed.
const CORE_ROWS: &str = "SELECT id, meeting_id, transcript, timestamp, summary, action_items, key_points,
    audio_start_time, audio_end_time, duration FROM transcripts WHERE meeting_id = ? ORDER BY audio_start_time, id";

async fn durable_connection(pool: &SqlitePool) -> Result<SqliteConnection> {
    Ok(SqliteConnection::connect_with(
        &pool
            .connect_options()
            .as_ref()
            .clone()
            .synchronous(sqlx::sqlite::SqliteSynchronous::Full),
    )
    .await?)
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
enum SpeakerChange {
    Undo {
        restore_speakers: String,
        restore_core: String,
        undo_id: String,
    },
    Labels(EnhancementSpeakerLabels),
}

async fn write_speakers(
    connection: &mut SqliteConnection,
    meeting_id: &str,
    change: Option<&SpeakerChange>,
    proposed_rows: &str,
) -> Result<()> {
    if let Some(SpeakerChange::Undo {
        restore_speakers,
        undo_id,
        ..
    }) = change
    {
        super::enhancement_review::restore_speakers(connection, meeting_id, restore_speakers)
            .await?;
        sqlx::query("UPDATE enhancement_reviews SET state='undone' WHERE id=? AND meeting_id=?")
            .bind(undo_id)
            .bind(meeting_id)
            .execute(&mut *connection)
            .await?;
    } else if let Some(SpeakerChange::Labels(labels)) = change {
        labels.write(connection, meeting_id).await?;
    } else {
        for table in ["transcript_speakers", "speaker_segments"] {
            sqlx::query(&format!("DELETE FROM {table} WHERE meeting_id = ?"))
                .bind(meeting_id)
                .execute(&mut *connection)
                .await?;
        }
        sqlx::query("INSERT INTO meeting_diarization_status (meeting_id, current_status, progress)
            VALUES (?, 'none', 0) ON CONFLICT(meeting_id) DO UPDATE SET current_status = 'none', progress = 0,
            failure_reason = NULL, run_id = NULL, assigned_coverage = NULL, transcription_success_rate = NULL,
            updated_at = CURRENT_TIMESTAMP").bind(meeting_id).execute(&mut *connection).await?;
    }
    let snapshot = super::enhancement_review::speaker_snapshot_on(connection, meeting_id).await?;
    sqlx::query("UPDATE enhancement_reviews SET state = 'applied', applied_speakers = ? WHERE meeting_id = ? AND state = 'pending' AND json_extract(payload, '$.publication_rows') = ?")
        .bind(snapshot).bind(meeting_id).bind(proposed_rows).execute(&mut *connection).await?;
    sqlx::query("DELETE FROM enhancement_publications WHERE meeting_id = ?")
        .bind(meeting_id)
        .execute(connection)
        .await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct Publication {
    meeting_id: String,
    original_rows: String,
    proposed_rows: String,
    core_backup: String,
    speaker_labels: Option<String>,
}

async fn remove_intent(feature: &SqlitePool, meeting_id: &str) -> Result<()> {
    let mut connection = durable_connection(feature).await?;
    sqlx::query("DELETE FROM enhancement_publications WHERE meeting_id = ?")
        .bind(meeting_id)
        .execute(&mut connection)
        .await?;
    Ok(())
}

pub(crate) async fn reconcile_publications(core: &SqlitePool, feature: &SqlitePool) -> Result<()> {
    let pending: Vec<Publication> = sqlx::query_as("SELECT * FROM enhancement_publications")
        .fetch_all(feature)
        .await?;
    for publication in pending {
        let meeting_exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM meetings WHERE id = ?)")
                .bind(&publication.meeting_id)
                .fetch_one(core)
                .await?;
        if !meeting_exists {
            // Deletion is authoritative, including a stop between core
            // deletion and feature-store cleanup. Never recreate the meeting.
            remove_intent(feature, &publication.meeting_id).await?;
            continue;
        }
        let actual: Vec<Transcript> = sqlx::query_as(CORE_ROWS)
            .bind(&publication.meeting_id)
            .fetch_all(core)
            .await?;
        let actual = serde_json::to_string(&actual)?;
        if actual == publication.original_rows {
            // The core write never committed. Old speaker data is untouched.
            remove_intent(feature, &publication.meeting_id).await?;
        } else if actual == publication.proposed_rows {
            let labels = publication
                .speaker_labels
                .as_deref()
                .map(serde_json::from_str::<SpeakerChange>)
                .transpose()?;
            let mut connection = durable_connection(feature).await?;
            let mut tx = connection.begin().await?;
            write_speakers(
                &mut tx,
                &publication.meeting_id,
                labels.as_ref(),
                &publication.proposed_rows,
            )
            .await?;
            tx.commit().await?;
        } else {
            log::warn!("Enhance recovery could not match the saved transcript for {}; current transcript retained and speaker review required", publication.meeting_id);
            let mut connection = durable_connection(feature).await?;
            let mut tx = connection.begin().await?;
            sqlx::query("INSERT INTO meeting_diarization_status (meeting_id, current_status, progress, failure_reason)
                VALUES (?, 'needs_review', 0, ?)
                ON CONFLICT(meeting_id) DO UPDATE SET current_status = 'needs_review', progress = 0,
                    failure_reason = excluded.failure_reason, assigned_coverage = NULL,
                    transcription_success_rate = NULL, updated_at = CURRENT_TIMESTAMP")
                .bind(&publication.meeting_id)
                .bind("Enhance recovery could not match the saved transcript. The current transcript was kept. Review the speaker labels or run speaker identification again.")
                .execute(&mut *tx).await?;
            sqlx::query("DELETE FROM enhancement_publications WHERE meeting_id = ?")
                .bind(&publication.meeting_id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
        }
    }
    Ok(())
}

async fn restore_core(core: &SqlitePool, publication: &Publication) -> Result<()> {
    let mut connection = durable_connection(core).await?;
    let mut tx = connection.begin().await?;
    let actual: Vec<Transcript> = sqlx::query_as(CORE_ROWS)
        .bind(&publication.meeting_id)
        .fetch_all(&mut *tx)
        .await?;
    if serde_json::to_string(&actual)? != publication.proposed_rows {
        return Err(anyhow!(
            "The transcript changed before Enhance recovery. The recovery record was kept."
        ));
    }
    sqlx::query("DELETE FROM transcripts WHERE meeting_id = ?")
        .bind(&publication.meeting_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO transcripts (id, meeting_id, transcript, timestamp, summary, action_items, key_points,
            audio_start_time, audio_end_time, duration, speaker)
        SELECT json_extract(value, '$.id'), json_extract(value, '$.meeting_id'), json_extract(value, '$.transcript'),
            json_extract(value, '$.timestamp'), json_extract(value, '$.summary'), json_extract(value, '$.action_items'),
            json_extract(value, '$.key_points'), json_extract(value, '$.audio_start_time'), json_extract(value, '$.audio_end_time'),
            json_extract(value, '$.duration'), json_extract(value, '$.speaker') FROM json_each(?)")
        .bind(&publication.core_backup).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn publish(
    core: &SqlitePool,
    feature: &SqlitePool,
    meeting_id: &str,
    original: &[Transcript],
    segments: &[TranscriptSegment],
    labels: Option<&EnhancementSpeakerLabels>,
    cancelled: &AtomicBool,
) -> Result<()> {
    publish_change(
        core,
        feature,
        meeting_id,
        original,
        segments,
        labels.map(serde_json::to_string).transpose()?,
        cancelled,
    )
    .await
}

pub(crate) async fn publish_undo(
    core: &SqlitePool,
    feature: &SqlitePool,
    meeting_id: &str,
    original: &[Transcript],
    proposal: &super::enhancement_review::Proposal,
    id: &str,
) -> Result<()> {
    let change = SpeakerChange::Undo {
        restore_speakers: proposal.speaker_backup.clone(),
        restore_core: serde_json::to_string(&proposal.core_backup)?,
        undo_id: id.into(),
    };
    publish_change(
        core,
        feature,
        meeting_id,
        original,
        &super::enhancement_review::original_segments(proposal)?,
        Some(serde_json::to_string(&change)?),
        &AtomicBool::new(false),
    )
    .await
}

async fn publish_change(
    core: &SqlitePool,
    feature: &SqlitePool,
    meeting_id: &str,
    original: &[Transcript],
    segments: &[TranscriptSegment],
    speaker_change: Option<String>,
    cancelled: &AtomicBool,
) -> Result<()> {
    let change = speaker_change
        .as_deref()
        .map(serde_json::from_str::<SpeakerChange>)
        .transpose()?;
    if segments.is_empty() && !matches!(change, Some(SpeakerChange::Undo { .. })) {
        return Err(anyhow!(
            "No transcript text was found. The saved transcript was kept."
        ));
    }
    let mut core_connection = durable_connection(core).await?;
    let mut core_tx = core_connection.begin().await?;
    let current: Vec<Transcript> = sqlx::query_as(CORE_ROWS)
        .bind(meeting_id)
        .fetch_all(&mut *core_tx)
        .await?;
    if serde_json::to_string(&current)? != serde_json::to_string(original)? {
        return Err(anyhow!(
            "The transcript changed during enhancement. The saved transcript was kept."
        ));
    }
    let backup: Vec<crate::diarization::finalize::CoreTranscriptBackup> = sqlx::query_as(
        "SELECT id, meeting_id, transcript, timestamp, summary, action_items, key_points,
        audio_start_time, audio_end_time, duration, speaker FROM transcripts WHERE meeting_id = ? ORDER BY audio_start_time, id",
    ).bind(meeting_id).fetch_all(&mut *core_tx).await?;
    let mut proposed: Vec<Transcript> =
        if let Some(SpeakerChange::Undo { restore_core, .. }) = &change {
            serde_json::from_str(restore_core)?
        } else {
            transcript_rows(meeting_id, segments)
        };
    proposed.sort_by(|a, b| {
        a.audio_start_time
            .partial_cmp(&b.audio_start_time)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });
    let publication = Publication {
        meeting_id: meeting_id.into(),
        original_rows: serde_json::to_string(original)?,
        proposed_rows: serde_json::to_string(&proposed)?,
        core_backup: serde_json::to_string(&backup)?,
        speaker_labels: speaker_change,
    };
    let mut feature_connection = durable_connection(feature).await?;
    sqlx::query("INSERT INTO enhancement_publications (meeting_id, original_rows, proposed_rows, core_backup, speaker_labels)
        VALUES (?, ?, ?, ?, ?)")
        .bind(meeting_id).bind(&publication.original_rows).bind(&publication.proposed_rows)
        .bind(&publication.core_backup).bind(&publication.speaker_labels)
        .execute(&mut feature_connection).await?;
    let mut feature_tx = feature_connection.begin().await?;
    let prepared: Result<()> = async {
        // Validate and prepare speaker writes before changing the transcript.
        // These writes and removal of the recovery record commit together.
        write_speakers(&mut feature_tx, meeting_id, change.as_ref(), &publication.proposed_rows).await?;
        sqlx::query("DELETE FROM transcripts WHERE meeting_id = ?")
            .bind(meeting_id).execute(&mut *core_tx).await?;
        if let Some(SpeakerChange::Undo{restore_core,..})=&change {
            sqlx::query("INSERT INTO transcripts (id,meeting_id,transcript,timestamp,summary,action_items,key_points,audio_start_time,audio_end_time,duration,speaker)
                SELECT json_extract(value,'$.id'),json_extract(value,'$.meeting_id'),json_extract(value,'$.transcript'),json_extract(value,'$.timestamp'),
                json_extract(value,'$.summary'),json_extract(value,'$.action_items'),json_extract(value,'$.key_points'),json_extract(value,'$.audio_start_time'),
                json_extract(value,'$.audio_end_time'),json_extract(value,'$.duration'),json_extract(value,'$.speaker') FROM json_each(?)")
                .bind(restore_core).execute(&mut *core_tx).await?;
        } else {
        for segment in segments {
            sqlx::query("INSERT INTO transcripts (id, meeting_id, transcript, timestamp, audio_start_time, audio_end_time, duration)
                VALUES (?, ?, ?, ?, ?, ?, ?)").bind(&segment.id).bind(meeting_id).bind(&segment.text).bind(&segment.timestamp)
                .bind(segment.audio_start_time).bind(segment.audio_end_time).bind(segment.duration).execute(&mut *core_tx).await?;
        }
        }
        if cancelled.load(Ordering::SeqCst) {
            return Err(anyhow!("Enhancement cancelled. The saved transcript was kept."));
        }
        Ok(())
    }.await;
    if let Err(error) = prepared {
        feature_tx.rollback().await?;
        core_tx.rollback().await?;
        remove_intent(feature, meeting_id).await?;
        return Err(error);
    }
    if let Err(error) = core_tx.commit().await {
        feature_tx.rollback().await?;
        // A failed COMMIT can be ambiguous. Resolve from the durable rows.
        reconcile_publications(core, feature).await?;
        let actual = crate::diarization::finalize::load_transcripts(core, meeting_id).await?;
        if serde_json::to_string(&actual)? == publication.proposed_rows {
            return Ok(());
        }
        return Err(error.into());
    }
    if let Err(error) = feature_tx.commit().await {
        let pending: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM enhancement_publications WHERE meeting_id = ?)",
        )
        .bind(meeting_id)
        .fetch_one(feature)
        .await?;
        if !pending {
            return Ok(());
        }
        restore_core(core, &publication).await?;
        remove_intent(feature, meeting_id).await?;
        return Err(error.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::vad::SpeechSegment;

    fn speech_segment(start_ms: f64, end_ms: f64, samples: usize) -> SpeechSegment {
        SpeechSegment {
            samples: vec![0.0; samples],
            start_timestamp_ms: start_ms,
            end_timestamp_ms: end_ms,
            confidence: 0.75,
        }
    }

    fn timed(text: &str, tokens: &[&str], times: &[f32]) -> TimestampedResult {
        TimestampedResult {
            text: text.to_string(),
            tokens: tokens.iter().map(|token| token.to_string()).collect(),
            timestamps: times.to_vec(),
        }
    }

    #[test]
    fn timed_words_build_spans_from_token_times() {
        // 0.125 seconds stays exact through the f32 token times.
        let result = timed("Hello world", &[" Hello", " world"], &[0.0, 0.125]);
        let words = timed_words(&result, 1000.0, 2000.0).expect("valid token times");
        assert_eq!(
            words,
            vec![
                ("Hello".to_string(), 1000.0, 1125.0),
                ("world".to_string(), 1125.0, 1285.0),
            ]
        );
    }

    #[test]
    fn timed_words_reject_unusable_token_times() {
        let mismatch = timed("one", &["one", " two"], &[0.0]);
        assert!(timed_words(&mismatch, 0.0, 1000.0).is_none());

        let empty = timed("", &[], &[]);
        assert!(timed_words(&empty, 0.0, 1000.0).is_none());

        let at_end = timed("one", &["one"], &[0.1]);
        assert!(timed_words(&at_end, 0.0, 100.0).is_none());

        let backwards = timed("one two", &["one", " two"], &[0.2, 0.1]);
        assert!(timed_words(&backwards, 0.0, 1000.0).is_none());
    }

    #[test]
    fn timed_words_require_tokens_to_rebuild_the_text() {
        let result = timed("hello there", &["hello", " world"], &[0.0, 0.1]);
        assert!(timed_words(&result, 0.0, 1000.0).is_none());
    }

    #[test]
    fn parakeet_rows_reject_unusable_token_times() {
        let result = timed("hello", &["hello"], &[]);
        let error = parakeet_rows(&result, 0.0, 1000.0, None).unwrap_err();
        assert!(error.to_string().contains("invalid token times"));
    }

    #[test]
    fn group_words_merge_sentences_without_breaks() {
        let words = vec![
            ("one".to_string(), 0.0, 300.0),
            ("two".to_string(), 400.0, 700.0),
        ];
        let rows = group_words(words, |_, _| Some("speaker-1".to_string()));
        assert_eq!(rows, vec![("one two".to_string(), 0.0, 700.0)]);
    }

    #[test]
    fn group_words_split_on_speaker_change() {
        let words = vec![
            ("first".to_string(), 0.0, 300.0),
            ("turn".to_string(), 400.0, 700.0),
            ("second".to_string(), 800.0, 1100.0),
        ];
        let rows = group_words(words, |start, _| {
            Some(if start < 500.0 { "S1" } else { "S2" }.to_string())
        });
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "first turn");
        assert_eq!(rows[1].0, "second");
    }

    #[test]
    fn group_words_split_on_long_pause() {
        let words = vec![
            ("before".to_string(), 0.0, 300.0),
            ("after".to_string(), 1000.0, 1300.0),
        ];
        let rows = group_words(words, |_, _| None);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn group_words_cap_row_duration() {
        let words = vec![
            ("start".to_string(), 0.0, 100.0),
            ("late".to_string(), 6000.0, 6100.0),
        ];
        let rows = group_words(words, |_, _| None);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn group_words_split_after_sentence_end() {
        let words = vec![
            ("Done.".to_string(), 0.0, 300.0),
            ("Next".to_string(), 400.0, 700.0),
        ];
        let rows = group_words(words, |_, _| None);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "Done.");
        assert_eq!(rows[1].0, "Next");
    }

    fn lossless_source_fixture() -> tempfile::TempDir {
        let folder = tempfile::tempdir().unwrap();
        let stems = folder.path().join(".diarization");
        std::fs::create_dir(&stems).unwrap();
        let tone: Vec<f32> = (0..48000).map(|i| (i as f32 * 0.03).sin() * 0.1).collect();
        super::super::encode::encode_single_audio(
            bytemuck::cast_slice(&tone),
            48000,
            1,
            &folder.path().join("audio.mp4"),
        )
        .unwrap();
        let ffmpeg = super::super::ffmpeg::find_ffmpeg_path().unwrap();
        for (name, frequency) in [("microphone", 300), ("system", 900)] {
            let output = std::process::Command::new(&ffmpeg)
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &format!("sine=frequency={frequency}:sample_rate=16000:duration=1"),
                    "-c:a",
                    "flac",
                ])
                .arg(stems.join(format!("{name}.flac")))
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        folder
    }

    #[test]
    fn enhancement_uses_complete_lossless_tracks_without_aac_round_trip() {
        let folder = lossless_source_fixture();
        let decoded = decode_enhancement_audio(&folder.path().join("audio.mp4")).unwrap();
        let microphone = super::super::decoder::decode_audio_file(
            &folder.path().join(".diarization/microphone.flac"),
        )
        .unwrap();
        let system = super::super::decoder::decode_audio_file(
            &folder.path().join(".diarization/system.flac"),
        )
        .unwrap();
        let expected: Vec<f32> = microphone
            .samples
            .iter()
            .zip(&system.samples)
            .map(|(a, b)| (a + b).clamp(-1.0, 1.0))
            .collect();
        assert_eq!(decoded.sample_rate, 16000);
        assert_eq!(decoded.tracks.len(), 2);
        assert_eq!(decoded.tracks[0].0, "microphone");
        assert_eq!(decoded.tracks[0].1.samples, microphone.samples);
        assert_eq!(decoded.tracks[1].0, "system");
        assert_eq!(decoded.tracks[1].1.samples, system.samples);
        assert_eq!(
            decoded.samples, expected,
            "both capture tracks must be kept, without another codec pass"
        );
    }

    #[test]
    fn enhancement_keeps_primary_audio_when_source_tracks_are_missing_stale_or_incomplete() {
        for problem in ["missing", "stale", "duration"] {
            let folder = lossless_source_fixture();
            let primary = folder.path().join("audio.mp4");
            if problem == "missing" {
                std::fs::remove_file(folder.path().join(".diarization/system.flac")).unwrap();
            } else if problem == "stale" {
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(&primary)
                    .unwrap()
                    .set_times(std::fs::FileTimes::new().set_modified(
                        std::time::SystemTime::now() + std::time::Duration::from_secs(60),
                    ))
                    .unwrap();
            } else {
                let replacement =
                    std::process::Command::new(super::super::ffmpeg::find_ffmpeg_path().unwrap())
                        .args([
                            "-v",
                            "error",
                            "-f",
                            "lavfi",
                            "-i",
                            "sine=sample_rate=16000:duration=2",
                            "-c:a",
                            "flac",
                            "-y",
                        ])
                        .arg(folder.path().join(".diarization/system.flac"))
                        .output()
                        .unwrap();
                assert!(replacement.status.success());
            }
            let expected = super::super::decoder::decode_audio_file(&primary).unwrap();
            let actual = decode_enhancement_audio(&primary).unwrap();
            assert!(
                actual.tracks.is_empty(),
                "{problem}: do not use incomplete capture sources"
            );
            assert_eq!(actual.sample_rate, expected.sample_rate, "{problem}");
            assert_eq!(
                actual.samples, expected.samples,
                "{problem}: fallback must keep the complete recording"
            );
        }
    }

    #[test]
    fn saved_whisper_input_bounds_invalid_float_samples_before_vad() {
        let decoded = super::super::decoder::DecodedAudio {
            samples: vec![f32::NAN, f32::INFINITY, -2.0, 2.0, 0.0].repeat(1000),
            sample_rate: 48000,
            channels: 1,
            duration_seconds: 5000.0 / 48000.0,
        };
        assert!(whisper_speech_segments(&decoded, 500, &[], |_, _| true).is_ok());
        assert!(whisper_speech_segments(&decoded, 500, &[], |_, _| false)
            .unwrap_err()
            .to_string()
            .contains("cancelled"));
    }

    #[test]
    fn quiet_baseline_speech_is_sent_back_to_recognition() {
        let decoded = super::super::decoder::DecodedAudio {
            samples: vec![0.0; 10 * 16000],
            sample_rate: 16000,
            channels: 1,
            duration_seconds: 10.0,
        };
        let baseline:Transcript=serde_json::from_value(serde_json::json!({"id":"old","meeting_id":"m1","transcript":"Quiet speech","timestamp":"old","audio_start_time":4.0,"audio_end_time":5.0,"duration":1.0})).unwrap();
        let chunks = whisper_speech_segments(&decoded, 500, &[baseline], |_, _| true).unwrap();
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].start_timestamp_ms <= 4000.0 && chunks[0].end_timestamp_ms >= 5000.0);
        assert_eq!(
            chunks[0].samples.len(),
            48_000,
            "include audio context around the known speech"
        );
    }

    #[test]
    fn source_pickup_check_keeps_quiet_independent_and_overlapping_speech() {
        let waveform = |scale: f32, phase: f32| {
            (0..6 * 16000)
                .map(|i| {
                    let t = i as f32 / 16000.0;
                    scale * (t * 300.0).sin() * (0.2 + 0.8 * (t * 5.0 + phase).sin().max(0.0))
                })
                .collect::<Vec<_>>()
        };
        let mut quiet = speech_segment(0.0, 6000.0, 6 * 16000);
        quiet.samples = waveform(0.004, 0.0);
        let mut other = super::super::decoder::DecodedAudio {
            samples: waveform(0.1, 0.0),
            sample_rate: 16000,
            channels: 1,
            duration_seconds: 6.0,
        };
        assert!(
            weak_cross_track_pickup(&quiet, &other),
            "reject weak copies of the same speech pattern"
        );
        other.samples.fill(0.0);
        assert!(
            !weak_cross_track_pickup(&quiet, &other),
            "quiet speech alone must remain"
        );
        other.samples = (0..6 * 16000)
            .map(|i| {
                let t = i as f32 / 16000.0;
                0.1 * (t * 300.0).sin() * (0.2 + 0.8 * (t * 2.21 + 2.5).sin().max(0.0))
            })
            .collect();
        assert!(
            !weak_cross_track_pickup(&quiet, &other),
            "independent overlapping speech must remain"
        );
        quiet.samples = waveform(0.05, 0.0);
        other.samples = waveform(0.1, 0.0);
        assert!(
            !weak_cross_track_pickup(&quiet, &other),
            "comparable voices must remain"
        );
    }

    #[test]
    fn separate_inputs_recover_missed_speech_once_from_the_complete_mix() {
        let silent = super::super::decoder::DecodedAudio {
            samples: vec![0.0; 160_000],
            sample_rate: 16000,
            channels: 1,
            duration_seconds: 10.0,
        };
        let audio = EnhancementAudio {
            mixed: silent.clone(),
            tracks: vec![
                ("microphone".into(), silent.clone()),
                ("system".into(), silent),
            ],
        };
        let baseline:Transcript=serde_json::from_value(serde_json::json!({"id":"quiet","meeting_id":"m1","transcript":"Original quiet words","timestamp":"old","audio_start_time":4.0,"audio_end_time":5.0,"duration":1.0})).unwrap();
        let inputs = whisper_recognition_inputs(&audio, 500, &[baseline], |_, _| true).unwrap();
        assert_eq!(
            inputs.len(),
            1,
            "do not decode a missed baseline region once per silent track"
        );
        assert_eq!(inputs[0].source, None);
        assert_eq!(inputs[0].samples.len(), 48_000);
        assert!(whisper_recognition_inputs(&audio, 500, &[], |_, _| false).is_err());
    }

    #[test]
    fn speech_context_keeps_a_phrase_and_its_paused_continuation() {
        let audio: Vec<f32> = (0..16 * 16_000).map(|i| i as f32).collect();
        let first = speech_segment(0.0, 2380.0, 38_080);
        let continuation = speech_segment(4710.0, 10_990.0, 100_480);
        let grouped = speech_context(&[first, continuation], &audio);
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].start_timestamp_ms, 0.0);
        assert_eq!(grouped[0].end_timestamp_ms, 10_990.0);
        assert_eq!(
            grouped[0].samples,
            audio[..175_840],
            "keep the pause and every source sample"
        );
    }

    #[test]
    fn speech_context_keeps_long_silence_and_complete_spans_apart() {
        let audio = vec![0.1; 30 * 16_000];
        let segments = vec![
            speech_segment(0.0, 2000.0, 32_000),
            speech_segment(7000.0, 16_000.0, 144_000),
            speech_segment(17_000.0, 19_000.0, 32_000),
        ];
        let grouped = speech_context(&segments, &audio);
        assert_eq!(grouped.len(), 3);
        assert_eq!(grouped[1].samples, segments[1].samples);
    }

    #[test]
    fn speech_context_retains_spans_if_source_bounds_are_unavailable() {
        let segments = vec![
            speech_segment(0.0, 1000.0, 16_000),
            speech_segment(2000.0, 3000.0, 16_000),
        ];
        let grouped = speech_context(&segments, &[]);
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[0].samples, segments[0].samples);
        assert_eq!(grouped[1].samples, segments[1].samples);
    }

    #[test]
    fn input_chunks_keep_a_short_phrase_in_one_piece() {
        // An estimated microphone speaker change at 644.966 seconds used to
        // cut the system speaker's phrase at 644.04..646.42 seconds.
        let mut audio = speech_segment(644_040.0, 646_420.0, 38_080);
        audio.samples = (0..38_080).map(|i| i as f32 / 38_080.0).collect();
        let chunks = input_chunks(&audio, 25 * 16_000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].samples, audio.samples);
        assert_eq!(chunks[0].start_timestamp_ms, audio.start_timestamp_ms);
        assert_eq!(chunks[0].end_timestamp_ms, audio.end_timestamp_ms);
    }

    #[test]
    fn input_chunks_preserve_all_samples_when_bounding_long_speech() {
        let mut audio = speech_segment(0.0, 60_000.0, 60 * 16_000);
        audio.samples = (0..60 * 16_000)
            .map(|i| 0.1 + (i % 100) as f32 / 1000.0)
            .collect();
        let chunks = input_chunks(&audio, 25 * 16_000);
        assert!(chunks.len() > 1);
        let joined: Vec<f32> = chunks
            .iter()
            .flat_map(|chunk| chunk.samples.iter().copied())
            .collect();
        assert_eq!(
            joined, audio.samples,
            "no speech is lost or repeated at cuts"
        );
        for pair in chunks.windows(2) {
            assert_eq!(pair[0].end_timestamp_ms, pair[1].start_timestamp_ms);
        }
        assert_eq!(
            chunks.last().unwrap().end_timestamp_ms,
            audio.end_timestamp_ms
        );
    }

    #[test]
    fn input_chunks_ignore_empty_segments() {
        let empty = speech_segment(0.0, 1000.0, 0);
        assert!(input_chunks(&empty, 25 * 16_000).is_empty());

        let zero_length = speech_segment(0.0, 0.0, 16_000);
        assert!(input_chunks(&zero_length, 25 * 16_000).is_empty());
    }

    #[test]
    fn transcript_rows_copy_segment_fields() {
        let segments = vec![TranscriptSegment {
            id: "row-1".to_string(),
            text: "hello".to_string(),
            timestamp: "00:01".to_string(),
            audio_start_time: Some(1.0),
            audio_end_time: Some(2.5),
            duration: Some(1.5),
            speaker_id: None,
            speaker_label: None,
            speaker_profile_id: None,
            speaker_source: None,
            speaker_color: None,
            is_overlap: None,
            candidate_speaker_ids: None,
            diarization_status: None,
            diarization_method: None,
            diarization_confidence: None,
        }];
        let rows = transcript_rows("meeting-1", &segments);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "row-1");
        assert_eq!(rows[0].meeting_id, "meeting-1");
        assert_eq!(rows[0].transcript, "hello");
        assert_eq!(rows[0].timestamp, "00:01");
        assert_eq!(rows[0].audio_start_time, Some(1.0));
        assert_eq!(rows[0].audio_end_time, Some(2.5));
        assert_eq!(rows[0].duration, Some(1.5));
        assert!(rows[0].summary.is_none());
        assert!(rows[0].action_items.is_none());
        assert!(rows[0].key_points.is_none());
    }
    async fn publication_fixture() -> (tempfile::TempDir, SqlitePool, SqlitePool, Vec<Transcript>) {
        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        let dir = tempfile::tempdir().unwrap();
        let connect = |name: &str| {
            SqliteConnectOptions::new()
                .filename(dir.path().join(name))
                .create_if_missing(true)
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        };
        let core = SqlitePoolOptions::new()
            .connect_with(connect("core.sqlite"))
            .await
            .unwrap();
        let feature = SqlitePoolOptions::new()
            .connect_with(connect("speaker.sqlite"))
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&core).await.unwrap();
        sqlx::migrate!("./speaker-migrations")
            .run(&feature)
            .await
            .unwrap();
        sqlx::query("INSERT INTO meetings(id,title,created_at,updated_at) VALUES ('m1','Test',CURRENT_TIMESTAMP,CURRENT_TIMESTAMP)")
            .execute(&core).await.unwrap();
        sqlx::query("INSERT INTO transcripts(id,meeting_id,transcript,timestamp,audio_start_time,audio_end_time,duration) VALUES ('old','m1','Old merged text','old time',0,88,88)")
            .execute(&core).await.unwrap();
        sqlx::query("INSERT INTO transcript_speakers(transcript_id,meeting_id,speaker_id,speaker_label,diarization_status,published) VALUES ('old','m1','old-speaker','Speaker 1','final',1)")
            .execute(&feature).await.unwrap();
        let original = crate::diarization::finalize::load_transcripts(&core, "m1")
            .await
            .unwrap();
        (dir, core, feature, original)
    }

    fn fresh_segments() -> Vec<TranscriptSegment> {
        let result = timed("Alice. Bob.", &[" Alice.", " Bob."], &[1.25, 15.5]);
        let rows = parakeet_rows(&result, 0.0, 25_000.0, None).unwrap();
        super::super::common::create_transcript_segments(&rows)
    }

    async fn staged_review(
        core: &SqlitePool,
        feature: &SqlitePool,
        original: &[Transcript],
    ) -> String {
        super::super::enhancement_review::stage(
            core,
            feature,
            super::super::enhancement_review::Proposal {
                meeting_id: "m1".into(),
                folder: "/unused".into(),
                original: original.to_vec(),
                segments: fresh_segments(),
                labels: None,
                duration_seconds: 88.0,
                language: None,
                provider: Some("whisper".into()),
                model: Some("large-v3".into()),
                core_backup: vec![],
                speaker_backup: String::new(),
                publication_rows: String::new(),
            },
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a_finished_proposal_keeps_saved_words_and_manual_speaker_data() {
        let (_dir, core, feature, original) = publication_fixture().await;
        let before = super::super::enhancement_review::speaker_snapshot(&feature, "m1")
            .await
            .unwrap();
        let id = staged_review(&core, &feature, &original).await;
        let saved = crate::diarization::finalize::load_transcripts(&core, "m1")
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_string(&saved).unwrap(),
            serde_json::to_string(&original).unwrap()
        );
        assert_eq!(
            before,
            super::super::enhancement_review::speaker_snapshot(&feature, "m1")
                .await
                .unwrap()
        );
        let pending = super::super::enhancement_review::pending(&feature, "m1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pending.id, id);
        assert_eq!(pending.original[0].transcript, "Old merged text");
        assert_eq!(pending.proposed.len(), 2);
        super::super::enhancement_review::discard(&feature, "m1", &id)
            .await
            .unwrap();
        assert!(super::super::enhancement_review::pending(&feature, "m1")
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            before,
            super::super::enhancement_review::speaker_snapshot(&feature, "m1")
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn accepted_review_retains_the_full_original_after_commit_and_reopen() {
        let (dir, core, feature, _original) = publication_fixture().await;
        sqlx::query("UPDATE transcripts SET summary='Keep summary',speaker='mic' WHERE id='old'")
            .execute(&core)
            .await
            .unwrap();
        let original = crate::diarization::finalize::load_transcripts(&core, "m1")
            .await
            .unwrap();
        let id = staged_review(&core, &feature, &original).await;
        let candidate = super::super::enhancement_review::load(&feature, "m1", &id)
            .await
            .unwrap();
        publish(
            &core,
            &feature,
            "m1",
            &candidate.original,
            &candidate.segments,
            None,
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
        feature.close().await;
        let reopened = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(dir.path().join("speaker.sqlite")),
            )
            .await
            .unwrap();
        let (status, payload): (String, String) =
            sqlx::query_as("SELECT state,payload FROM enhancement_reviews WHERE id=?")
                .bind(id)
                .fetch_one(&reopened)
                .await
                .unwrap();
        assert_eq!(status, "applied");
        let saved: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(saved["core_backup"][0]["summary"], "Keep summary");
        assert_eq!(saved["core_backup"][0]["speaker"], "mic");
        assert!(saved["speaker_backup"]
            .as_str()
            .unwrap()
            .contains("Speaker 1"));
        assert!(super::super::enhancement_review::pending(&reopened, "m1")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn review_does_not_overwrite_text_edited_after_generation() {
        let (_dir, core, feature, original) = publication_fixture().await;
        let id = staged_review(&core, &feature, &original).await;
        let candidate = super::super::enhancement_review::load(&feature, "m1", &id)
            .await
            .unwrap();
        sqlx::query("UPDATE transcripts SET transcript='Manual correction' WHERE id='old'")
            .execute(&core)
            .await
            .unwrap();
        let result = publish(
            &core,
            &feature,
            "m1",
            &candidate.original,
            &candidate.segments,
            None,
            &AtomicBool::new(false),
        )
        .await;
        assert!(result.is_err());
        let text: String = sqlx::query_scalar("SELECT transcript FROM transcripts WHERE id='old'")
            .fetch_one(&core)
            .await
            .unwrap();
        assert_eq!(text, "Manual correction");
        assert!(super::super::enhancement_review::pending(&feature, "m1")
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn a_failed_publication_does_not_accept_the_review() {
        let (_dir, core, feature, original) = publication_fixture().await;
        let id = staged_review(&core, &feature, &original).await;
        let candidate = super::super::enhancement_review::load(&feature, "m1", &id)
            .await
            .unwrap();
        assert!(publish(
            &core,
            &feature,
            "m1",
            &candidate.original,
            &candidate.segments,
            None,
            &AtomicBool::new(true)
        )
        .await
        .is_err());
        assert!(super::super::enhancement_review::pending(&feature, "m1")
            .await
            .unwrap()
            .is_some());
        sqlx::query(
            "UPDATE transcript_speakers SET speaker_label='Manual name' WHERE transcript_id='old'",
        )
        .execute(&feature)
        .await
        .unwrap();
        assert_ne!(
            candidate.speaker_backup,
            super::super::enhancement_review::speaker_snapshot(&feature, "m1")
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn restart_accepts_only_the_review_whose_core_write_survived() {
        for committed in [false, true] {
            let (_dir, core, feature, original) = publication_fixture().await;
            let id = staged_review(&core, &feature, &original).await;
            let candidate = super::super::enhancement_review::load(&feature, "m1", &id)
                .await
                .unwrap();
            sqlx::query("INSERT INTO enhancement_publications VALUES ('m1', ?, ?, '[]', NULL)")
                .bind(serde_json::to_string(&original).unwrap())
                .bind(&candidate.publication_rows)
                .execute(&feature)
                .await
                .unwrap();
            if committed {
                sqlx::query("DELETE FROM transcripts WHERE meeting_id='m1'")
                    .execute(&core)
                    .await
                    .unwrap();
                for row in &candidate.segments {
                    sqlx::query("INSERT INTO transcripts(id,meeting_id,transcript,timestamp,audio_start_time,audio_end_time,duration) VALUES (?,'m1',?,?,?,?,?)")
                        .bind(&row.id).bind(&row.text).bind(&row.timestamp).bind(row.audio_start_time).bind(row.audio_end_time).bind(row.duration)
                        .execute(&core).await.unwrap();
                }
            }
            reconcile_publications(&core, &feature).await.unwrap();
            let status: String =
                sqlx::query_scalar("SELECT state FROM enhancement_reviews WHERE id=?")
                    .bind(&id)
                    .fetch_one(&feature)
                    .await
                    .unwrap();
            assert_eq!(status, if committed { "applied" } else { "pending" });
            crate::database::repositories::diarization::DiarizationRepository::delete_meeting_data(
                &feature, "m1",
            )
            .await
            .unwrap();
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM enhancement_reviews")
                .fetch_one(&feature)
                .await
                .unwrap();
            assert_eq!(
                count, 0,
                "meeting deletion must remove retained transcript history"
            );
        }
    }

    #[tokio::test]
    async fn undo_restores_words_times_summaries_and_speakers_exactly() {
        let (_dir, core, feature, _) = publication_fixture().await;
        sqlx::query(
            "UPDATE transcripts SET summary='Original summary',speaker='mic' WHERE id='old'",
        )
        .execute(&core)
        .await
        .unwrap();
        let original = crate::diarization::finalize::load_transcripts(&core, "m1")
            .await
            .unwrap();
        let id = staged_review(&core, &feature, &original).await;
        let candidate = super::super::enhancement_review::load(&feature, "m1", &id)
            .await
            .unwrap();
        publish(
            &core,
            &feature,
            "m1",
            &original,
            &candidate.segments,
            None,
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
        assert!(
            super::super::enhancement_review::latest(&feature, "m1")
                .await
                .unwrap()
                .unwrap()
                .applied
        );
        let restored = super::super::enhancement_review::undo(&core, &feature, "m1", &id)
            .await
            .unwrap();
        let actual: Vec<crate::diarization::finalize::CoreTranscriptBackup> = sqlx::query_as(
            "SELECT * FROM transcripts WHERE meeting_id='m1' ORDER BY audio_start_time,id",
        )
        .fetch_all(&core)
        .await
        .unwrap();
        assert_eq!(
            serde_json::to_string(&actual).unwrap(),
            serde_json::to_string(&candidate.core_backup).unwrap()
        );
        assert_eq!(
            candidate.speaker_backup,
            super::super::enhancement_review::speaker_snapshot(&feature, "m1")
                .await
                .unwrap()
        );
        assert_eq!(
            super::super::enhancement_review::original_segments(&restored).unwrap()[0]
                .speaker_label
                .as_deref(),
            Some("Speaker 1")
        );
        assert!(super::super::enhancement_review::latest(&feature, "m1")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn undo_keeps_later_word_and_speaker_edits() {
        for edit_speaker in [false, true] {
            let (_dir, core, feature, original) = publication_fixture().await;
            let id = staged_review(&core, &feature, &original).await;
            let candidate = super::super::enhancement_review::load(&feature, "m1", &id)
                .await
                .unwrap();
            publish(
                &core,
                &feature,
                "m1",
                &original,
                &candidate.segments,
                None,
                &AtomicBool::new(false),
            )
            .await
            .unwrap();
            if edit_speaker {
                sqlx::query(
                    "UPDATE meeting_diarization_status SET method='manual' WHERE meeting_id='m1'",
                )
                .execute(&feature)
                .await
                .unwrap();
            } else {
                sqlx::query(
                    "UPDATE transcripts SET transcript='Later correction' WHERE meeting_id='m1'",
                )
                .execute(&core)
                .await
                .unwrap();
            }
            let before = crate::diarization::finalize::load_transcripts(&core, "m1")
                .await
                .unwrap();
            assert!(
                super::super::enhancement_review::undo(&core, &feature, "m1", &id)
                    .await
                    .is_err()
            );
            assert_eq!(
                serde_json::to_string(&before).unwrap(),
                serde_json::to_string(
                    &crate::diarization::finalize::load_transcripts(&core, "m1")
                        .await
                        .unwrap()
                )
                .unwrap()
            );
        }
    }

    #[tokio::test]
    async fn failed_undo_keeps_the_applied_result_and_can_retry() {
        let (_dir, core, feature, original) = publication_fixture().await;
        let id = staged_review(&core, &feature, &original).await;
        let candidate = super::super::enhancement_review::load(&feature, "m1", &id)
            .await
            .unwrap();
        publish(
            &core,
            &feature,
            "m1",
            &original,
            &candidate.segments,
            None,
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
        sqlx::query("CREATE TRIGGER reject_undo BEFORE INSERT ON transcript_speakers BEGIN SELECT RAISE(ABORT,'test failure'); END").execute(&feature).await.unwrap();
        assert!(
            super::super::enhancement_review::undo(&core, &feature, "m1", &id)
                .await
                .is_err()
        );
        let current = crate::diarization::finalize::load_transcripts(&core, "m1")
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_string(&current).unwrap(),
            candidate.publication_rows
        );
        assert!(
            super::super::enhancement_review::latest(&feature, "m1")
                .await
                .unwrap()
                .unwrap()
                .applied
        );
        sqlx::query("DROP TRIGGER reject_undo")
            .execute(&feature)
            .await
            .unwrap();
        super::super::enhancement_review::undo(&core, &feature, "m1", &id)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn restart_finishes_the_speaker_restore_for_committed_undo() {
        let (_dir, core, feature, original) = publication_fixture().await;
        let id = staged_review(&core, &feature, &original).await;
        let candidate = super::super::enhancement_review::load(&feature, "m1", &id)
            .await
            .unwrap();
        publish(
            &core,
            &feature,
            "m1",
            &original,
            &candidate.segments,
            None,
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
        let current = crate::diarization::finalize::load_transcripts(&core, "m1")
            .await
            .unwrap();
        let current_json = serde_json::to_string(&current).unwrap();
        let restore = serde_json::to_string(&SpeakerChange::Undo {
            restore_speakers: candidate.speaker_backup.clone(),
            restore_core: serde_json::to_string(&candidate.core_backup).unwrap(),
            undo_id: id.clone(),
        })
        .unwrap();
        let rollback = Publication {
            meeting_id: "m1".into(),
            original_rows: current_json.clone(),
            proposed_rows: current_json.clone(),
            core_backup: serde_json::to_string(&candidate.core_backup).unwrap(),
            speaker_labels: None,
        };
        restore_core(&core, &rollback).await.unwrap();
        sqlx::query("INSERT INTO enhancement_publications VALUES ('m1',?,?,'[]',?)")
            .bind(current_json)
            .bind(serde_json::to_string(&original).unwrap())
            .bind(restore)
            .execute(&feature)
            .await
            .unwrap();
        reconcile_publications(&core, &feature).await.unwrap();
        assert_eq!(
            candidate.speaker_backup,
            super::super::enhancement_review::speaker_snapshot(&feature, "m1")
                .await
                .unwrap()
        );
        let state: String = sqlx::query_scalar("SELECT state FROM enhancement_reviews WHERE id=?")
            .bind(id)
            .fetch_one(&feature)
            .await
            .unwrap();
        assert_eq!(state, "undone");
    }

    #[tokio::test]
    async fn an_estimated_boundary_shift_does_not_reject_existing_speech() {
        let (_dir, core, feature, _) = publication_fixture().await;
        sqlx::query("UPDATE transcripts SET audio_start_time=17,audio_end_time=18,duration=1 WHERE id='old'")
            .execute(&core).await.unwrap();
        let original = crate::diarization::finalize::load_transcripts(&core, "m1")
            .await
            .unwrap();
        // The replacement ends at 15.66 s. Token and VAD boundaries can differ.
        let id = staged_review(&core, &feature, &original).await;
        assert!(!id.is_empty());
    }

    #[tokio::test]
    async fn missing_speech_region_blocks_apply_and_preserves_saved_text() {
        let (_dir, core, feature, original) = publication_fixture().await;
        let id = staged_review(&core, &feature, &original).await;
        let mut proposal = super::super::enhancement_review::load(&feature, "m1", &id)
            .await
            .unwrap();
        proposal.original[0].audio_start_time = Some(40.0);
        proposal.original[0].audio_end_time = Some(45.0);
        let error = super::super::enhancement_review::stage(&core, &feature, proposal)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("00:40"));
        let current = crate::diarization::finalize::load_transcripts(&core, "m1")
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_string(&current).unwrap(),
            serde_json::to_string(&original).unwrap()
        );
        assert_eq!(
            super::super::enhancement_review::pending(&feature, "m1")
                .await
                .unwrap()
                .unwrap()
                .id,
            id,
            "a rejected replacement must not erase an earlier saved result"
        );
    }

    #[tokio::test]
    async fn repeated_enhancements_can_be_undone_in_order() {
        let (_dir, core, feature, original) = publication_fixture().await;
        let first = staged_review(&core, &feature, &original).await;
        let candidate = super::super::enhancement_review::load(&feature, "m1", &first)
            .await
            .unwrap();
        publish(
            &core,
            &feature,
            "m1",
            &original,
            &candidate.segments,
            None,
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
        let once = crate::diarization::finalize::load_transcripts(&core, "m1")
            .await
            .unwrap();
        let second = staged_review(&core, &feature, &once).await;
        let candidate = super::super::enhancement_review::load(&feature, "m1", &second)
            .await
            .unwrap();
        publish(
            &core,
            &feature,
            "m1",
            &once,
            &candidate.segments,
            None,
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
        super::super::enhancement_review::undo(&core, &feature, "m1", &second)
            .await
            .unwrap();
        assert_eq!(
            super::super::enhancement_review::latest(&feature, "m1")
                .await
                .unwrap()
                .unwrap()
                .id,
            first
        );
        super::super::enhancement_review::undo(&core, &feature, "m1", &first)
            .await
            .unwrap();
        let final_rows = crate::diarization::finalize::load_transcripts(&core, "m1")
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_string(&final_rows).unwrap(),
            serde_json::to_string(&original).unwrap()
        );
    }

    #[tokio::test]
    async fn fresh_model_text_and_times_replace_a_long_old_row() {
        let (_dir, core, feature, original) = publication_fixture().await;
        let segments = fresh_segments();
        publish(
            &core,
            &feature,
            "m1",
            &original,
            &segments,
            None,
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
        let rows = crate::diarization::finalize::load_transcripts(&core, "m1")
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].transcript, "Alice.");
        assert_eq!(rows[1].transcript, "Bob.");
        assert_eq!(rows[0].audio_start_time, Some(1.25));
        assert_eq!(rows[1].audio_start_time, Some(15.5));
        assert!(rows
            .iter()
            .all(|row| row.audio_end_time.unwrap() < 16.0 && row.id != "old"));
        let stale: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM transcript_speakers WHERE meeting_id='m1'")
                .fetch_one(&feature)
                .await
                .unwrap();
        assert_eq!(stale, 0, "old speaker rows must not attach to the new text");
    }

    #[tokio::test]
    async fn failed_or_cancelled_enhancement_keeps_old_text_times_and_speaker() {
        let (_dir, core, feature, original) = publication_fixture().await;
        let segments = fresh_segments();
        sqlx::query("CREATE TRIGGER fail_enhance BEFORE INSERT ON transcripts WHEN NEW.id != 'old' BEGIN SELECT RAISE(ABORT,'test failure'); END")
            .execute(&core).await.unwrap();
        assert!(publish(
            &core,
            &feature,
            "m1",
            &original,
            &segments,
            None,
            &AtomicBool::new(false)
        )
        .await
        .is_err());
        sqlx::query("DROP TRIGGER fail_enhance")
            .execute(&core)
            .await
            .unwrap();
        assert!(publish(
            &core,
            &feature,
            "m1",
            &original,
            &segments,
            None,
            &AtomicBool::new(true)
        )
        .await
        .is_err());
        let rows = crate::diarization::finalize::load_transcripts(&core, "m1")
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_string(&rows).unwrap(),
            serde_json::to_string(&original).unwrap()
        );
        let label: String = sqlx::query_scalar(
            "SELECT speaker_label FROM transcript_speakers WHERE transcript_id='old'",
        )
        .fetch_one(&feature)
        .await
        .unwrap();
        assert_eq!(label, "Speaker 1");
    }

    fn speaker_labels(segments: &[TranscriptSegment]) -> EnhancementSpeakerLabels {
        serde_json::from_value(serde_json::json!({
            "windows": segments.iter().map(|row| serde_json::json!({
                "source": "microphone", "start": row.audio_start_time.unwrap(), "end": row.audio_end_time.unwrap(),
                "speaker_id": "alice", "speaker_profile_id": null, "candidate_speaker_ids": [],
                "confidence": 0.9, "is_overlap": false,
            })).collect::<Vec<_>>(),
            "transcripts": segments.iter().map(|row| serde_json::json!({
                "id": row.id, "text": row.text, "start": row.audio_start_time.unwrap(), "end": row.audio_end_time.unwrap(),
                "speaker_id": "alice", "speaker_profile_id": null, "speaker_label": "Alice", "source": "microphone",
                "is_overlap": false, "candidates": [], "confidence": 0.9,
            })).collect::<Vec<_>>(),
            "quality_flags": [], "coverage": 1.0,
        })).unwrap()
    }

    #[tokio::test]
    async fn wal_restart_resolves_each_publication_boundary() {
        for core_committed in [false, true] {
            let (_dir, core, feature, original) = publication_fixture().await;
            let segments = fresh_segments();
            let proposed = transcript_rows("m1", &segments);
            let labels = speaker_labels(&segments);
            // Simulate a process stop after the intent commits, either before
            // or after the independent core transaction commits.
            let mut feature_connection = durable_connection(&feature).await.unwrap();
            sqlx::query("INSERT INTO enhancement_publications VALUES ('m1', ?, ?, '[]', ?)")
                .bind(serde_json::to_string(&original).unwrap())
                .bind(serde_json::to_string(&proposed).unwrap())
                .bind(serde_json::to_string(&labels).unwrap())
                .execute(&mut feature_connection)
                .await
                .unwrap();
            feature_connection.close().await.unwrap();
            if core_committed {
                let mut tx = core.begin().await.unwrap();
                sqlx::query("DELETE FROM transcripts WHERE meeting_id = 'm1'")
                    .execute(&mut *tx)
                    .await
                    .unwrap();
                for segment in &segments {
                    sqlx::query("INSERT INTO transcripts (id, meeting_id, transcript, timestamp, audio_start_time, audio_end_time, duration) VALUES (?, 'm1', ?, ?, ?, ?, ?)")
                        .bind(&segment.id).bind(&segment.text).bind(&segment.timestamp)
                        .bind(segment.audio_start_time).bind(segment.audio_end_time).bind(segment.duration)
                        .execute(&mut *tx).await.unwrap();
                }
                tx.commit().await.unwrap();
            }
            let core_options = core.connect_options().as_ref().clone();
            let feature_options = feature.connect_options().as_ref().clone();
            core.close().await;
            feature.close().await;
            let core = SqlitePool::connect_with(core_options).await.unwrap();
            let feature = SqlitePool::connect_with(feature_options).await.unwrap();
            for _ in 0..2 {
                crate::diarization::finalize::reconcile_interrupted_runs(&core, &feature)
                    .await
                    .unwrap();
            }
            let rows = crate::diarization::finalize::load_transcripts(&core, "m1")
                .await
                .unwrap();
            assert_eq!(
                serde_json::to_string(&rows).unwrap(),
                serde_json::to_string(if core_committed { &proposed } else { &original }).unwrap()
            );
            let labels: Vec<String> = sqlx::query_scalar(
                "SELECT speaker_label FROM transcript_speakers WHERE meeting_id = 'm1'",
            )
            .fetch_all(&feature)
            .await
            .unwrap();
            assert_eq!(
                labels,
                if core_committed {
                    vec!["Alice", "Alice"]
                } else {
                    vec!["Speaker 1"]
                }
            );
            let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM enhancement_publications")
                .fetch_one(&feature)
                .await
                .unwrap();
            assert_eq!(pending, 0);
        }
    }

    #[tokio::test]
    async fn a_speaker_commit_failure_restores_the_complete_old_core_row() {
        let (_dir, core, feature, _) = publication_fixture().await;
        sqlx::query(
            "UPDATE transcripts SET speaker = 'mic', summary = 'keep summary' WHERE id = 'old'",
        )
        .execute(&core)
        .await
        .unwrap();
        let original = crate::diarization::finalize::load_transcripts(&core, "m1")
            .await
            .unwrap();
        sqlx::raw_sql("CREATE TABLE fail_commit (profile_id TEXT REFERENCES speaker_profiles(id) DEFERRABLE INITIALLY DEFERRED);
            CREATE TRIGGER fail_speaker_commit AFTER INSERT ON meeting_diarization_status BEGIN
                INSERT INTO fail_commit VALUES ('missing-profile'); END;")
            .execute(&feature).await.unwrap();
        assert!(publish(
            &core,
            &feature,
            "m1",
            &original,
            &fresh_segments(),
            None,
            &AtomicBool::new(false)
        )
        .await
        .is_err());
        let rows = crate::diarization::finalize::load_transcripts(&core, "m1")
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_string(&rows).unwrap(),
            serde_json::to_string(&original).unwrap()
        );
        let speaker: String =
            sqlx::query_scalar("SELECT speaker FROM transcripts WHERE id = 'old'")
                .fetch_one(&core)
                .await
                .unwrap();
        assert_eq!(speaker, "mic");
        let label: String = sqlx::query_scalar(
            "SELECT speaker_label FROM transcript_speakers WHERE transcript_id = 'old'",
        )
        .fetch_one(&feature)
        .await
        .unwrap();
        assert_eq!(label, "Speaker 1");
        let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM enhancement_publications")
            .fetch_one(&feature)
            .await
            .unwrap();
        assert_eq!(pending, 0);
    }

    #[tokio::test]
    async fn wal_publication_commits_labels_with_fresh_rows() {
        let (_dir, core, feature, original) = publication_fixture().await;
        let segments = fresh_segments();
        publish(
            &core,
            &feature,
            "m1",
            &original,
            &segments,
            Some(&speaker_labels(&segments)),
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
        let labels: Vec<String> = sqlx::query_scalar(
            "SELECT speaker_label FROM transcript_speakers WHERE meeting_id = 'm1'",
        )
        .fetch_all(&feature)
        .await
        .unwrap();
        assert_eq!(labels, vec!["Alice", "Alice"]);
        reconcile_publications(&core, &feature).await.unwrap();
    }

    #[tokio::test]
    async fn unmatched_recovery_keeps_current_rows_and_does_not_block_other_meetings() {
        let (_dir, core, feature, original) = publication_fixture().await;
        sqlx::query("INSERT INTO enhancement_publications VALUES ('m1', ?, '[]', '[]', NULL)")
            .bind(serde_json::to_string(&original).unwrap())
            .execute(&feature)
            .await
            .unwrap();
        sqlx::query("UPDATE transcripts SET transcript = 'Edited after Enhance' WHERE id = 'old'")
            .execute(&core)
            .await
            .unwrap();
        sqlx::query("INSERT INTO meeting_diarization_status (meeting_id, current_status, progress, assigned_coverage)
            VALUES ('m1', 'completed', 100, 1)").execute(&feature).await.unwrap();
        sqlx::query("INSERT INTO meetings(id,title,created_at,updated_at) VALUES ('m2','Other',CURRENT_TIMESTAMP,CURRENT_TIMESTAMP)")
            .execute(&core).await.unwrap();
        // An independent publication must still finish after the mismatch.
        sqlx::query("INSERT INTO enhancement_publications VALUES ('m2', '[{}]', '[]', '[]', NULL)")
            .execute(&feature)
            .await
            .unwrap();
        for _ in 0..2 {
            crate::diarization::finalize::reconcile_interrupted_runs(&core, &feature)
                .await
                .unwrap();
        }
        let rows = crate::diarization::finalize::load_transcripts(&core, "m1")
            .await
            .unwrap();
        let mut expected = original;
        expected[0].transcript = "Edited after Enhance".into();
        assert_eq!(
            serde_json::to_string(&rows).unwrap(),
            serde_json::to_string(&expected).unwrap()
        );
        let status: (String, i64, Option<f64>, String) = sqlx::query_as(
            "SELECT current_status, progress, assigned_coverage, failure_reason FROM meeting_diarization_status WHERE meeting_id = 'm1'"
        ).fetch_one(&feature).await.unwrap();
        assert_eq!(status.0, "needs_review");
        assert_eq!(status.1, 0);
        assert_eq!(status.2, None);
        assert!(status.3.contains("current transcript was kept"));
        let label: String = sqlx::query_scalar(
            "SELECT speaker_label FROM transcript_speakers WHERE transcript_id = 'old'",
        )
        .fetch_one(&feature)
        .await
        .unwrap();
        assert_eq!(label, "Speaker 1");
        let other_status: String = sqlx::query_scalar(
            "SELECT current_status FROM meeting_diarization_status WHERE meeting_id = 'm2'",
        )
        .fetch_one(&feature)
        .await
        .unwrap();
        assert_eq!(other_status, "none");
        let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM enhancement_publications")
            .fetch_one(&feature)
            .await
            .unwrap();
        assert_eq!(pending, 0);
    }

    #[tokio::test]
    async fn unmatched_recovery_keeps_intent_if_review_status_cannot_be_saved() {
        let (_dir, core, feature, _) = publication_fixture().await;
        sqlx::query("INSERT INTO enhancement_publications VALUES ('m1', '[]', '[]', '[]', NULL)")
            .execute(&feature)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TRIGGER fail_review BEFORE INSERT ON meeting_diarization_status
            BEGIN SELECT RAISE(ABORT, 'test review status failure'); END",
        )
        .execute(&feature)
        .await
        .unwrap();
        assert!(reconcile_publications(&core, &feature).await.is_err());
        let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM enhancement_publications")
            .fetch_one(&feature)
            .await
            .unwrap();
        assert_eq!(pending, 1);
    }

    #[tokio::test]
    async fn a_deleted_meeting_does_not_leave_a_recovery_blocker() {
        for feature_cleanup_completed in [false, true] {
            let (_dir, core, feature, original) = publication_fixture().await;
            sqlx::query("INSERT INTO enhancement_publications VALUES ('m1', ?, '[]', '[]', NULL)")
                .bind(serde_json::to_string(&original).unwrap())
                .execute(&feature)
                .await
                .unwrap();
            sqlx::query("DELETE FROM meetings WHERE id = 'm1'")
                .execute(&core)
                .await
                .unwrap();
            if feature_cleanup_completed {
                crate::database::repositories::diarization::DiarizationRepository::delete_meeting_data(&feature, "m1")
                    .await.unwrap();
            }
            reconcile_publications(&core, &feature).await.unwrap();
            let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM enhancement_publications")
                .fetch_one(&feature)
                .await
                .unwrap();
            assert_eq!(pending, 0);
            let meetings: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM meetings")
                .fetch_one(&core)
                .await
                .unwrap();
            assert_eq!(meetings, 0);
        }
    }
}
