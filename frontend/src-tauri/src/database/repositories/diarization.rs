use crate::database::models::DiarizationSetting;
use serde::Serialize;
use sqlx::{Sqlite, SqlitePool};

pub struct DiarizationRepository;

/// Transcript windows loaded from the core database for this meeting.
/// Callers must not accept these values from an unverified client request.
#[derive(Debug, Clone)]
pub struct TranscriptCorrectionWindow {
    pub transcript_id: String,
    pub audio_start_time: Option<f64>,
    pub audio_end_time: Option<f64>,
    pub speaker_source: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingSpeaker {
    pub speaker_id: String,
    pub speaker_label: String,
    pub speaker_color: Option<String>,
    pub segment_count: i64,
    pub verified_segment_count: i64,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSpeakerMetadata {
    pub transcript_id: String,
    pub meeting_id: String,
    pub audio_start_time: Option<f64>,
    pub audio_end_time: Option<f64>,
    pub speaker_id: Option<String>,
    pub speaker_label: Option<String>,
    pub speaker_profile_id: Option<String>,
    pub speaker_source: Option<String>,
    pub speaker_color: Option<String>,
    pub is_overlap: i64,
    pub candidate_speaker_ids: Option<String>,
    pub diarization_status: String,
    pub diarization_method: Option<String>,
    pub diarization_confidence: Option<f64>,
}

impl DiarizationRepository {
    pub async fn get_settings(pool: &SqlitePool) -> Result<DiarizationSetting, sqlx::Error> {
        let existing = sqlx::query_as::<_, DiarizationSetting>(
            "SELECT * FROM diarization_settings WHERE id = '1'",
        )
        .fetch_optional(pool)
        .await?;

        if let Some(settings) = existing {
            return Ok(settings);
        }

        sqlx::query("INSERT INTO diarization_settings (id) VALUES ('1')")
            .execute(pool)
            .await?;

        sqlx::query_as::<_, DiarizationSetting>("SELECT * FROM diarization_settings WHERE id = '1'")
            .fetch_one(pool)
            .await
    }

    pub async fn save_settings(
        pool: &SqlitePool,
        enabled: bool,
        mode: &str,
        show_provisional_labels: bool,
        post_call_refinement_enabled: bool,
        overlap_handling: &str,
        speaker_review_enabled: bool,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO diarization_settings (
                id, enabled, mode, show_provisional_labels,
                post_call_refinement_enabled, overlap_handling,
                speaker_review_enabled, updated_at
             )
             VALUES ('1', ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
             ON CONFLICT(id) DO UPDATE SET
                enabled = excluded.enabled,
                mode = excluded.mode,
                show_provisional_labels = excluded.show_provisional_labels,
                post_call_refinement_enabled = excluded.post_call_refinement_enabled,
                overlap_handling = excluded.overlap_handling,
                speaker_review_enabled = excluded.speaker_review_enabled,
                updated_at = CURRENT_TIMESTAMP",
        )
        .bind(enabled as i64)
        .bind(mode)
        .bind(show_provisional_labels as i64)
        .bind(post_call_refinement_enabled as i64)
        .bind(overlap_handling)
        .bind(speaker_review_enabled as i64)
        .execute(pool)
        .await?;

        Ok(())
    }

    pub async fn update_transcript_assignment<'e, E>(
        executor: E,
        transcript_id: &str,
        meeting_id: &str,
        audio_start_time: Option<f64>,
        audio_end_time: Option<f64>,
        assignment: &crate::diarization::types::SpeakerAssignment,
    ) -> Result<(), sqlx::Error>
    where
        E: sqlx::Executor<'e, Database = Sqlite>,
    {
        let diarization_status =
            diarization_status_to_database_value(assignment.diarization_status);

        sqlx::query(
            "INSERT INTO transcript_speakers (
                transcript_id, meeting_id, audio_start_time, audio_end_time,
                speaker_id, speaker_label, speaker_profile_id, speaker_source,
                speaker_color, is_overlap, candidate_speaker_ids,
                diarization_status, diarization_method, diarization_confidence,
                published, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, CURRENT_TIMESTAMP)
             ON CONFLICT(transcript_id) DO UPDATE SET
                meeting_id = excluded.meeting_id,
                audio_start_time = excluded.audio_start_time,
                audio_end_time = excluded.audio_end_time,
                speaker_id = excluded.speaker_id,
                speaker_label = excluded.speaker_label,
                speaker_profile_id = excluded.speaker_profile_id,
                speaker_source = excluded.speaker_source,
                speaker_color = excluded.speaker_color,
                is_overlap = excluded.is_overlap,
                candidate_speaker_ids = excluded.candidate_speaker_ids,
                diarization_status = excluded.diarization_status,
                diarization_method = excluded.diarization_method,
                diarization_confidence = excluded.diarization_confidence,
                published = 1,
                updated_at = CURRENT_TIMESTAMP",
        )
        .bind(transcript_id)
        .bind(meeting_id)
        .bind(audio_start_time)
        .bind(audio_end_time)
        .bind(&assignment.speaker_id)
        .bind(&assignment.speaker_label)
        .bind(&assignment.speaker_profile_id)
        .bind(&assignment.speaker_source)
        .bind(&assignment.speaker_color)
        .bind(assignment.is_overlap as i64)
        .bind(serde_json::to_string(&assignment.candidate_speaker_ids).ok())
        .bind(diarization_status)
        .bind(&assignment.diarization_method)
        .bind(assignment.diarization_confidence)
        .execute(executor)
        .await?;

        Ok(())
    }

