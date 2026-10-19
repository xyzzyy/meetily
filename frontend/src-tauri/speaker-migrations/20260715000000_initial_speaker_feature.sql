-- Branch-only speaker-recognition state. This migration is deliberately kept
-- outside Meetily's core migration directory and runs only against
-- speaker_recognition.sqlite.

CREATE TABLE IF NOT EXISTS diarization_settings (
    id TEXT PRIMARY KEY NOT NULL DEFAULT '1',
    enabled INTEGER NOT NULL DEFAULT 0,
    mode TEXT NOT NULL DEFAULT 'live_plus_post_call',
    show_provisional_labels INTEGER NOT NULL DEFAULT 1,
    post_call_refinement_enabled INTEGER NOT NULL DEFAULT 1,
    overlap_handling TEXT NOT NULL DEFAULT 'multiple_speakers',
    speaker_review_enabled INTEGER NOT NULL DEFAULT 1,
    updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);
INSERT INTO diarization_settings (id) VALUES ('1') ON CONFLICT(id) DO NOTHING;

CREATE TABLE IF NOT EXISTS meeting_diarization_status (
    meeting_id TEXT PRIMARY KEY NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 0,
    live_enabled INTEGER NOT NULL DEFAULT 0,
    post_call_enabled INTEGER NOT NULL DEFAULT 0,
    current_status TEXT NOT NULL DEFAULT 'none',
    quality_flags TEXT,
    processed_at DATETIME,
    updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    run_id TEXT,
    progress INTEGER NOT NULL DEFAULT 0,
    method TEXT,
    failure_reason TEXT,
    assigned_coverage REAL,
    transcription_success_rate REAL
);

CREATE TABLE IF NOT EXISTS speaker_segments (
    id TEXT PRIMARY KEY NOT NULL,
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
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    speaker_profile_id TEXT,
    candidate_speaker_ids TEXT
);
CREATE INDEX IF NOT EXISTS idx_speaker_segments_meeting_time
ON speaker_segments(meeting_id, start_time, end_time);

CREATE TABLE IF NOT EXISTS transcript_speakers (
    transcript_id TEXT PRIMARY KEY NOT NULL,
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
    run_id TEXT,
    published INTEGER NOT NULL DEFAULT 1,
    updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_transcript_speakers_meeting_time
ON transcript_speakers(meeting_id, audio_start_time, transcript_id);

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
CREATE INDEX IF NOT EXISTS idx_speaker_profiles_name ON speaker_profiles(name);

CREATE TABLE IF NOT EXISTS diarization_runs (
    id TEXT PRIMARY KEY NOT NULL,
    meeting_id TEXT NOT NULL,
    status TEXT NOT NULL,
    method TEXT NOT NULL,
    progress INTEGER NOT NULL DEFAULT 0,
    quality_flags TEXT,
    error TEXT,
    transcript_backup TEXT,
    proposed_transcripts TEXT,
    proposed_speaker_metadata TEXT,
    live_rows_hash TEXT,
    final_rows_hash TEXT,
    assigned_coverage REAL,
    transcription_success_rate REAL,
    started_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    completed_at DATETIME
);
CREATE INDEX IF NOT EXISTS idx_diarization_runs_meeting_started
ON diarization_runs(meeting_id, started_at DESC);

CREATE TABLE IF NOT EXISTS transcript_apply_rows (
    run_id TEXT NOT NULL,
    row_kind TEXT NOT NULL CHECK(row_kind IN ('live', 'final')),
    ordinal INTEGER NOT NULL,
    transcript_id TEXT NOT NULL,
    transcript_json TEXT NOT NULL,
    speaker_metadata_json TEXT,
    PRIMARY KEY(run_id, row_kind, ordinal)
);
