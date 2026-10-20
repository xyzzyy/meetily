-- Commit this intent before changing core transcript rows. Recovery finishes
-- the speaker update if the core commit survived, or drops the intent if not.
CREATE TABLE enhancement_publications (
    meeting_id TEXT PRIMARY KEY,
    original_rows TEXT NOT NULL,
    proposed_rows TEXT NOT NULL,
    core_backup TEXT NOT NULL,
    speaker_labels TEXT
);
