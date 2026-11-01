//! Retain each enhancement and its original text for comparison and Undo.
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use sqlx::{Column, Row, SqliteConnection, SqlitePool, TypeInfo, ValueRef};

use crate::{
    api::TranscriptSegment,
    database::models::Transcript,
    diarization::finalize::{CoreTranscriptBackup, EnhancementSpeakerLabels},
};

#[derive(Serialize, Deserialize)]
pub(crate) struct Proposal {
    pub meeting_id: String,
    pub folder: String,
    pub original: Vec<Transcript>,
    pub segments: Vec<TranscriptSegment>,
    pub labels: Option<EnhancementSpeakerLabels>,
    pub duration_seconds: f64,
    pub language: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    // Retain the full core rows and speaker state, including manual edits.
    pub core_backup: Vec<CoreTranscriptBackup>,
    pub speaker_backup: String,
    pub publication_rows: String,
    /// Saved speech the new run left without words. Shown, never blocking.
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Serialize)]
pub struct ReviewPreview {
    pub id: String,
    pub original: Vec<Transcript>,
    pub proposed: Vec<TranscriptSegment>,
    pub warnings: Vec<String>,
    pub model: Option<String>,
    pub applied: bool,
}

impl Proposal {
    fn preview(self, id: String, applied: bool) -> ReviewPreview {
        ReviewPreview {
            id,
            original: self.original,
            proposed: self.segments,
            warnings: self.warnings,
            model: self.model,
            applied,
        }
    }
}

/// Read only the meeting's speaker metadata; never include voice embeddings.
/// Stable row and column order makes intervening manual edits detectable.
pub(crate) async fn speaker_snapshot(pool: &SqlitePool, meeting_id: &str) -> Result<String> {
    let mut connection = pool.acquire().await?;
    speaker_snapshot_on(&mut connection, meeting_id).await
}

pub(crate) async fn speaker_snapshot_on(
    connection: &mut SqliteConnection,
    meeting_id: &str,
) -> Result<String> {
    let mut snapshot = serde_json::Map::new();
    for (table, order) in [
        ("transcript_speakers", "transcript_id"),
        ("speaker_segments", "id"),
        ("meeting_diarization_status", "meeting_id"),
    ] {
        let rows = sqlx::query(&format!(
            "SELECT * FROM {table} WHERE meeting_id = ? ORDER BY {order}"
        ))
        .bind(meeting_id)
        .fetch_all(&mut *connection)
        .await?;
        let mut values = Vec::new();
        for row in rows {
            let mut object = serde_json::Map::new();
            for column in row.columns() {
                let name = column.name();
                let value = row.try_get_raw(name)?;
                let value = if value.is_null() {
                    serde_json::Value::Null
                } else {
                    match value.type_info().name() {
                        "INTEGER" => serde_json::json!(row.try_get::<i64, _>(name)?),
                        "REAL" => serde_json::json!(row.try_get::<f64, _>(name)?),
                        _ => serde_json::json!(row.try_get::<String, _>(name)?),
                    }
                };
                object.insert(name.into(), value);
            }
            values.push(serde_json::Value::Object(object));
        }
        snapshot.insert(table.into(), values.into());
    }
    Ok(serde_json::to_string(&snapshot)?)
}

