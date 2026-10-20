-- Repair stores converted after the original sample migration had run.
-- Only profiles without sample history qualify. Existing contributions and
-- removed samples must never be reconstructed from later manual corrections.
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
      AND NOT EXISTS (
          SELECT 1 FROM speaker_profile_samples existing
          WHERE existing.profile_id = ts.speaker_profile_id
      )
    GROUP BY ts.speaker_profile_id, ts.meeting_id, ts.speaker_id
) candidate
JOIN speaker_profiles p ON p.id = candidate.profile_id;
