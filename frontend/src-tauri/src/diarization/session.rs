// diarization/session.rs
//
// Per-recording diarization state: embedding extractor + online clusterer.
// Created when a recording starts (if the feature is enabled and the model
// is present) and dropped when it ends.

use super::clustering::{ClusterAssignment, SpeakerClusterer};
use super::embedding::{EmbeddingError, EmbeddingExtractor};
use std::collections::HashMap;
use std::path::Path;

/// Segments shorter than this carry forward the previous speaker label
/// instead of computing an unreliable embedding (1s at 16kHz).
const MIN_SAMPLES_FOR_EMBEDDING: usize = 16_000;

pub struct DiarizationSession {
    extractor: EmbeddingExtractor,
    clusterer: SpeakerClusterer,
    previous_by_source: HashMap<String, ClusterAssignment>,
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
            previous_by_source: HashMap::new(),
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
        self.label_segment_detailed("mixed", samples_16k)
            .map(|assignment| assignment.label)
    }

    pub fn label_segment_detailed(&mut self, source: &str, samples_16k: &[f32]) -> Option<ClusterAssignment> {
        label_for_source(&mut self.clusterer, &mut self.previous_by_source, source, samples_16k,
            |samples| self.extractor.compute(samples))
    }

    pub fn clusterer(&self) -> &SpeakerClusterer {
        &self.clusterer
    }

    pub fn clusterer_mut(&mut self) -> &mut SpeakerClusterer {
        &mut self.clusterer
    }
}

// Share voice clusters across sources, but never carry a remote speaker into a
// short microphone turn (or the reverse). The extractor is supplied separately
// so these decisions can be tested without loading a model or recording audio.
fn label_for_source(
    clusterer: &mut SpeakerClusterer,
    previous: &mut HashMap<String, ClusterAssignment>,
    source: &str,
    samples: &[f32],
    compute: impl FnOnce(&[f32]) -> Result<Vec<f32>, EmbeddingError>,
) -> Option<ClusterAssignment> {
    let fallback = |needs_review| previous.get(source).cloned().map(|mut assignment| {
        assignment.confidence = 0.0;
        assignment.needs_review |= needs_review;
        assignment
    });
    if samples.len() < MIN_SAMPLES_FOR_EMBEDDING {
        return fallback(false);
    }
    match compute(samples) {
        Ok(embedding) => {
            let assignment = clusterer.assign_detailed(&embedding);
            previous.insert(source.to_string(), assignment.clone());
            Some(assignment)
        }
        Err(error) => {
            log::warn!("Diarization embedding failed for {source}: {error}");
            fallback(true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_turns_keep_only_the_previous_speaker_from_the_same_source() {
        let mut clusterer = SpeakerClusterer::new();
        clusterer.seed_profile("alice", "Alice", vec![1.0, 0.0]);
        clusterer.seed_profile("bob", "Bob", vec![0.0, 1.0]);
        let mut previous = HashMap::new();
        let long = vec![0.0; MIN_SAMPLES_FOR_EMBEDDING];
        label_for_source(&mut clusterer, &mut previous, "system", &long, |_| Ok(vec![0.0, 1.0]));
        assert!(label_for_source(&mut clusterer, &mut previous, "microphone", &[], |_| panic!("short audio must not run the model")).is_none());
        label_for_source(&mut clusterer, &mut previous, "microphone", &long, |_| Ok(vec![1.0, 0.0]));
        for (source, profile) in [("system", "bob"), ("microphone", "alice")] {
            let assignment = label_for_source(&mut clusterer, &mut previous, source, &[], |_| unreachable!()).unwrap();
            assert_eq!(assignment.profile_id.as_deref(), Some(profile));
            assert_eq!(assignment.confidence, 0.0);
        }
    }

    #[test]
    fn embedding_errors_do_not_copy_another_sources_speaker() {
        let mut clusterer = SpeakerClusterer::new();
        clusterer.seed_profile("bob", "Bob", vec![0.0, 1.0]);
        let mut previous = HashMap::new();
        let long = vec![0.0; MIN_SAMPLES_FOR_EMBEDDING];
        label_for_source(&mut clusterer, &mut previous, "system", &long, |_| Ok(vec![0.0, 1.0]));
        assert!(label_for_source(&mut clusterer, &mut previous, "microphone", &long, |_| Err(EmbeddingError::NoOutput)).is_none());
        let assignment = label_for_source(&mut clusterer, &mut previous, "system", &long, |_| Err(EmbeddingError::NoOutput)).unwrap();
        assert_eq!(assignment.profile_id.as_deref(), Some("bob"));
        assert!(assignment.needs_review);
    }
}
