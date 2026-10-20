-- Each meeting speaker that contributed audio to a remembered voice.
--
-- speaker_profiles.embedding stays the blended voiceprint used for recognition,
-- and a new sample is blended into it. Each sample also keeps its own
-- voiceprint, so removing one rebuilds the blend from the samples that remain.
-- The table also stops the same meeting speaker from being counted twice,
-- which speaker_profile_id alone can't do: automatic recognition links a
-- speaker to a profile without contributing audio.
CREATE TABLE IF NOT EXISTS speaker_profile_samples (
    profile_id TEXT NOT NULL,
    meeting_id TEXT NOT NULL,
    speaker_id TEXT NOT NULL,
    -- NULL when the contribution can't be separated from the blend.
    embedding BLOB,
    sample_count INTEGER NOT NULL DEFAULT 0,
    speech_seconds REAL NOT NULL,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (profile_id, meeting_id, speaker_id)
);

-- Voices remembered before this table existed. The old Remember action built
-- each profile from one meeting speaker, linked that speaker to it and marked
-- its rows manual, so the profile's voiceprint is that speaker's sample.
-- If several manual speakers point at one profile, the one with the most rows
-- gets the voiceprint; the others are only recorded so they aren't re-added.
INSERT OR IGNORE INTO speaker_profile_samples (
    profile_id, meeting_id, speaker_id, embedding, sample_count, speech_seconds
)
SELECT candidate.profile_id, candidate.meeting_id, candidate.speaker_id,
       CASE WHEN candidate.rank = 1 THEN p.embedding END,
       CASE WHEN candidate.rank = 1 THEN p.sample_count ELSE 0 END,
       CASE WHEN candidate.rank = 1 THEN p.speech_seconds ELSE 0 END
FROM (
    SELECT ts.speaker_profile_id AS profile_id, ts.meeting_id, ts.speaker_id,
           ROW_NUMBER() OVER (
               PARTITION BY ts.speaker_profile_id
               ORDER BY COUNT(*) DESC, ts.meeting_id, ts.speaker_id
           ) AS rank
    FROM transcript_speakers ts
    WHERE ts.speaker_profile_id IS NOT NULL
      AND ts.speaker_id IS NOT NULL
      AND ts.diarization_method = 'manual'
    GROUP BY ts.speaker_profile_id, ts.meeting_id, ts.speaker_id
) candidate
JOIN speaker_profiles p ON p.id = candidate.profile_id;
