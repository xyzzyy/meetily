use crate::api::{TranscriptSearchResult, TranscriptSegment};
use chrono::Utc;
use sqlx::{Connection, Error as SqlxError, SqlitePool};
use tracing::{error, info};
use uuid::Uuid;

pub struct TranscriptsRepository;

impl TranscriptsRepository {
    /// Saves a new meeting and its associated transcript segments.
    /// This function uses a transaction to ensure that either both the meeting
    /// and all its transcripts are saved, or none of them are.
    pub async fn save_transcript(
        pool: &SqlitePool,
        meeting_title: &str,
        transcripts: &[TranscriptSegment],
        folder_path: Option<String>,
    ) -> Result<String, SqlxError> {
        let meeting_id = format!("meeting-{}", Uuid::new_v4());

        let mut conn = pool.acquire().await?;
        let mut transaction = conn.begin().await?;

        let now = Utc::now();

        // 1. Create the new meeting
        let result = sqlx::query(
            "INSERT INTO meetings (id, title, created_at, updated_at, folder_path) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&meeting_id)
        .bind(meeting_title)
        .bind(now)
        .bind(now)
        .bind(&folder_path)
        .execute(&mut *transaction)
        .await;

        if let Err(e) = result {
            error!("Failed to create meeting '{}': {}", meeting_title, e);
            transaction.rollback().await?;
            return Err(e);
        }

        info!("Successfully created meeting with id: {}", meeting_id);

        // 2. Save each transcript segment with audio timing fields
        for segment in transcripts {
            let transcript_id = format!("transcript-{}", Uuid::new_v4());
            let result = sqlx::query(
                "INSERT INTO transcripts (id, meeting_id, transcript, timestamp, audio_start_time, audio_end_time, duration)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&transcript_id)
            .bind(&meeting_id)
            .bind(&segment.text)
            .bind(&segment.timestamp)
            .bind(segment.audio_start_time)
            .bind(segment.audio_end_time)
            .bind(segment.duration)
            .execute(&mut *transaction)
            .await;

            if let Err(e) = result {
                error!(
                    "Failed to save transcript segment for meeting {}: {}",
                    meeting_id, e
                );
                transaction.rollback().await?;
                return Err(e);
            }
        }

        info!(
            "Successfully saved {} transcript segments for meeting {}",
            transcripts.len(),
            meeting_id
        );

        // Commit the transaction
        transaction.commit().await?;

        Ok(meeting_id)
    }

    /// Searches for a query string within the transcripts.
    /// It returns a list of matching transcripts with context.
    pub async fn search_transcripts(
        pool: &SqlitePool,
        query: &str,
    ) -> Result<Vec<TranscriptSearchResult>, SqlxError> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }

        let search_query = format!("%{}%", query.to_lowercase());

        let rows = sqlx::query_as::<_, (String, String, String, String)>(
            "SELECT m.id, m.title, t.transcript, t.timestamp
             FROM meetings m
             JOIN transcripts t ON m.id = t.meeting_id
             WHERE LOWER(t.transcript) LIKE ?",
        )
        .bind(&search_query)
        .fetch_all(pool)
        .await?;

        let results = rows
            .into_iter()
            .map(|(id, title, transcript, timestamp)| {
                let match_context = Self::get_match_context(&transcript, query);
                TranscriptSearchResult {
                    id,
                    title,
                    match_context,
                    timestamp,
                }
            })
            .collect();

        Ok(results)
    }

    /// Helper function to extract a snippet of text around the first match of a query.
    fn get_match_context(transcript: &str, query: &str) -> String {
        const CONTEXT_BYTES: usize = 100;

        match find_case_insensitive(transcript, &query.to_lowercase()) {
            Some((match_start, match_end)) => {
                let start_index = floor_char_boundary(
                    transcript,
                    match_start.saturating_sub(CONTEXT_BYTES),
                );
                let end_index = ceil_char_boundary(
                    transcript,
                    (match_end + CONTEXT_BYTES).min(transcript.len()),
                );

                let mut context = String::new();
                if start_index > 0 {
                    context.push_str("...");
                }
                context.push_str(&transcript[start_index..end_index]);
                if end_index < transcript.len() {
                    context.push_str("...");
                }
                context
            }
            None => transcript.chars().take(200).collect(), // Fallback to the start of the transcript
        }
    }
}

/// Byte range of the first case-insensitive match of `query_lower` in `text`.
/// Searching the original text keeps the offsets valid where lowercasing changes
/// a character's length (for example the Turkish capital dotted I).
fn find_case_insensitive(text: &str, query_lower: &str) -> Option<(usize, usize)> {
    if query_lower.is_empty() {
        return Some((0, 0));
    }
    for (start, _) in text.char_indices() {
        let mut lowered = String::new();
        let mut end = start;
        for (offset, character) in text[start..].char_indices() {
            lowered.extend(character.to_lowercase());
            end = start + offset + character.len_utf8();
            if lowered.len() >= query_lower.len() {
                break;
            }
        }
        if lowered == query_lower {
            return Some((start, end));
        }
    }
    None
}

