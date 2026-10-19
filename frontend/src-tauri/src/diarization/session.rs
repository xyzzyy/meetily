// diarization/session.rs
//
// Per-recording diarization state: embedding extractor + online clusterer.
// Created when a recording starts (if the feature is enabled and the model
// is present) and dropped when it ends.

use super::clustering::{ClusterAssignment, SpeakerClusterer};
use super::embedding::{EmbeddingError, EmbeddingExtractor};
use std::path::Path;

/// Segments shorter than this carry forward the previous speaker label
/// instead of computing an unreliable embedding (1s at 16kHz).
const MIN_SAMPLES_FOR_EMBEDDING: usize = 16_000;

pub struct DiarizationSession {
    extractor: EmbeddingExtractor,
    clusterer: SpeakerClusterer,
}

impl DiarizationSession {
    pub fn new(embedding_model_path: &Path) -> Result<Self, EmbeddingError> {
        Self::with_profiles(embedding_model_path, Vec::new())
    }

    /// Create a session pre-seeded with saved voice profiles (name, centroid)
    /// so returning speakers are labeled by name instead of "Speaker N".
    pub fn with_profiles(
        embedding_model_path: &Path,
        profiles: Vec<(String, String, Vec<f32>)>,
    ) -> Result<Self, EmbeddingError> {
        let mut clusterer = SpeakerClusterer::new();
        for (id, name, centroid) in profiles {
            clusterer.seed_profile(&id, &name, centroid);
        }
        Ok(Self {
            extractor: EmbeddingExtractor::new(embedding_model_path)?,
            clusterer,
        })
    }

    /// (label, centroid, segment count) snapshot for persisting this
    /// recording's speakers (written to speakers.json at recording end).
    pub fn centroid_snapshot(&self) -> Vec<(String, Vec<f32>, usize)> {
        self.clusterer
            .centroids()
            .map(|(label, centroid, count)| (label.to_string(), centroid.to_vec(), count))
            .collect()
    }

    /// Assign a speaker label to a 16kHz mono speech segment.
    /// Returns None only when no label can be produced (e.g. first segment
    /// is too short). Diarization failures must never break transcription —
    /// errors are logged and degrade to the previous label or None.
    pub fn label_segment(&mut self, samples_16k: &[f32]) -> Option<String> {
        self.label_segment_detailed(samples_16k)
            .map(|assignment| assignment.label)
    }

    pub fn label_segment_detailed(&mut self, samples_16k: &[f32]) -> Option<ClusterAssignment> {
        if samples_16k.len() < MIN_SAMPLES_FOR_EMBEDDING {
            // Too short to fingerprint: carry the previous speaker, including
            // the remembered voice it matched, so one person stays one speaker.
            let profile_id = self.clusterer.last_profile_id();
            return self.clusterer.last_label().map(|label| ClusterAssignment {
                label,
                profile_id,
                confidence: 0.0,
                needs_review: false,
            });
        }
        match self.extractor.compute(samples_16k) {
            Ok(embedding) => Some(self.clusterer.assign_detailed(&embedding)),
            Err(e) => {
                log::warn!(
                    "Diarization embedding failed, carrying previous label: {}",
                    e
                );
                let profile_id = self.clusterer.last_profile_id();
                self.clusterer.last_label().map(|label| ClusterAssignment {
                    label,
                    profile_id,
                    confidence: 0.0,
                    needs_review: true,
                })
            }
        }
    }

    pub fn clusterer(&self) -> &SpeakerClusterer {
        &self.clusterer
    }

    pub fn clusterer_mut(&mut self) -> &mut SpeakerClusterer {
        &mut self.clusterer
    }
}
