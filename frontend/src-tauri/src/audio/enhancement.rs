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

/// Split only at known turn changes, then bound long input at quiet points.
/// Remove the shared splitter's context overlap so Enhance cannot repeat words.
pub(crate) fn input_chunks(
    segment: &super::vad::SpeechSegment,
    max_samples: usize,
    turns: &[f64],
) -> Vec<super::vad::SpeechSegment> {
    let duration = segment.end_timestamp_ms - segment.start_timestamp_ms;
    if segment.samples.is_empty() || duration <= 0.0 {
        return Vec::new();
    }
    let mut cuts = vec![0];
    for seconds in turns {
        let fraction = (seconds * 1000.0 - segment.start_timestamp_ms) / duration;
        if fraction > 0.0 && fraction < 1.0 {
            cuts.push((fraction * segment.samples.len() as f64).round() as usize);
        }
    }
    cuts.push(segment.samples.len());
    cuts.sort_unstable();
    cuts.dedup();
    let mut chunks = Vec::new();
    for pair in cuts.windows(2) {
        let piece = super::vad::SpeechSegment {
            samples: segment.samples[pair[0]..pair[1]].to_vec(),
            start_timestamp_ms: segment.start_timestamp_ms
                + duration * pair[0] as f64 / segment.samples.len() as f64,
            end_timestamp_ms: segment.start_timestamp_ms
                + duration * pair[1] as f64 / segment.samples.len() as f64,
            confidence: segment.confidence,
        };
        let mut cursor = piece.start_timestamp_ms;
        for mut chunk in super::common::split_segment_at_silence(&piece, max_samples) {
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

/// Text and speaker metadata share one SQLite transaction. No new row IDs are
/// visible without their labels. A dedicated connection keeps ATTACH local.
pub(crate) async fn publish(
    core: &SqlitePool,
    feature: &SqlitePool,
    meeting_id: &str,
    original: &[Transcript],
    segments: &[TranscriptSegment],
    labels: Option<&EnhancementSpeakerLabels>,
    cancelled: &AtomicBool,
) -> Result<()> {
    if segments.is_empty() {
        return Err(anyhow!(
            "No transcript text was found. The saved transcript was kept."
        ));
    }
    let feature_path: String =
        sqlx::query_scalar("SELECT file FROM pragma_database_list WHERE name = 'main'")
            .fetch_one(feature)
            .await?;
    if feature_path.is_empty() {
        return Err(anyhow!("Speaker storage has no database file"));
    }
    let mut connection = SqliteConnection::connect_with(&core.connect_options()).await?;
    sqlx::query("ATTACH DATABASE ? AS speaker_feature")
        .bind(&feature_path)
        .execute(&mut connection)
        .await?;
    let mut tx = connection.begin().await?;
    let current: Vec<Transcript> = sqlx::query_as("SELECT id, meeting_id, transcript, timestamp, summary, action_items, key_points,
        audio_start_time, audio_end_time, duration FROM transcripts WHERE meeting_id = ? ORDER BY audio_start_time, id")
        .bind(meeting_id).fetch_all(&mut *tx).await?;
    if serde_json::to_string(&current)? != serde_json::to_string(original)? {
        return Err(anyhow!(
            "The transcript changed during enhancement. The saved transcript was kept."
        ));
    }
    sqlx::query("DELETE FROM transcripts WHERE meeting_id = ?")
        .bind(meeting_id)
        .execute(&mut *tx)
        .await?;
    for segment in segments {
        sqlx::query("INSERT INTO transcripts (id, meeting_id, transcript, timestamp, audio_start_time, audio_end_time, duration)
            VALUES (?, ?, ?, ?, ?, ?, ?)").bind(&segment.id).bind(meeting_id).bind(&segment.text).bind(&segment.timestamp)
            .bind(segment.audio_start_time).bind(segment.audio_end_time).bind(segment.duration).execute(&mut *tx).await?;
    }
    if let Some(labels) = labels {
        labels.write(&mut tx, meeting_id).await?;
    } else {
        sqlx::query("DELETE FROM speaker_feature.transcript_speakers WHERE meeting_id = ?")
            .bind(meeting_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM speaker_feature.speaker_segments WHERE meeting_id = ?")
            .bind(meeting_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO speaker_feature.meeting_diarization_status (meeting_id, current_status, progress)
            VALUES (?, 'none', 0) ON CONFLICT(meeting_id) DO UPDATE SET current_status = 'none', progress = 0,
            failure_reason = NULL, run_id = NULL, assigned_coverage = NULL, transcription_success_rate = NULL,
            updated_at = CURRENT_TIMESTAMP").bind(meeting_id).execute(&mut *tx).await?;
    }
    if cancelled.load(Ordering::SeqCst) {
        return Err(anyhow!(
            "Enhancement cancelled. The saved transcript was kept."
        ));
    }
    tx.commit().await?;
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

    #[test]
    fn input_chunks_cut_at_turn_boundaries() {
        let audio = speech_segment(0.0, 1000.0, 16_000);
        let chunks = input_chunks(&audio, 25 * 16_000, &[0.5]);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].samples.len(), 8_000);
        assert_eq!(chunks[0].start_timestamp_ms, 0.0);
        assert_eq!(chunks[0].end_timestamp_ms, 500.0);
        assert_eq!(chunks[1].samples.len(), 8_000);
        assert_eq!(chunks[1].start_timestamp_ms, 500.0);
        assert_eq!(chunks[1].end_timestamp_ms, 1000.0);
    }

    #[test]
    fn input_chunks_ignore_turns_outside_the_segment() {
        let audio = speech_segment(10_000.0, 11_000.0, 16_000);
        let chunks = input_chunks(&audio, 25 * 16_000, &[5.0, 25.0]);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].start_timestamp_ms, 10_000.0);
        assert_eq!(chunks[0].end_timestamp_ms, 11_000.0);
    }

    #[test]
    fn input_chunks_ignore_empty_segments() {
        let empty = speech_segment(0.0, 1000.0, 0);
        assert!(input_chunks(&empty, 25 * 16_000, &[]).is_empty());

        let zero_length = speech_segment(0.0, 0.0, 16_000);
        assert!(input_chunks(&zero_length, 25 * 16_000, &[]).is_empty());
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
        let connect = |name: &str| SqliteConnectOptions::new().filename(dir.path().join(name)).create_if_missing(true);
        let core = SqlitePoolOptions::new().connect_with(connect("core.sqlite")).await.unwrap();
        let feature = SqlitePoolOptions::new().connect_with(connect("speaker.sqlite")).await.unwrap();
        sqlx::migrate!("./migrations").run(&core).await.unwrap();
        sqlx::migrate!("./speaker-migrations").run(&feature).await.unwrap();
        sqlx::query("INSERT INTO meetings(id,title,created_at,updated_at) VALUES ('m1','Test',CURRENT_TIMESTAMP,CURRENT_TIMESTAMP)")
            .execute(&core).await.unwrap();
        sqlx::query("INSERT INTO transcripts(id,meeting_id,transcript,timestamp,audio_start_time,audio_end_time,duration) VALUES ('old','m1','Old merged text','old time',0,88,88)")
            .execute(&core).await.unwrap();
        sqlx::query("INSERT INTO transcript_speakers(transcript_id,meeting_id,speaker_id,speaker_label,diarization_status,published) VALUES ('old','m1','old-speaker','Speaker 1','final',1)")
            .execute(&feature).await.unwrap();
        let original = crate::diarization::finalize::load_transcripts(&core,"m1").await.unwrap();
        (dir,core,feature,original)
    }

    fn fresh_segments() -> Vec<TranscriptSegment> {
        let result = timed("Alice. Bob.", &[" Alice.", " Bob."], &[1.25, 15.5]);
        let rows = parakeet_rows(&result, 0.0, 25_000.0, None).unwrap();
        super::super::common::create_transcript_segments(&rows)
    }

    #[tokio::test]
    async fn fresh_model_text_and_times_replace_a_long_old_row() {
        let (_dir,core,feature,original) = publication_fixture().await;
        let segments = fresh_segments();
        publish(&core,&feature,"m1",&original,&segments,None,&AtomicBool::new(false)).await.unwrap();
        let rows = crate::diarization::finalize::load_transcripts(&core,"m1").await.unwrap();
        assert_eq!(rows.len(),2);
        assert_eq!(rows[0].transcript,"Alice.");
        assert_eq!(rows[1].transcript,"Bob.");
        assert_eq!(rows[0].audio_start_time,Some(1.25));
        assert_eq!(rows[1].audio_start_time,Some(15.5));
        assert!(rows.iter().all(|row| row.audio_end_time.unwrap() < 16.0 && row.id != "old"));
        let stale: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM transcript_speakers WHERE meeting_id='m1'")
            .fetch_one(&feature).await.unwrap();
        assert_eq!(stale,0,"old speaker rows must not attach to the new text");
    }

    #[tokio::test]
    async fn failed_or_cancelled_enhancement_keeps_old_text_times_and_speaker() {
        let (_dir,core,feature,original) = publication_fixture().await;
        let segments = fresh_segments();
        sqlx::query("CREATE TRIGGER fail_enhance BEFORE INSERT ON transcripts WHEN NEW.id != 'old' BEGIN SELECT RAISE(ABORT,'test failure'); END")
            .execute(&core).await.unwrap();
        assert!(publish(&core,&feature,"m1",&original,&segments,None,&AtomicBool::new(false)).await.is_err());
        sqlx::query("DROP TRIGGER fail_enhance").execute(&core).await.unwrap();
        assert!(publish(&core,&feature,"m1",&original,&segments,None,&AtomicBool::new(true)).await.is_err());
        let rows = crate::diarization::finalize::load_transcripts(&core,"m1").await.unwrap();
        assert_eq!(serde_json::to_string(&rows).unwrap(),serde_json::to_string(&original).unwrap());
        let label: String = sqlx::query_scalar("SELECT speaker_label FROM transcript_speakers WHERE transcript_id='old'")
            .fetch_one(&feature).await.unwrap();
        assert_eq!(label,"Speaker 1");
    }

}
