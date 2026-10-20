-- Keep color choices separate from analysis output, which is replaced on rerun.
CREATE TABLE meeting_speaker_colors (
    meeting_id TEXT NOT NULL,
    speaker_id TEXT NOT NULL,
    color TEXT NOT NULL,
    PRIMARY KEY (meeting_id, speaker_id)
);
