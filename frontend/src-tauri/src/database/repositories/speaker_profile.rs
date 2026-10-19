// database/repositories/speaker_profile.rs
//
// CRUD for persistent voice profiles (speaker identification).
// Embeddings are stored as f32 little-endian BLOBs.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::{Error as SqlxError, FromRow, SqlitePool};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct SpeakerProfileRow {
    pub id: String,
    pub name: String,
    pub embedding: Vec<u8>,
    pub model_version: String,
    pub sample_count: i64,
    pub speech_seconds: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerProfile {
    pub id: String,
    pub name: String,
    #[serde(skip)]
    pub embedding: Vec<f32>,
    pub model_version: String,
    pub sample_count: i64,
    pub speech_seconds: f64,
}

pub fn embedding_to_blob(embedding: &[f32]) -> Vec<u8> {
    embedding.iter().flat_map(|v| v.to_le_bytes()).collect()
}

pub fn blob_to_embedding(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

/// Blend a new voice sample into an existing voiceprint, weighting each by the
/// seconds of speech behind it, and re-normalize so cosine matching still works.
pub fn blend_embeddings(
    existing: &[f32],
    existing_seconds: f64,
    sample: &[f32],
    sample_seconds: f64,
) -> Vec<f32> {
    let existing_weight = existing_seconds.max(0.0) as f32;
    let sample_weight = sample_seconds.max(0.0) as f32;
    let mut blended: Vec<f32> = existing
        .iter()
        .zip(sample)
        .map(|(a, b)| a * existing_weight + b * sample_weight)
        .collect();
    let norm = blended.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        blended.iter_mut().for_each(|v| *v /= norm);
    }
    blended
}

/// Where a voice sample came from, so the same meeting speaker is never added
/// to a profile twice.
pub struct SampleSource<'a> {
    pub meeting_id: &'a str,
    pub speaker_id: &'a str,
}

pub struct SpeakerProfilesRepository;