/// Do not silently remove previously transcribed speech, including row interiors.
/// This checks missing output, not whether changed words are correct.
/// Decoder and token times are estimates. Allow a two-second boundary shift.
/// After that allowance, report uncovered stretches longer than two seconds.
/// A new run can be better even where it differs, so this warns and Undo
/// restores the saved transcript; it never discards the run.
fn baseline_gaps(proposal: &Proposal) -> Option<String> {
    let mut gaps = Vec::new();
    let mut intervals: Vec<_> = proposal
        .segments
        .iter()
        .filter_map(|new| {
            let (Some(a), Some(b)) = (new.audio_start_time, new.audio_end_time) else {
                return None;
            };
            (a.is_finite() && b.is_finite() && b > a && new.text.chars().any(char::is_alphanumeric))
                .then_some((a - 2.0, b + 2.0))
        })
        .collect();
    intervals.sort_by(|a, b| a.0.total_cmp(&b.0));
    for row in &proposal.original {
        let (Some(start), Some(end)) = (row.audio_start_time, row.audio_end_time) else {
            continue;
        };
        if !start.is_finite()
            || !end.is_finite()
            || start < 0.0
            || end <= start
            || start >= proposal.duration_seconds
            || !row.transcript.chars().any(char::is_alphanumeric)
        {
            continue;
        }
        let end = end.min(proposal.duration_seconds);
        // Stretches of the row more than two seconds from any new words.
        let mut uncovered = Vec::new();
        let mut cursor = start;
        let mut covered = false;
        for &(a, b) in &intervals {
            if b <= start {
                continue;
            }
            if a >= end {
                break;
            }
            covered = true;
            if a - cursor > 2.0 {
                uncovered.push((cursor, a));
            }
            cursor = cursor.max(b);
        }
        if !covered {
            uncovered.push((start, end));
        } else if end - cursor > 2.0 {
            uncovered.push((cursor, end));
        }
        // Saved rows have no word times, so estimate a stretch's words from
        // their place in the row.
        let saved = words(&row.transcript);
        let lost = uncovered.into_iter().find(|&(from, to)| {
            let place = |i: usize| start + (i as f64 + 0.5) / saved.len() as f64 * (end - start);
            let mut expected: Vec<String> = (0..saved.len())
                .filter(|&i| (from..=to).contains(&place(i)))
                .map(|i| saved[i].clone())
                .collect();
            if expected.is_empty() {
                let middle = (from + to) / 2.0;
                expected.extend(
                    (0..saved.len())
                        .min_by(|&a, &b| (place(a) - middle).abs().total_cmp(&(place(b) - middle).abs()))
                        .map(|i| saved[i].clone()),
                );
            }
            !words_nearby(proposal, &expected, from, to)
        });
        if let Some((from, _)) = lost {
            gaps.push(format!("{:02}:{:02}", from as u64 / 60, from as u64 % 60));
        }
    }
    gaps.dedup();
    let more = gaps.len().saturating_sub(5);
    gaps.truncate(5);
    if more > 0 {
        gaps.push(format!("{more} more"));
    }
    (!gaps.is_empty()).then(|| {
        format!(
            "Enhance has no words where the previous transcript had speech at {}. Check those parts, or use Undo to restore the previous transcript.",
            gaps.join(", ")
        )
    })
}

/// Lowercased words without punctuation.
fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|word| !word.is_empty())
        .collect()
}

/// Word times can be wrong even when the words are there: Nemotron sometimes
/// packs a sentence into one 80 ms step per word, seconds before it was said.
/// A stretch counts as lost unless at least 60% of its expected words are
/// found nearby, in order. This errs toward a warning, which never discards
/// the run.
fn words_nearby(proposal: &Proposal, expected: &[String], start: f64, end: f64) -> bool {
    const REACH_SECONDS: f64 = 10.0;
    let mut nearby: Vec<_> = proposal
        .segments
        .iter()
        .filter(|new| {
            new.audio_start_time.is_some_and(|a| a < end + REACH_SECONDS)
                && new.audio_end_time.is_some_and(|b| b > start - REACH_SECONDS)
        })
        .collect();
    nearby.sort_by(|a, b| a.audio_start_time.unwrap().total_cmp(&b.audio_start_time.unwrap()));
    let found: Vec<String> = nearby.iter().flat_map(|new| words(&new.text)).collect();
    // Longest common subsequence, so words must also keep their order.
    let mut row = vec![0usize; found.len() + 1];
    for word in expected {
        let mut diagonal = 0;
        for (i, other) in found.iter().enumerate() {
            let above = row[i + 1];
            row[i + 1] = if word == other {
                diagonal + 1
            } else {
                above.max(row[i])
            };
            diagonal = above;
        }
    }
    row[found.len()] as f64 >= expected.len() as f64 * 0.6
}

