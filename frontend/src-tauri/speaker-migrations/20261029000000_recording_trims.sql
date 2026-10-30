-- A trim spans the audio folder and two SQLite databases. Finish this intent
-- before speaker recovery, so an old transcript backup cannot restore removed text.
CREATE TABLE recording_trims (
    meeting_id TEXT PRIMARY KEY NOT NULL,
    folder_path TEXT NOT NULL,
    original_filename TEXT NOT NULL,
    audio_hash TEXT NOT NULL,
    duration_seconds REAL NOT NULL
);