    pub async fn get_meeting_speakers(
        pool: &SqlitePool,
        meeting_id: &str,
    ) -> Result<Vec<MeetingSpeaker>, sqlx::Error> {
        let mut speakers = sqlx::query_as::<_, MeetingSpeaker>(
            "SELECT
                speaker_id AS speaker_id,
                COALESCE(NULLIF(TRIM(MAX(speaker_label)), ''), speaker_id) AS speaker_label,
                MAX(speaker_color) AS speaker_color,
                COUNT(*) AS segment_count,
                SUM(CASE WHEN diarization_method = 'manual' THEN 1 ELSE 0 END) AS verified_segment_count
             FROM transcript_speakers
             WHERE meeting_id = ?
                AND published = 1
                AND is_overlap = 0
                AND speaker_id IS NOT NULL
                AND TRIM(speaker_id) <> ''
             GROUP BY speaker_id
             ORDER BY MIN(COALESCE(audio_start_time, 0)), speaker_id",
        )
        .bind(meeting_id)
        .fetch_all(pool)
        .await?;
        let choices: Vec<(String, String)> = sqlx::query_as(
            "SELECT speaker_id, color FROM meeting_speaker_colors WHERE meeting_id = ?",
        )
        .bind(meeting_id)
        .fetch_all(pool)
        .await?;
        crate::diarization::colors::apply_colors(&mut speakers, &choices.into_iter().collect());
        Ok(speakers)
    }

    pub async fn set_speaker_color(
        pool: &SqlitePool,
        meeting_id: &str,
        speaker_id: &str,
        color: &str,
    ) -> Result<u64, sqlx::Error> {
        Ok(sqlx::query(
            "INSERT INTO meeting_speaker_colors (meeting_id, speaker_id, color)
             SELECT ?, ?, ? WHERE EXISTS (
                 SELECT 1 FROM transcript_speakers
                 WHERE meeting_id = ? AND speaker_id = ? AND is_overlap = 0 AND published = 1
             ) ON CONFLICT(meeting_id, speaker_id) DO UPDATE SET color = excluded.color",
        )
        .bind(meeting_id)
        .bind(speaker_id)
        .bind(color)
        .bind(meeting_id)
        .bind(speaker_id)
        .execute(pool)
        .await?
        .rows_affected())
    }

    pub async fn rename_speaker(
        pool: &SqlitePool,
        meeting_id: &str,
        speaker_id: &str,
        speaker_label: &str,
    ) -> Result<u64, sqlx::Error> {
        let mut transaction = pool.begin().await?;

        let transcript_result = sqlx::query(
            "UPDATE transcript_speakers
             SET speaker_label = ?, diarization_method = 'manual',
                 updated_at = CURRENT_TIMESTAMP
             WHERE meeting_id = ? AND speaker_id = ? AND is_overlap = 0",
        )
        .bind(speaker_label)
        .bind(meeting_id)
        .bind(speaker_id)
        .execute(&mut *transaction)
        .await?;

        let segment_result = sqlx::query(
            "UPDATE speaker_segments
             SET speaker_label = ?, diarization_method = 'manual'
             WHERE meeting_id = ? AND speaker_id = ? AND is_overlap = 0",
        )
        .bind(speaker_label)
        .bind(meeting_id)
        .bind(speaker_id)
        .execute(&mut *transaction)
        .await?;

        sqlx::query(
            "UPDATE meeting_diarization_status
             SET updated_at = CURRENT_TIMESTAMP
             WHERE meeting_id = ?",
        )
        .bind(meeting_id)
        .execute(&mut *transaction)
        .await?;

        transaction.commit().await?;

        Ok(transcript_result.rows_affected() + segment_result.rows_affected())
    }

