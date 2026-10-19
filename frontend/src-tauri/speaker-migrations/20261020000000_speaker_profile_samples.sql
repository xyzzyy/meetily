-- Which meeting speakers have contributed audio to each remembered voice.
-- Attributing the same meeting speaker to a profile again must not count the
-- same speech twice, and automatic recognition links a speaker to a profile
-- without contributing audio, so speaker_profile_id alone can't tell them apart.
CREATE TABLE IF NOT EXISTS speaker_profile_samples (
    profile_id TEXT NOT NULL,
    meeting_id TEXT NOT NULL,
    speaker_id TEXT NOT NULL,
    speech_seconds REAL NOT NULL,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (profile_id, meeting_id, speaker_id)
);

-- Voices remembered before this table existed: the old Remember action linked
-- the meeting speaker to its new profile and marked the rows manual. Record
-- those as contributions so attributing the same speaker again can't count
-- the same speech twice. Their seconds are already in speaker_profiles.
INSERT OR IGNORE INTO speaker_profile_samples (profile_id, meeting_id, speaker_id, speech_seconds)
SELECT DISTINCT ts.speaker_profile_id, ts.meeting_id, ts.speaker_id, 0
FROM transcript_speakers ts
JOIN speaker_profiles p ON p.id = ts.speaker_profile_id
WHERE ts.speaker_id IS NOT NULL AND ts.diarization_method = 'manual';
