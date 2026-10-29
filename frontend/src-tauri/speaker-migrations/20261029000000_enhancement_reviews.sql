-- A completed model run is a proposal, not proof that its words improved.
-- Applied proposals retain their original rows for recovery and inspection.
CREATE TABLE enhancement_reviews (
    id TEXT PRIMARY KEY NOT NULL,
    meeting_id TEXT NOT NULL,
    payload TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'applied', 'undone')),
    applied_speakers TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE UNIQUE INDEX enhancement_reviews_pending ON enhancement_reviews(meeting_id) WHERE state = 'pending';