    pub async fn merge_speakers(
        pool: &SqlitePool,
        meeting_id: &str,
        source_speaker_ids: &[String],
        target_speaker_id: &str,
        target_label: &str,
    ) -> Result<u64, sqlx::Error> {
        let mut transaction = pool.begin().await?;
        let target_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM transcript_speakers
             WHERE meeting_id = ? AND speaker_id = ? AND published = 1 AND is_overlap = 0)",
        )
        .bind(meeting_id).bind(target_speaker_id).fetch_one(&mut *transaction).await?;
        if !target_exists {
            return Err(sqlx::Error::RowNotFound);
        }
        let target_color: Option<String> = sqlx::query_scalar(
            "SELECT MAX(speaker_color) FROM transcript_speakers
             WHERE meeting_id = ? AND speaker_id = ? AND is_overlap = 0 AND published = 1",
        )
        .bind(meeting_id)
        .bind(target_speaker_id)
        .fetch_one(&mut *transaction)
        .await?;
        let mut updated = 0;
        for source_id in source_speaker_ids {
            if source_id == target_speaker_id {
                continue;
            }
            let changed = sqlx::query(
                "UPDATE transcript_speakers SET
                    speaker_id = ?, speaker_label = ?, speaker_profile_id = NULL,
                    speaker_color = ?, diarization_method = 'manual',
                    updated_at = CURRENT_TIMESTAMP
                 WHERE meeting_id = ? AND speaker_id = ? AND is_overlap = 0",
            )
            .bind(target_speaker_id)
            .bind(target_label)
            .bind(&target_color)
            .bind(meeting_id)
            .bind(source_id)
            .execute(&mut *transaction)
            .await?
            .rows_affected();
            updated += changed;
            if changed > 0 {
                super::speaker_profile::SpeakerProfilesRepository::merge_speaker_samples(
                    &mut transaction, meeting_id, source_id, target_speaker_id,
                ).await?;
            }

            sqlx::query(
                "UPDATE speaker_segments SET speaker_id = ?, speaker_label = ?,
                    diarization_method = 'manual'
                 WHERE meeting_id = ? AND speaker_id = ? AND is_overlap = 0",
            )
            .bind(target_speaker_id)
            .bind(target_label)
            .bind(meeting_id)
            .bind(source_id)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(updated)
    }

    pub async fn reassign_transcripts(
        pool: &SqlitePool,
        meeting_id: &str,
        transcript_windows: &[TranscriptCorrectionWindow],
        speaker_id: Option<&str>,
        speaker_label: Option<&str>,
        is_overlap: bool,
        candidate_speaker_ids: &[String],
    ) -> Result<u64, sqlx::Error> {
        let mut transaction = pool.begin().await?;
        let assigned_id = if is_overlap { None } else { speaker_id };
        let target_color: Option<String> = sqlx::query_scalar(
            "SELECT MAX(speaker_color) FROM transcript_speakers
             WHERE meeting_id = ? AND speaker_id = ? AND published = 1",
        )
        .bind(meeting_id)
        .bind(assigned_id)
        .fetch_one(&mut *transaction)
        .await?;
        let candidates = serde_json::to_string(if is_overlap {
            candidate_speaker_ids
        } else {
            &[]
        })
        .unwrap_or_else(|_| "[]".to_string());
        let mut updated = 0;
        for window in transcript_windows {
            // Corrections are the source of truth for voice enrollment. The original
            // speaker_segments remain analysis output and must not supply training audio.
            updated += sqlx::query(
                "INSERT INTO transcript_speakers (
                    transcript_id, meeting_id, audio_start_time, audio_end_time,
                    speaker_id, speaker_label, speaker_profile_id, speaker_source,
                    speaker_color, is_overlap, candidate_speaker_ids,
                    diarization_status, diarization_method, diarization_confidence,
                    published, updated_at
                 ) VALUES (?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, 'final', 'manual', 1.0,
                           1, CURRENT_TIMESTAMP)
                 ON CONFLICT(transcript_id) DO UPDATE SET
                    audio_start_time = excluded.audio_start_time,
                    audio_end_time = excluded.audio_end_time,
                    speaker_id = excluded.speaker_id,
                    speaker_label = excluded.speaker_label,
                    speaker_profile_id = NULL,
                    speaker_source = COALESCE(excluded.speaker_source,
                                              transcript_speakers.speaker_source),
                    speaker_color = excluded.speaker_color,
                    is_overlap = excluded.is_overlap,
                    candidate_speaker_ids = excluded.candidate_speaker_ids,
                    diarization_status = 'final', diarization_method = 'manual',
                    diarization_confidence = 1.0, published = 1,
                    updated_at = CURRENT_TIMESTAMP
                 WHERE transcript_speakers.meeting_id = excluded.meeting_id",
            )
            .bind(&window.transcript_id)
            .bind(meeting_id)
            .bind(window.audio_start_time)
            .bind(window.audio_end_time)
            .bind(assigned_id)
            .bind(if is_overlap {
                Some("Multiple speakers")
            } else {
                speaker_label
            })
            .bind(&window.speaker_source)
            .bind(&target_color)
            .bind(is_overlap as i64)
            .bind(&candidates)
            .execute(&mut *transaction)
            .await?
            .rows_affected();
        }
        transaction.commit().await?;
        Ok(updated)
    }

    pub async fn get_transcript_metadata(
        pool: &SqlitePool,
        transcript_ids: &[String],
    ) -> Result<Vec<TranscriptSpeakerMetadata>, sqlx::Error> {
        if transcript_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut builder = sqlx::QueryBuilder::<Sqlite>::new(
            "SELECT transcript_id, meeting_id, audio_start_time, audio_end_time, \
             speaker_id, speaker_label, speaker_profile_id, speaker_source, \
             speaker_color, is_overlap, candidate_speaker_ids, diarization_status, \
             diarization_method, diarization_confidence FROM transcript_speakers \
             WHERE published = 1 AND transcript_id IN (",
        );
        let mut separated = builder.separated(", ");
        for id in transcript_ids {
            separated.push_bind(id);
        }
        separated.push_unseparated(")");
        let mut rows: Vec<TranscriptSpeakerMetadata> =
            builder.build_query_as().fetch_all(pool).await?;
        let meetings: std::collections::BTreeSet<_> =
            rows.iter().map(|row| row.meeting_id.clone()).collect();
        for meeting_id in meetings {
            let colors: std::collections::HashMap<_, _> =
                Self::get_meeting_speakers(pool, &meeting_id)
                    .await?
                    .into_iter()
                    .map(|speaker| (speaker.speaker_id, speaker.speaker_color))
                    .collect();
            for row in rows.iter_mut().filter(|row| row.meeting_id == meeting_id) {
                if let Some(color) = row.speaker_id.as_ref().and_then(|id| colors.get(id)) {
                    row.speaker_color = color.clone();
                }
            }
        }
        Ok(rows)
    }

    pub async fn delete_meeting_data(
        pool: &SqlitePool,
        meeting_id: &str,
    ) -> Result<(), sqlx::Error> {
        let mut transaction = pool.begin().await?;
        for table in [
            "transcript_speakers",
            "meeting_speaker_colors",
            "speaker_segments",
            "meeting_diarization_status",
            "diarization_runs",
        ] {
            let query = format!("DELETE FROM {table} WHERE meeting_id = ?");
            sqlx::query(&query)
                .bind(meeting_id)
                .execute(&mut *transaction)
                .await?;
        }
        transaction.commit().await
    }
}