impl SpeakerProfilesRepository {
    pub async fn list(pool: &SqlitePool) -> Result<Vec<SpeakerProfile>, SqlxError> {
        let rows = sqlx::query_as::<_, SpeakerProfileRow>(
            "SELECT id, name, embedding, model_version, sample_count, speech_seconds
             FROM speaker_profiles ORDER BY name",
        )
        .fetch_all(pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| SpeakerProfile {
                id: r.id,
                name: r.name,
                embedding: blob_to_embedding(&r.embedding),
                model_version: r.model_version,
                sample_count: r.sample_count,
                speech_seconds: r.speech_seconds,
            })
            .collect())
    }

    /// Create a profile from one meeting speaker's voice and record that sample.
    pub async fn create_from_sample(
        pool: &SqlitePool,
        name: &str,
        embedding: &[f32],
        model_version: &str,
        sample_count: i64,
        speech_seconds: f64,
        source: SampleSource<'_>,
    ) -> Result<String, SqlxError> {
        let id = format!("speaker-{}", Uuid::new_v4());
        let now = Utc::now();
        let mut transaction = pool.begin().await?;
        sqlx::query(
            "INSERT INTO speaker_profiles (
                id, name, embedding, model_version, sample_count,
                speech_seconds, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(name)
        .bind(embedding_to_blob(embedding))
        .bind(model_version)
        .bind(sample_count)
        .bind(speech_seconds)
        .bind(now)
        .bind(now)
        .execute(&mut *transaction)
        .await?;
        Self::record_sample(&mut transaction, &id, &source, speech_seconds).await?;
        transaction.commit().await?;
        Ok(id)
    }

    /// Add one meeting speaker's voice to an existing profile.
    ///
    /// Returns `false` without changing anything if that speaker already
    /// contributed to the profile.
    pub async fn add_sample(
        pool: &SqlitePool,
        profile_id: &str,
        embedding: &[f32],
        model_version: &str,
        sample_count: i64,
        speech_seconds: f64,
        source: SampleSource<'_>,
    ) -> Result<bool, SqlxError> {
        let mut transaction = pool.begin().await?;
        let already_added: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM speaker_profile_samples
             WHERE profile_id = ? AND meeting_id = ? AND speaker_id = ?",
        )
        .bind(profile_id)
        .bind(source.meeting_id)
        .bind(source.speaker_id)
        .fetch_optional(&mut *transaction)
        .await?;
        if already_added.is_some() {
            return Ok(false);
        }
        let row = sqlx::query_as::<_, SpeakerProfileRow>(
            "SELECT id, name, embedding, model_version, sample_count, speech_seconds
             FROM speaker_profiles WHERE id = ?",
        )
        .bind(profile_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(SqlxError::RowNotFound)?;
        if row.model_version != model_version {
            return Err(SqlxError::Protocol(format!(
                "Remembered voice uses speaker model {}, but this sample uses {}",
                row.model_version, model_version
            )));
        }
        let blended = blend_embeddings(
            &blob_to_embedding(&row.embedding),
            row.speech_seconds,
            embedding,
            speech_seconds,
        );
        sqlx::query(
            "UPDATE speaker_profiles
             SET embedding = ?, sample_count = sample_count + ?,
                 speech_seconds = speech_seconds + ?, updated_at = ?
             WHERE id = ?",
        )
        .bind(embedding_to_blob(&blended))
        .bind(sample_count)
        .bind(speech_seconds)
        .bind(Utc::now())
        .bind(profile_id)
        .execute(&mut *transaction)
        .await?;
        Self::record_sample(&mut transaction, profile_id, &source, speech_seconds).await?;
        transaction.commit().await?;
        Ok(true)
    }

    async fn record_sample(
        transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        profile_id: &str,
        source: &SampleSource<'_>,
        speech_seconds: f64,
    ) -> Result<(), SqlxError> {
        sqlx::query(
            "INSERT INTO speaker_profile_samples (profile_id, meeting_id, speaker_id, speech_seconds)
             VALUES (?, ?, ?, ?)",
        )
        .bind(profile_id)
        .bind(source.meeting_id)
        .bind(source.speaker_id)
        .bind(speech_seconds)
        .execute(&mut **transaction)
        .await?;
        Ok(())
    }

    /// Find a remembered voice by name, ignoring case and surrounding spaces.
    /// If several share the name, the one with the most speech wins.
    pub async fn find_by_name(
        pool: &SqlitePool,
        name: &str,
    ) -> Result<Option<SpeakerProfile>, SqlxError> {
        let wanted = name.trim().to_lowercase();
        Ok(Self::list(pool)
            .await?
            .into_iter()
            .filter(|profile| profile.name.trim().to_lowercase() == wanted)
            .max_by(|a, b| a.speech_seconds.total_cmp(&b.speech_seconds)))
    }

    /// Merge remembered voices that share a name (ignoring case and spacing)
    /// and speaker model into one, blending their voiceprints by speech.
    ///
    /// Earlier builds created a new profile every time a voice was remembered,
    /// so the same person could appear several times. Transcript rows and
    /// sample records move to the surviving profile. Returns how many
    /// duplicate profiles were removed.
    pub async fn merge_duplicate_names(pool: &SqlitePool) -> Result<usize, SqlxError> {
        let mut groups = std::collections::BTreeMap::<(String, String), Vec<SpeakerProfile>>::new();
        for profile in Self::list(pool).await? {
            groups
                .entry((profile.name.trim().to_lowercase(), profile.model_version.clone()))
                .or_default()
                .push(profile);
        }
        let mut removed = 0;
        for (_, mut group) in groups.into_iter().filter(|(_, group)| group.len() > 1) {
            // Keep the profile with the most speech; ties keep the first listed.
            group.sort_by(|a, b| b.speech_seconds.total_cmp(&a.speech_seconds));
            let keep = group.remove(0);
            let mut embedding = keep.embedding.clone();
            let mut seconds = keep.speech_seconds;
            let mut samples = keep.sample_count;
            let mut transaction = pool.begin().await?;
            for duplicate in &group {
                embedding = blend_embeddings(&embedding, seconds, &duplicate.embedding, duplicate.speech_seconds);
                seconds += duplicate.speech_seconds;
                samples += duplicate.sample_count;
                for statement in [
                    "UPDATE OR IGNORE speaker_profile_samples SET profile_id = ? WHERE profile_id = ?",
                    "DELETE FROM speaker_profile_samples WHERE profile_id = ?2 AND ?1 IS NOT NULL",
                    "UPDATE transcript_speakers SET speaker_profile_id = ? WHERE speaker_profile_id = ?",
                    "UPDATE speaker_segments SET speaker_profile_id = ? WHERE speaker_profile_id = ?",
                ] {
                    sqlx::query(statement)
                        .bind(&keep.id)
                        .bind(&duplicate.id)
                        .execute(&mut *transaction)
                        .await?;
                }
                sqlx::query("DELETE FROM speaker_profiles WHERE id = ?")
                    .bind(&duplicate.id)
                    .execute(&mut *transaction)
                    .await?;
                removed += 1;
            }
            sqlx::query(
                "UPDATE speaker_profiles
                 SET embedding = ?, sample_count = ?, speech_seconds = ?, updated_at = ?
                 WHERE id = ?",
            )
            .bind(embedding_to_blob(&embedding))
            .bind(samples)
            .bind(seconds)
            .bind(Utc::now())
            .bind(&keep.id)
            .execute(&mut *transaction)
            .await?;
            transaction.commit().await?;
        }
        Ok(removed)
    }

    pub async fn rename(pool: &SqlitePool, id: &str, name: &str) -> Result<(), SqlxError> {
        sqlx::query("UPDATE speaker_profiles SET name = ?, updated_at = ? WHERE id = ?")
            .bind(name)
            .bind(Utc::now())
            .bind(id)
            .execute(pool)
            .await?;
        Ok(())
    }

    pub async fn delete(pool: &SqlitePool, id: &str) -> Result<(), SqlxError> {
        let mut transaction = pool.begin().await?;
        sqlx::query("DELETE FROM speaker_profile_samples WHERE profile_id = ?")
            .bind(id)
            .execute(&mut *transaction)
            .await?;
        sqlx::query("DELETE FROM speaker_profiles WHERE id = ?")
            .bind(id)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn get(pool: &SqlitePool, id: &str) -> Result<Option<SpeakerProfile>, SqlxError> {
        let row = sqlx::query_as::<_, SpeakerProfileRow>(
            "SELECT id, name, embedding, model_version, sample_count, speech_seconds
             FROM speaker_profiles WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(pool)
        .await?;

        Ok(row.map(|row| SpeakerProfile {
            id: row.id,
            name: row.name,
            embedding: blob_to_embedding(&row.embedding),
            model_version: row.model_version,
            sample_count: row.sample_count,
            speech_seconds: row.speech_seconds,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_roundtrip() {
        let embedding = vec![0.5f32, -1.25, 3.75, 0.0];
        let blob = embedding_to_blob(&embedding);
        assert_eq!(blob.len(), 16);
        assert_eq!(blob_to_embedding(&blob), embedding);
    }

    #[test]
    fn blending_weights_by_speech_and_stays_normalized() {
        let blended = blend_embeddings(&[1.0, 0.0], 30.0, &[0.0, 1.0], 10.0);
        let norm = blended.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
        // Three times as much speech behind the existing voice keeps it dominant.
        assert!((blended[0] / blended[1] - 3.0).abs() < 1e-5);
    }

    async fn speaker_store() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./speaker-migrations").run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn adding_a_sample_grows_the_profile_once_per_meeting_speaker() {
        let pool = speaker_store().await;
        let source = || SampleSource { meeting_id: "meeting-1", speaker_id: "speaker-a" };
        let id = SpeakerProfilesRepository::create_from_sample(
            &pool, "Alice", &[1.0, 0.0], "v1", 2, 20.0, source(),
        )
        .await
        .unwrap();

        // The same meeting speaker again adds nothing.
        assert!(!SpeakerProfilesRepository::add_sample(&pool, &id, &[0.0, 1.0], "v1", 3, 20.0, source())
            .await
            .unwrap());
        let unchanged = SpeakerProfilesRepository::get(&pool, &id).await.unwrap().unwrap();
        assert_eq!((unchanged.sample_count, unchanged.speech_seconds), (2, 20.0));

        // A speaker from another meeting is blended in.
        let other = SampleSource { meeting_id: "meeting-2", speaker_id: "speaker-b" };
        assert!(SpeakerProfilesRepository::add_sample(&pool, &id, &[0.0, 1.0], "v1", 3, 20.0, other)
            .await
            .unwrap());
        let grown = SpeakerProfilesRepository::get(&pool, &id).await.unwrap().unwrap();
        assert_eq!((grown.sample_count, grown.speech_seconds), (5, 40.0));
        assert!((grown.embedding[0] - grown.embedding[1]).abs() < 1e-6);
        assert_eq!(SpeakerProfilesRepository::list(&pool).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn samples_from_another_speaker_model_are_rejected() {
        let pool = speaker_store().await;
        let id = SpeakerProfilesRepository::create_from_sample(
            &pool, "Alice", &[1.0, 0.0], "v1", 1, 10.0,
            SampleSource { meeting_id: "m1", speaker_id: "s1" },
        )
        .await
        .unwrap();
        assert!(SpeakerProfilesRepository::add_sample(
            &pool, &id, &[0.0, 1.0], "v2", 1, 10.0,
            SampleSource { meeting_id: "m2", speaker_id: "s2" },
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn names_match_ignoring_case_and_spacing() {
        let pool = speaker_store().await;
        SpeakerProfilesRepository::create_from_sample(
            &pool, "Scrum Master Madior", &[1.0, 0.0], "v1", 1, 10.0,
            SampleSource { meeting_id: "m1", speaker_id: "s1" },
        )
        .await
        .unwrap();
        let found = SpeakerProfilesRepository::find_by_name(&pool, "  scrum master MADIOR ")
            .await
            .unwrap();
        assert_eq!(found.map(|profile| profile.name).as_deref(), Some("Scrum Master Madior"));
        assert!(SpeakerProfilesRepository::find_by_name(&pool, "Someone Else").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn duplicate_names_merge_into_one_voice_with_all_their_data() {
        let pool = speaker_store().await;
        let first = SpeakerProfilesRepository::create_from_sample(
            &pool, "Scrum Master Madior", &[1.0, 0.0], "v1", 7, 30.0,
            SampleSource { meeting_id: "m1", speaker_id: "S1" },
        )
        .await
        .unwrap();
        let second = SpeakerProfilesRepository::create_from_sample(
            &pool, "scrum master madior ", &[0.0, 1.0], "v1", 20, 30.0,
            SampleSource { meeting_id: "m2", speaker_id: "S2" },
        )
        .await
        .unwrap();
        let other = SpeakerProfilesRepository::create_from_sample(
            &pool, "Alice", &[1.0, 0.0], "v1", 1, 10.0,
            SampleSource { meeting_id: "m1", speaker_id: "S3" },
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO transcript_speakers (transcript_id, meeting_id, speaker_id, speaker_profile_id)
             VALUES ('t1', 'm2', 'S2', ?)",
        )
        .bind(&second)
        .execute(&pool)
        .await
        .unwrap();

        assert_eq!(SpeakerProfilesRepository::merge_duplicate_names(&pool).await.unwrap(), 1);
        let profiles = SpeakerProfilesRepository::list(&pool).await.unwrap();
        assert_eq!(profiles.len(), 2);
        let merged = profiles.iter().find(|profile| profile.id == first || profile.id == second).unwrap();
        assert_eq!((merged.sample_count, merged.speech_seconds), (27, 60.0));
        assert!((merged.embedding[0] - merged.embedding[1]).abs() < 1e-6);
        assert!(profiles.iter().any(|profile| profile.id == other));

        let linked: String = sqlx::query_scalar("SELECT speaker_profile_id FROM transcript_speakers WHERE transcript_id = 't1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(linked, merged.id);
        let sources: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM speaker_profile_samples WHERE profile_id = ?")
            .bind(&merged.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(sources, 2);

        // Running it again changes nothing.
        assert_eq!(SpeakerProfilesRepository::merge_duplicate_names(&pool).await.unwrap(), 0);
    }
}