pub(crate) async fn stage(
    core: &SqlitePool,
    feature: &SqlitePool,
    mut proposal: Proposal,
) -> Result<String> {
    if proposal.segments.is_empty() {
        return Err(anyhow!(
            "No transcript text was found. The saved transcript was kept."
        ));
    }
    proposal.warnings = baseline_gaps(&proposal).into_iter().collect();
    proposal.core_backup = sqlx::query_as("SELECT id, meeting_id, transcript, timestamp, summary, action_items, key_points,
        audio_start_time, audio_end_time, duration, speaker FROM transcripts WHERE meeting_id = ? ORDER BY audio_start_time, id")
        .bind(&proposal.meeting_id).fetch_all(core).await?;
    proposal.speaker_backup = speaker_snapshot(feature, &proposal.meeting_id).await?;
    let mut rows = super::enhancement::transcript_rows(&proposal.meeting_id, &proposal.segments);
    rows.sort_by(|a, b| {
        a.audio_start_time
            .partial_cmp(&b.audio_start_time)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.id.cmp(&b.id))
    });
    proposal.publication_rows = serde_json::to_string(&rows)?;
    let id = uuid::Uuid::new_v4().to_string();
    let mut tx = feature.begin().await?;
    sqlx::query("DELETE FROM enhancement_reviews WHERE meeting_id = ? AND state = 'pending'")
        .bind(&proposal.meeting_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO enhancement_reviews(id, meeting_id, payload) VALUES (?, ?, ?)")
        .bind(&id)
        .bind(&proposal.meeting_id)
        .bind(serde_json::to_string(&proposal)?)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(id)
}

pub(crate) async fn pending(
    feature: &SqlitePool,
    meeting_id: &str,
) -> Result<Option<ReviewPreview>> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT id, payload FROM enhancement_reviews WHERE meeting_id = ? AND state = 'pending'",
    )
    .bind(meeting_id)
    .fetch_optional(feature)
    .await?;
    row.map(|(id, payload)| Ok(serde_json::from_str::<Proposal>(&payload)?.preview(id, false)))
        .transpose()
}

/// A pending review is always newer than every applied one: staging replaces
/// any earlier pending review, and applying one leaves none pending. Show it
/// first so a failed automatic apply can still be retried or discarded.
pub(crate) async fn latest(
    feature: &SqlitePool,
    meeting_id: &str,
) -> Result<Option<ReviewPreview>> {
    if let Some(review) = pending(feature, meeting_id).await? {
        return Ok(Some(review));
    }
    let row: Option<(String,String)> = sqlx::query_as("SELECT id,payload FROM enhancement_reviews WHERE meeting_id=? AND state='applied' ORDER BY rowid DESC LIMIT 1")
        .bind(meeting_id).fetch_optional(feature).await?;
    row.map(|(id, payload)| Ok(serde_json::from_str::<Proposal>(&payload)?.preview(id, true)))
        .transpose()
}

pub(crate) async fn load_applied(
    feature: &SqlitePool,
    meeting_id: &str,
    id: &str,
) -> Result<(Proposal, String)> {
    let row: Option<(String,String)> = sqlx::query_as("SELECT payload,applied_speakers FROM enhancement_reviews WHERE meeting_id=? AND id=? AND state='applied'")
        .bind(meeting_id).bind(id).fetch_optional(feature).await?;
    let (payload, snapshot) =
        row.ok_or_else(|| anyhow!("This enhancement is no longer available for Undo."))?;
    Ok((serde_json::from_str(&payload)?, snapshot))
}