pub(crate) fn diarization_status_to_database_value(
    status: crate::diarization::types::DiarizationStatus,
) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "none".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diarization::types::{DiarizationStatus, SpeakerAssignment};
    use sqlx::sqlite::SqlitePoolOptions;

    async fn transcript_test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("test pool should connect");

        sqlx::query(
            "CREATE TABLE transcript_speakers (
                transcript_id TEXT PRIMARY KEY,
                meeting_id TEXT NOT NULL,
                audio_start_time REAL,
                audio_end_time REAL,
                speaker_id TEXT,
                speaker_label TEXT,
                speaker_profile_id TEXT,
                speaker_source TEXT,
                speaker_color TEXT,
                is_overlap INTEGER NOT NULL DEFAULT 0,
                candidate_speaker_ids TEXT,
                diarization_status TEXT NOT NULL DEFAULT 'none',
                diarization_method TEXT,
                diarization_confidence REAL,
                published INTEGER NOT NULL DEFAULT 1,
                updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
            )",
        )
        .execute(&pool)
        .await
        .expect("transcripts table should be created");

        sqlx::raw_sql(include_str!(
            "../../../speaker-migrations/20261019000000_speaker_colors.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    async fn speaker_test_pool() -> SqlitePool {
        let pool = transcript_test_pool().await;

        sqlx::query("CREATE TABLE speaker_profile_samples (
            profile_id TEXT NOT NULL, meeting_id TEXT NOT NULL, speaker_id TEXT NOT NULL,
            embedding BLOB, sample_count INTEGER NOT NULL DEFAULT 0, speech_seconds REAL NOT NULL,
            PRIMARY KEY (profile_id, meeting_id, speaker_id))")
            .execute(&pool).await.unwrap();

        sqlx::query(
            "CREATE TABLE speaker_segments (
                id TEXT PRIMARY KEY,
                meeting_id TEXT NOT NULL,
                source TEXT NOT NULL,
                start_time REAL NOT NULL,
                end_time REAL NOT NULL,
                speaker_id TEXT,
                speaker_label TEXT,
                confidence REAL,
                is_overlap INTEGER NOT NULL DEFAULT 0,
                diarization_status TEXT NOT NULL,
                diarization_method TEXT,
                created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
            )",
        )
        .execute(&pool)
        .await
        .expect("speaker_segments table should be created");

        sqlx::query(
            "CREATE TABLE meeting_diarization_status (
                meeting_id TEXT PRIMARY KEY NOT NULL,
                updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
            )",
        )
        .execute(&pool)
        .await
        .expect("meeting_diarization_status table should be created");

        sqlx::query("INSERT INTO meeting_diarization_status (meeting_id) VALUES ('meeting-1')")
            .execute(&pool)
            .await
            .expect("meeting diarization status should be inserted");

        for (id, meeting_id, speaker_id, speaker_label, speaker_color, audio_start_time) in [
            (
                "t-1",
                "meeting-1",
                Some("speaker-1"),
                Some("Speaker 1"),
                Some("#2563eb"),
                0.0,
            ),
            (
                "t-2",
                "meeting-1",
                Some("speaker-1"),
                Some("Speaker 1"),
                Some("#2563eb"),
                1.0,
            ),
            (
                "t-3",
                "meeting-1",
                Some("speaker-2"),
                Some("Speaker 2"),
                Some("#16a34a"),
                2.0,
            ),
            (
                "t-4",
                "meeting-2",
                Some("speaker-1"),
                Some("Other meeting speaker"),
                Some("#dc2626"),
                3.0,
            ),
            ("t-5", "meeting-1", None, None, None, 4.0),
        ] {
            sqlx::query(
                "INSERT INTO transcript_speakers (
                    transcript_id, meeting_id, speaker_id, speaker_label, speaker_color, audio_start_time
                 )
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(id)
            .bind(meeting_id)
            .bind(speaker_id)
            .bind(speaker_label)
            .bind(speaker_color)
            .bind(audio_start_time)
            .execute(&pool)
            .await
            .expect("transcript row should be inserted");
        }

        for (id, meeting_id, speaker_id, speaker_label) in [
            ("s-1", "meeting-1", Some("speaker-1"), Some("Speaker 1")),
            ("s-2", "meeting-1", Some("speaker-1"), Some("Speaker 1")),
            ("s-3", "meeting-1", Some("speaker-2"), Some("Speaker 2")),
            (
                "s-4",
                "meeting-2",
                Some("speaker-1"),
                Some("Other meeting speaker"),
            ),
        ] {
            sqlx::query(
                "INSERT INTO speaker_segments (
                    id, meeting_id, source, start_time, end_time,
                    speaker_id, speaker_label, diarization_status
                 )
                 VALUES (?, ?, 'unit_test', 0, 1, ?, ?, 'final')",
            )
            .bind(id)
            .bind(meeting_id)
            .bind(speaker_id)
            .bind(speaker_label)
            .execute(&pool)
            .await
            .expect("speaker segment row should be inserted");
        }

        pool
    }

    #[tokio::test]
    async fn update_transcript_assignment_persists_assignment_fields() {
        let pool = transcript_test_pool().await;
        let assignment = SpeakerAssignment {
            speaker_id: Some("speaker-1".to_string()),
            speaker_label: Some("Speaker 1".to_string()),
            speaker_profile_id: None,
            speaker_source: Some("system".to_string()),
            speaker_color: Some("#2563eb".to_string()),
            is_overlap: true,
            candidate_speaker_ids: vec!["speaker-1".to_string(), "speaker-2".to_string()],
            diarization_status: DiarizationStatus::Final,
            diarization_method: Some("pyannote-rs".to_string()),
            diarization_confidence: Some(0.82),
        };

        DiarizationRepository::update_transcript_assignment(
            &pool,
            "transcript-1",
            "meeting-1",
            Some(1.0),
            Some(2.0),
            &assignment,
        )
        .await
        .expect("assignment should be persisted");

        let row: (
            Option<String>,
            Option<String>,
            Option<String>,
            i64,
            String,
            Option<String>,
            Option<f64>,
        ) = sqlx::query_as(
            "SELECT
                speaker_id,
                speaker_label,
                speaker_color,
                is_overlap,
                diarization_status,
                diarization_method,
                diarization_confidence
             FROM transcript_speakers WHERE transcript_id = 'transcript-1'",
        )
        .fetch_one(&pool)
        .await
        .expect("updated transcript row should be fetched");

        assert_eq!(row.0.as_deref(), Some("speaker-1"));
        assert_eq!(row.1.as_deref(), Some("Speaker 1"));
        assert_eq!(row.2.as_deref(), Some("#2563eb"));
        assert_eq!(row.3, 1);
        assert_eq!(row.4, "final");
        assert_eq!(row.5.as_deref(), Some("pyannote-rs"));
        assert_eq!(row.6, Some(0.82));
    }

    #[tokio::test]
    async fn get_meeting_speakers_returns_distinct_speakers_for_full_meeting() {
        let pool = speaker_test_pool().await;

        let speakers = DiarizationRepository::get_meeting_speakers(&pool, "meeting-1")
            .await
            .expect("speakers should be loaded");

        assert_eq!(speakers.len(), 2);
        assert_eq!(speakers[0].speaker_id, "speaker-1");
        assert_eq!(speakers[0].speaker_label, "Speaker 1");
        assert_eq!(speakers[0].speaker_color.as_deref(), Some("#2563eb"));
        assert_eq!(speakers[0].segment_count, 2);
        assert_eq!(speakers[1].speaker_id, "speaker-2");
        assert_eq!(speakers[1].speaker_label, "Speaker 2");
        assert_eq!(speakers[1].segment_count, 1);
    }

    #[tokio::test]
    async fn legacy_overlap_rows_are_not_individual_speakers() {
        let pool = speaker_test_pool().await;
        sqlx::query("UPDATE transcript_speakers SET is_overlap = 1, speaker_id = 'multiple-speakers', speaker_label = 'Multiple speakers' WHERE transcript_id = 't-1'")
            .execute(&pool).await.unwrap();
        let speakers = DiarizationRepository::get_meeting_speakers(&pool, "meeting-1").await.unwrap();
        assert!(speakers.iter().all(|speaker| speaker.speaker_id != "multiple-speakers"));
        assert_eq!(speakers[0].segment_count, 1);
        assert_eq!(DiarizationRepository::rename_speaker(&pool, "meeting-1", "multiple-speakers", "Alice").await.unwrap(), 0);
        assert_eq!(DiarizationRepository::merge_speakers(&pool, "meeting-1", &["multiple-speakers".into()], "speaker-2", "Bob").await.unwrap(), 0);
        let label: String = sqlx::query_scalar("SELECT speaker_label FROM transcript_speakers WHERE transcript_id = 't-1'")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(label, "Multiple speakers");
    }

    #[tokio::test]
    async fn speaker_verification_counts_only_manual_rows() {
        let pool = speaker_test_pool().await;
        sqlx::query("UPDATE transcript_speakers SET diarization_method = 'manual' WHERE transcript_id = 't-1'")
            .execute(&pool).await.unwrap();
        let speakers = DiarizationRepository::get_meeting_speakers(&pool, "meeting-1").await.unwrap();
        assert_eq!(speakers[0].segment_count, 2);
        assert_eq!(speakers[0].verified_segment_count, 1);
        assert_eq!(speakers[1].verified_segment_count, 0);
        DiarizationRepository::rename_speaker(&pool, "meeting-1", "speaker-1", "Alice").await.unwrap();
        let speakers = DiarizationRepository::get_meeting_speakers(&pool, "meeting-1").await.unwrap();
        assert_eq!(speakers[0].verified_segment_count, speakers[0].segment_count);
    }

    #[tokio::test]
    async fn color_choices_are_scoped_and_survive_new_analysis_rows() {
        let pool = speaker_test_pool().await;
        let before = DiarizationRepository::get_meeting_speakers(&pool, "meeting-1")
            .await
            .unwrap();
        assert_eq!(
            DiarizationRepository::set_speaker_color(&pool, "meeting-1", "speaker-1", "#be185d")
                .await
                .unwrap(),
            1
        );
        let after = DiarizationRepository::get_meeting_speakers(&pool, "meeting-1")
            .await
            .unwrap();
        assert_eq!(after[0].speaker_color.as_deref(), Some("#be185d"));
        assert_eq!(after[1].speaker_color, before[1].speaker_color);
        // A rerun replaces metadata with the analysis default, but keeps the choice table.
        sqlx::query("UPDATE transcript_speakers SET speaker_color = '#3b82f6' WHERE meeting_id = 'meeting-1' AND speaker_id = 'speaker-1'")
            .execute(&pool).await.unwrap();
        let metadata =
            DiarizationRepository::get_transcript_metadata(&pool, &["t-1".into(), "t-2".into()])
                .await
                .unwrap();
        assert!(metadata
            .iter()
            .filter(|row| row.speaker_id.as_deref() == Some("speaker-1"))
            .all(|row| row.speaker_color.as_deref() == Some("#be185d")));
        let other = DiarizationRepository::get_meeting_speakers(&pool, "meeting-2")
            .await
            .unwrap();
        assert!(other
            .iter()
            .all(|row| row.speaker_color.as_deref() != Some("#be185d")));
        assert_eq!(
            DiarizationRepository::set_speaker_color(&pool, "meeting-1", "absent", "#be185d")
                .await
                .unwrap(),
            0
        );
        let methods: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM transcript_speakers WHERE diarization_method = 'manual'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(methods, 0);
    }

    #[tokio::test]
    async fn rename_speaker_updates_transcripts_and_segments_for_one_meeting() {
        let pool = speaker_test_pool().await;

        let updated_count =
            DiarizationRepository::rename_speaker(&pool, "meeting-1", "speaker-1", "Alice")
                .await
                .expect("speaker should be renamed");

        assert_eq!(updated_count, 4);

        let meeting_one_labels: Vec<String> = sqlx::query_scalar(
            "SELECT speaker_label FROM transcript_speakers
             WHERE meeting_id = 'meeting-1' AND speaker_id = 'speaker-1'
             ORDER BY transcript_id",
        )
        .fetch_all(&pool)
        .await
        .expect("meeting one transcript labels should be fetched");

        assert_eq!(meeting_one_labels, vec!["Alice", "Alice"]);

        let meeting_two_label: String = sqlx::query_scalar(
            "SELECT speaker_label FROM transcript_speakers
             WHERE meeting_id = 'meeting-2' AND speaker_id = 'speaker-1'",
        )
        .fetch_one(&pool)
        .await
        .expect("meeting two transcript label should be fetched");

        assert_eq!(meeting_two_label, "Other meeting speaker");

        let segment_labels: Vec<String> = sqlx::query_scalar(
            "SELECT speaker_label FROM speaker_segments
             WHERE meeting_id = 'meeting-1' AND speaker_id = 'speaker-1'
             ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .expect("speaker segment labels should be fetched");

        assert_eq!(segment_labels, vec!["Alice", "Alice"]);
    }

    fn correction_window(id: &str, start: f64, end: f64) -> TranscriptCorrectionWindow {
        TranscriptCorrectionWindow {
            transcript_id: id.to_string(),
            audio_start_time: Some(start),
            audio_end_time: Some(end),
            speaker_source: Some("mixed".to_string()),
        }
    }

    #[tokio::test]
    async fn reassign_transcripts_adds_missing_metadata_and_preserves_other_rows() {
        let pool = speaker_test_pool().await;
        let untouched_before =
            DiarizationRepository::get_transcript_metadata(&pool, &["t-1".to_string()])
                .await
                .unwrap();

        let updated = DiarizationRepository::reassign_transcripts(
            &pool,
            "meeting-1",
            &[correction_window("missing-metadata", 10.0, 15.0)],
            Some("speaker-2"),
            Some("Bob"),
            false,
            &["speaker-1".to_string()],
        )
        .await
        .unwrap();

        assert_eq!(updated, 1);
        let rows = DiarizationRepository::get_transcript_metadata(
            &pool,
            &["missing-metadata".to_string()],
        )
        .await
        .unwrap();
        let row = &rows[0];
        assert_eq!(row.meeting_id, "meeting-1");
        assert_eq!(row.audio_start_time, Some(10.0));
        assert_eq!(row.audio_end_time, Some(15.0));
        assert_eq!(row.speaker_id.as_deref(), Some("speaker-2"));
        assert_eq!(row.speaker_label.as_deref(), Some("Bob"));
        assert_eq!(row.speaker_color.as_deref(), Some("#16a34a"));
        assert_eq!(row.speaker_source.as_deref(), Some("mixed"));
        assert_eq!(row.speaker_profile_id, None);
        assert_eq!(row.diarization_method.as_deref(), Some("manual"));
        assert_eq!(row.candidate_speaker_ids.as_deref(), Some("[]"));

        let untouched_after =
            DiarizationRepository::get_transcript_metadata(&pool, &["t-1".to_string()])
                .await
                .unwrap();
        assert_eq!(
            serde_json::to_value(untouched_before).unwrap(),
            serde_json::to_value(untouched_after).unwrap()
        );
    }

    #[tokio::test]
    async fn reassign_transcripts_clears_previous_profile_and_preserves_audio_source() {
        let pool = speaker_test_pool().await;
        sqlx::query(
            "UPDATE transcript_speakers SET speaker_profile_id = 'alice-profile',
                speaker_source = 'system' WHERE transcript_id = 't-1'",
        )
        .execute(&pool)
        .await
        .unwrap();
        let mut window = correction_window("t-1", 0.0, 1.0);
        window.speaker_source = None;

        DiarizationRepository::reassign_transcripts(
            &pool,
            "meeting-1",
            &[window],
            Some("speaker-2"),
            Some("Bob"),
            false,
            &[],
        )
        .await
        .unwrap();
        let rows = DiarizationRepository::get_transcript_metadata(&pool, &["t-1".to_string()])
            .await
            .unwrap();
        assert_eq!(rows[0].speaker_id.as_deref(), Some("speaker-2"));
        assert_eq!(rows[0].speaker_profile_id, None);
        assert_eq!(rows[0].speaker_source.as_deref(), Some("system"));
        assert_eq!(rows[0].speaker_color.as_deref(), Some("#16a34a"));
        let untouched_speaker: String = sqlx::query_scalar(
            "SELECT speaker_id FROM transcript_speakers WHERE transcript_id = 't-2'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(untouched_speaker, "speaker-1");
    }

    #[tokio::test]
    async fn overlap_correction_clears_single_speaker_fields() {
        let pool = speaker_test_pool().await;
        sqlx::query(
            "UPDATE transcript_speakers SET speaker_profile_id = 'alice-profile'
             WHERE transcript_id = 't-1'",
        )
        .execute(&pool)
        .await
        .unwrap();
        let candidates = vec!["speaker-1".to_string(), "speaker-2".to_string()];
        DiarizationRepository::reassign_transcripts(
            &pool,
            "meeting-1",
            &[correction_window("t-1", 0.0, 1.0)],
            Some("speaker-1"),
            Some("Alice"),
            true,
            &candidates,
        )
        .await
        .unwrap();

        let rows = DiarizationRepository::get_transcript_metadata(&pool, &["t-1".to_string()])
            .await
            .unwrap();
        let row = &rows[0];
        assert_eq!(row.speaker_id, None);
        assert_eq!(row.speaker_profile_id, None);
        assert_eq!(row.speaker_color, None);
        assert_eq!(row.speaker_label.as_deref(), Some("Multiple speakers"));
        assert_eq!(row.is_overlap, 1);
        assert_eq!(
            serde_json::from_str::<Vec<String>>(row.candidate_speaker_ids.as_ref().unwrap())
                .unwrap(),
            candidates
        );
    }

    #[tokio::test]
    async fn corrections_do_not_reassign_metadata_from_another_meeting() {
        let pool = speaker_test_pool().await;
        let updated = DiarizationRepository::reassign_transcripts(
            &pool,
            "meeting-1",
            &[correction_window("t-4", 0.0, 1.0)],
            Some("speaker-2"),
            Some("Bob"),
            false,
            &[],
        )
        .await
        .unwrap();
        assert_eq!(updated, 0);
        let rows = DiarizationRepository::get_transcript_metadata(&pool, &["t-4".to_string()])
            .await
            .unwrap();
        assert_eq!(rows[0].meeting_id, "meeting-2");
        assert_eq!(rows[0].speaker_id.as_deref(), Some("speaker-1"));
    }

    #[tokio::test]
    async fn merge_keeps_voice_history_and_does_not_count_audio_twice() {
        use super::super::speaker_profile::{SpeakerProfilesRepository, SampleSource, SampleRemoval};
        for target_contributed in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let store = crate::database::speaker_store::SpeakerFeatureStore::new(
                &directory.path().join("speakers.sqlite"),
            ).await.unwrap();
            let pool = store.pool();
            sqlx::query("INSERT INTO transcript_speakers (transcript_id, meeting_id, speaker_id, speaker_label) VALUES ('a', 'meeting', 'source', 'Alice'), ('b', 'meeting', 'target', 'Alice')")
                .execute(pool).await.unwrap();
            let id = SpeakerProfilesRepository::create_from_sample(pool, "Alice", &[1.0, 0.0], "v1", 1, 10.0,
                SampleSource { meeting_id: "meeting", speaker_id: "source" }).await.unwrap();
            if target_contributed {
                SpeakerProfilesRepository::add_sample(pool, &id, &[0.0, 1.0], "v1", 1, 10.0,
                    SampleSource { meeting_id: "meeting", speaker_id: "target" }).await.unwrap();
            }
            let before = SpeakerProfilesRepository::get(pool, &id).await.unwrap().unwrap();
            DiarizationRepository::merge_speakers(pool, "meeting", &["source".into()], "target", "Alice")
                .await.unwrap();
            let samples = SpeakerProfilesRepository::list_samples(pool, &id).await.unwrap();
            assert_eq!(samples.len(), 1);
            assert_eq!(samples[0].speaker_id, "target");
            assert_eq!(samples[0].speech_seconds, before.speech_seconds);
            assert!(!SpeakerProfilesRepository::add_sample(pool, &id, &[1.0, 0.0], "v1", 9, 90.0,
                SampleSource { meeting_id: "meeting", speaker_id: "target" }).await.unwrap());
            let after = SpeakerProfilesRepository::get(pool, &id).await.unwrap().unwrap();
            assert_eq!(after.embedding, before.embedding);
            assert_eq!(after.sample_count, before.sample_count);
            assert_eq!(after.speech_seconds, before.speech_seconds);
            assert_eq!(SpeakerProfilesRepository::remove_sample(pool, &id,
                SampleSource { meeting_id: "meeting", speaker_id: "target" }).await.unwrap(), SampleRemoval::Forgotten);
        }
    }

    #[tokio::test]
    async fn merge_clears_source_profile_and_marks_rename_as_manual() {
        let pool = speaker_test_pool().await;
        sqlx::query(
            "UPDATE transcript_speakers SET speaker_profile_id = 'alice-profile'
             WHERE meeting_id = 'meeting-1' AND speaker_id = 'speaker-1'",
        )
        .execute(&pool)
        .await
        .unwrap();
        DiarizationRepository::merge_speakers(
            &pool,
            "meeting-1",
            &["speaker-1".to_string()],
            "speaker-2",
            "Bob",
        )
        .await
        .unwrap();
        let rows = DiarizationRepository::get_transcript_metadata(&pool, &["t-1".to_string()])
            .await
            .unwrap();
        assert_eq!(rows[0].speaker_profile_id, None);
        assert_eq!(rows[0].speaker_color.as_deref(), Some("#16a34a"));
        assert_eq!(rows[0].diarization_method.as_deref(), Some("manual"));

        DiarizationRepository::rename_speaker(&pool, "meeting-1", "speaker-2", "Robert")
            .await
            .unwrap();
        let rows = DiarizationRepository::get_transcript_metadata(&pool, &["t-3".to_string()])
            .await
            .unwrap();
        assert_eq!(rows[0].speaker_label.as_deref(), Some("Robert"));
        assert_eq!(rows[0].diarization_method.as_deref(), Some("manual"));
    }
}
