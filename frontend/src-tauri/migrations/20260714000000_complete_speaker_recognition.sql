-- Complete local speaker recognition storage. The legacy transcripts.speaker
-- column remains source metadata and is intentionally not reused.

ALTER TABLE transcripts ADD COLUMN speaker_profile_id TEXT;
ALTER TABLE transcripts ADD COLUMN speaker_source TEXT;
ALTER TABLE transcripts ADD COLUMN candidate_speaker_ids TEXT;
ALTER TABLE speaker_segments ADD COLUMN speaker_profile_id TEXT;
ALTER TABLE speaker_segments ADD COLUMN candidate_speaker_ids TEXT;
ALTER TABLE meeting_diarization_status ADD COLUMN run_id TEXT;
ALTER TABLE meeting_diarization_status ADD COLUMN progress INTEGER NOT NULL DEFAULT 0;
ALTER TABLE meeting_diarization_status ADD COLUMN method TEXT;
ALTER TABLE meeting_diarization_status ADD COLUMN failure_reason TEXT;
ALTER TABLE meeting_diarization_status ADD COLUMN assigned_coverage REAL;
ALTER TABLE meeting_diarization_status ADD COLUMN transcription_success_rate REAL;

CREATE TABLE IF NOT EXISTS speaker_profiles (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    embedding BLOB NOT NULL,
    model_version TEXT NOT NULL,
    sample_count INTEGER NOT NULL DEFAULT 0,
    speech_seconds REAL NOT NULL DEFAULT 0,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    last_used_at DATETIME
);

CREATE TABLE IF NOT EXISTS diarization_runs (
    id TEXT PRIMARY KEY NOT NULL,
    meeting_id TEXT NOT NULL,
    status TEXT NOT NULL,
    method TEXT NOT NULL,
    progress INTEGER NOT NULL DEFAULT 0,
    quality_flags TEXT,
    error TEXT,
    transcript_backup TEXT,
    assigned_coverage REAL,
    transcription_success_rate REAL,
    started_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    completed_at DATETIME,
    FOREIGN KEY(meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_diarization_runs_meeting_started
ON diarization_runs(meeting_id, started_at DESC);

CREATE INDEX IF NOT EXISTS idx_speaker_profiles_name
ON speaker_profiles(name);