/// Restore only the saved meeting metadata, using columns from the local schema.
pub(crate) async fn restore_speakers(
    connection: &mut SqliteConnection,
    meeting_id: &str,
    snapshot: &str,
) -> Result<()> {
    let snapshot: serde_json::Value = serde_json::from_str(snapshot)?;
    for table in [
        "transcript_speakers",
        "speaker_segments",
        "meeting_diarization_status",
    ] {
        let rows = snapshot[table]
            .as_array()
            .ok_or_else(|| anyhow!("The saved speaker snapshot is invalid."))?;
        sqlx::query(&format!("DELETE FROM {table} WHERE meeting_id=?"))
            .bind(meeting_id)
            .execute(&mut *connection)
            .await?;
        if let Some(first) = rows.first() {
            let schema = sqlx::query(&format!("PRAGMA table_info({table})"))
                .fetch_all(&mut *connection)
                .await?;
            let columns: Vec<String> = schema
                .iter()
                .map(|r| r.get::<String, _>("name"))
                .filter(|n| first.get(n).is_some())
                .collect();
            let names = columns
                .iter()
                .map(|c| format!("\"{c}\""))
                .collect::<Vec<_>>()
                .join(",");
            let values = columns
                .iter()
                .map(|c| format!("json_extract(value,'$.{c}')"))
                .collect::<Vec<_>>()
                .join(",");
            sqlx::query(&format!(
                "INSERT INTO {table} ({names}) SELECT {values} FROM json_each(?)"
            ))
            .bind(serde_json::to_string(rows)?)
            .execute(&mut *connection)
            .await?;
        }
    }
    Ok(())
}

pub(crate) async fn undo(
    core: &SqlitePool,
    feature: &SqlitePool,
    meeting_id: &str,
    id: &str,
) -> Result<Proposal> {
    let (proposal, applied_speakers) = load_applied(feature, meeting_id, id).await?;
    let current = crate::diarization::finalize::load_transcripts(core, meeting_id).await?;
    if serde_json::to_string(&current)? != proposal.publication_rows
        || speaker_snapshot(feature, meeting_id).await? != applied_speakers
    {
        return Err(anyhow!("The transcript or speaker labels changed after Enhance. Undo would replace those edits, so they were kept."));
    }
    super::enhancement::publish_undo(core, feature, meeting_id, &current, &proposal, id).await?;
    Ok(proposal)
}

pub(crate) fn original_segments(proposal: &Proposal) -> Result<Vec<TranscriptSegment>> {
    let snapshot: serde_json::Value = serde_json::from_str(&proposal.speaker_backup)?;
    proposal
        .original
        .iter()
        .map(|row| {
            let mut value = serde_json::to_value(row)?;
            let object = value.as_object_mut().unwrap();
            object.insert(
                "text".into(),
                serde_json::Value::String(row.transcript.clone()),
            );
            if let Some(metadata) = snapshot["transcript_speakers"]
                .as_array()
                .and_then(|rows| rows.iter().find(|meta| meta["transcript_id"] == row.id))
            {
                for (key, value) in metadata.as_object().unwrap() {
                    if key.starts_with("speaker_") || key.starts_with("diarization_") {
                        object.insert(key.clone(), value.clone());
                    }
                }
                object.insert(
                    "is_overlap".into(),
                    serde_json::json!(metadata["is_overlap"].as_i64().unwrap_or(0) != 0),
                );
                if let Some(ids) = metadata["candidate_speaker_ids"].as_str() {
                    object.insert("candidate_speaker_ids".into(), serde_json::from_str(ids)?);
                }
            }
            Ok(serde_json::from_value(value)?)
        })
        .collect()
}

pub(crate) async fn load(feature: &SqlitePool, meeting_id: &str, id: &str) -> Result<Proposal> {
    let payload: Option<String> = sqlx::query_scalar("SELECT payload FROM enhancement_reviews WHERE id = ? AND meeting_id = ? AND state = 'pending'")
        .bind(id).bind(meeting_id).fetch_optional(feature).await?;
    Ok(serde_json::from_str(&payload.ok_or_else(|| {
        anyhow!("The proposed transcript is no longer available.")
    })?)?)
}

pub(crate) async fn discard(feature: &SqlitePool, meeting_id: &str, id: &str) -> Result<()> {
    sqlx::query(
        "DELETE FROM enhancement_reviews WHERE id = ? AND meeting_id = ? AND state = 'pending'",
    )
    .bind(id)
    .bind(meeting_id)
    .execute(feature)
    .await?;
    Ok(())
}