/// Largest character boundary at or below `index`.
fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Smallest character boundary at or above `index`.
fn ceil_char_boundary(text: &str, mut index: usize) -> usize {
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    #[test]
    fn match_context_never_splits_accented_text() {
        let words = ["la reunión", "planificación", "del año", "próximo", "cuánto", "también", "equipo"];
        for prefix in 0..60 {
            let mut before = String::new();
            let mut word = prefix;
            while before.len() < 150 + prefix {
                before.push_str(words[word % words.len()]);
                before.push(' ');
                word += 3;
            }
            let after = "y luego revisamos también el próximo año con el equipo completo ".repeat(3);
            let text = format!("{before}presupuesto {after}");
            let context = TranscriptsRepository::get_match_context(&text, "presupuesto");
            assert!(context.contains("presupuesto"), "prefix {prefix}: {context}");
        }
    }

    #[test]
    fn match_context_is_case_insensitive_and_keeps_surrounding_text() {
        let context = TranscriptsRepository::get_match_context("Hola, el PRESUPUESTO es claro.", "presupuesto");
        assert_eq!(context, "Hola, el PRESUPUESTO es claro.");
    }

    #[test]
    fn match_context_handles_characters_whose_lowercase_is_longer() {
        // The dotted capital I lowercases to two characters, shifting byte offsets.
        let text = format!("{} budget review {}", "İ".repeat(80), "x".repeat(150));
        let context = TranscriptsRepository::get_match_context(&text, "budget");
        assert!(context.contains("budget"));
    }

    #[test]
    fn match_context_falls_back_to_the_start_without_a_match() {
        let text = "é".repeat(300);
        assert_eq!(TranscriptsRepository::get_match_context(&text, "zzz").chars().count(), 200);
    }

    async fn migrated_test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("test database should open");
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("migrations should run");
        pool
    }

    #[tokio::test]
    async fn saves_only_upstream_transcript_fields() {
        let pool = migrated_test_pool().await;

        let segment = TranscriptSegment {
            id: "live-segment-1".to_string(),
            text: "Hello from Alice".to_string(),
            timestamp: "00:01".to_string(),
            audio_start_time: Some(1.0),
            audio_end_time: Some(2.5),
            duration: Some(1.5),
            speaker_id: Some("speaker-1".to_string()),
            speaker_label: Some("Alice".to_string()),
            speaker_profile_id: None,
            speaker_source: Some("system".to_string()),
            speaker_color: Some("#3b82f6".to_string()),
            is_overlap: Some(false),
            candidate_speaker_ids: Some(Vec::new()),
            diarization_status: Some("provisional".to_string()),
            diarization_method: Some("wespeaker_live".to_string()),
            diarization_confidence: Some(0.9),
        };

        let meeting_id = TranscriptsRepository::save_transcript(
            &pool,
            "Save regression test",
            &[segment],
            Some("/tmp/meeting".to_string()),
        )
        .await
        .expect("meeting and transcript should save atomically");

        let saved: (String, Option<f64>, Option<f64>, Option<f64>) = sqlx::query_as(
            "SELECT transcript, audio_start_time, audio_end_time, duration
             FROM transcripts WHERE meeting_id = ?",
        )
        .bind(meeting_id)
        .fetch_one(&pool)
        .await
        .expect("saved transcript should be readable");
        assert_eq!(saved, ("Hello from Alice".to_string(), Some(1.0), Some(2.5), Some(1.5)));

        let custom_columns: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('transcripts')
             WHERE name IN ('speaker_id', 'speaker_label', 'diarization_status')",
        )
        .fetch_one(&pool)
        .await
        .expect("schema should be inspectable");
        assert_eq!(custom_columns, 0);
    }

    #[tokio::test]
    async fn saves_meeting_without_transcript_rows() {
        let pool = migrated_test_pool().await;
        let meeting_id = TranscriptsRepository::save_transcript(
            &pool,
            "Empty meeting",
            &[],
            None,
        )
        .await
        .expect("empty meeting should still save");

        let meeting_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM meetings WHERE id = ?")
            .bind(&meeting_id)
            .fetch_one(&pool)
            .await
            .expect("meeting count should be readable");
        let transcript_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM transcripts WHERE meeting_id = ?")
                .bind(&meeting_id)
                .fetch_one(&pool)
                .await
                .expect("transcript count should be readable");
        assert_eq!((meeting_count, transcript_count), (1, 0));
    }

    #[tokio::test]
    async fn rolls_back_meeting_when_a_transcript_insert_fails() {
        let pool = migrated_test_pool().await;
        sqlx::query(
            "CREATE TRIGGER reject_transcript BEFORE INSERT ON transcripts
             BEGIN SELECT RAISE(ABORT, 'forced transcript failure'); END",
        )
        .execute(&pool)
        .await
        .expect("failure trigger should be created");

        let segment = TranscriptSegment {
            id: "live-segment-1".to_string(),
            text: "This insert must fail".to_string(),
            timestamp: "00:01".to_string(),
            audio_start_time: Some(1.0),
            audio_end_time: Some(2.0),
            duration: Some(1.0),
            speaker_id: None,
            speaker_label: None,
            speaker_profile_id: None,
            speaker_source: None,
            speaker_color: None,
            is_overlap: Some(false),
            candidate_speaker_ids: None,
            diarization_status: None,
            diarization_method: None,
            diarization_confidence: None,
        };

        TranscriptsRepository::save_transcript(&pool, "Rollback meeting", &[segment], None)
            .await
            .expect_err("forced transcript failure should abort the transaction");
        let meeting_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM meetings")
            .fetch_one(&pool)
            .await
            .expect("meeting count should be readable");
        assert_eq!(meeting_count, 0);
    }
}
