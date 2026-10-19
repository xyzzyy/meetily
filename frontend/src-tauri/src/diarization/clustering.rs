// diarization/clustering.rs
//
// Online speaker clustering over L2-normalized embeddings.
// Each incoming embedding joins the nearest cluster centroid if cosine
// similarity exceeds the threshold, otherwise starts a new "Speaker N"
// cluster. Centroids are running means, re-normalized after update.

/// Minimum cosine similarity for an embedding to join an existing cluster.
/// Tuned for WeSpeaker CAM++ embeddings; raise to split more aggressively.
pub const CLUSTER_SIMILARITY_THRESHOLD: f32 = 0.55;

/// Minimum cosine similarity for a new cluster to match a saved voice profile.
/// Slightly stricter than intra-meeting clustering to avoid false renames.
pub const PROFILE_MATCH_THRESHOLD: f32 = 0.65;
pub const PROFILE_MATCH_MARGIN: f32 = 0.08;
pub const MAX_TENTATIVE_SPEAKERS: usize = 20;

#[derive(Debug, Clone, PartialEq)]
pub struct ClusterAssignment {
    pub label: String,
    pub profile_id: Option<String>,
    pub confidence: f32,
    pub needs_review: bool,
}

pub struct SpeakerCluster {
    pub centroid: Vec<f32>,
    pub count: usize,
    pub label: String,
    /// True for clusters seeded from saved voice profiles (not counted
    /// toward "Speaker N" numbering and excluded from centroid snapshots
    /// until they have real segments).
    pub from_profile: bool,
    pub profile_id: Option<String>,
}

pub struct SpeakerClusterer {
    clusters: Vec<SpeakerCluster>,
    last_label: Option<String>,
    anon_speaker_count: usize,
}

pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    // Inputs are L2-normalized, so the dot product is the cosine similarity
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

impl SpeakerClusterer {
    pub fn new() -> Self {
        Self {
            clusters: Vec::new(),
            last_label: None,
            anon_speaker_count: 0,
        }
    }

    /// Seed the clusterer with a saved voice profile so returning speakers
    /// are recognized by name instead of getting an anonymous label.
    pub fn seed_profile(&mut self, id: &str, name: &str, centroid: Vec<f32>) {
        self.clusters.push(SpeakerCluster {
            centroid,
            count: 0,
            label: name.to_string(),
            from_profile: true,
            profile_id: Some(id.to_string()),
        });
    }

    /// Label of the most recently assigned segment (used to carry labels
    /// across segments too short for reliable embeddings).
    pub fn last_label(&self) -> Option<String> {
        self.last_label.clone()
    }

    /// The remembered voice behind the last label, if it matched one.
    pub fn last_profile_id(&self) -> Option<String> {
        let label = self.last_label.as_deref()?;
        self.clusters
            .iter()
            .find(|cluster| cluster.label == label)
            .and_then(|cluster| cluster.profile_id.clone())
    }

    /// Assign an L2-normalized embedding to a cluster, returning its label.
    /// Unmatched profile-seeded clusters require the stricter profile
    /// threshold for their first match.
    pub fn assign(&mut self, embedding: &[f32]) -> String {
        self.assign_detailed(embedding).label
    }

    pub fn assign_detailed(&mut self, embedding: &[f32]) -> ClusterAssignment {
        let mut profile_matches: Vec<(usize, f32)> = self
            .clusters
            .iter()
            .enumerate()
            .filter(|(_, cluster)| cluster.from_profile && cluster.count == 0)
            .map(|(index, cluster)| (index, cosine_similarity(embedding, &cluster.centroid)))
            .filter(|(_, similarity)| *similarity >= PROFILE_MATCH_THRESHOLD)
            .collect();
        profile_matches.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        if let Some(&(index, similarity)) = profile_matches.first() {
            let runner_up = profile_matches
                .get(1)
                .map(|match_| match_.1)
                .unwrap_or(-1.0);
            if similarity - runner_up >= PROFILE_MATCH_MARGIN {
                let label = self.update_cluster(index, embedding);
                self.last_label = Some(label.clone());
                return ClusterAssignment {
                    label,
                    profile_id: self.clusters[index].profile_id.clone(),
                    confidence: similarity,
                    needs_review: false,
                };
            }
        }

        let best = self
            .clusters
            .iter()
            .enumerate()
            .map(|(i, c)| (i, cosine_similarity(embedding, &c.centroid)))
            .filter(|(i, similarity)| {
                let cluster = &self.clusters[*i];
                (!cluster.from_profile || cluster.count > 0)
                    && *similarity >= CLUSTER_SIMILARITY_THRESHOLD
            })
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        let (label, profile_id, confidence, needs_review) = match best {
            Some((idx, similarity)) => (
                self.update_cluster(idx, embedding),
                self.clusters[idx].profile_id.clone(),
                similarity,
                false,
            ),
            None => {
                if self.anon_speaker_count >= MAX_TENTATIVE_SPEAKERS {
                    ("Unknown speaker".to_string(), None, 0.0, true)
                } else {
                    self.anon_speaker_count += 1;
                    let label = format!("Speaker {}", self.anon_speaker_count);
                    self.clusters.push(SpeakerCluster {
                        centroid: embedding.to_vec(),
                        count: 1,
                        label: label.clone(),
                        from_profile: false,
                        profile_id: None,
                    });
                    (label, None, 1.0, false)
                }
            }
        };

        self.last_label = Some(label.clone());
        ClusterAssignment {
            label,
            profile_id,
            confidence,
            needs_review,
        }
    }

    fn update_cluster(&mut self, idx: usize, embedding: &[f32]) -> String {
        let cluster = &mut self.clusters[idx];
        let n = cluster.count as f32;
        for (centroid, value) in cluster.centroid.iter_mut().zip(embedding.iter()) {
            *centroid = (*centroid * n + value) / (n + 1.0);
        }
        let norm = cluster
            .centroid
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt();
        if norm > 0.0 {
            for value in &mut cluster.centroid {
                *value /= norm;
            }
        }
        cluster.count += 1;
        cluster.label.clone()
    }

    /// Snapshot of (label, centroid, segment count) for profile persistence.
    /// Profile-seeded clusters that never matched audio are excluded.
    pub fn centroids(&self) -> impl Iterator<Item = (&str, &[f32], usize)> {
        self.clusters
            .iter()
            .filter(|c| c.count > 0)
            .map(|c| (c.label.as_str(), c.centroid.as_slice(), c.count))
    }

    /// Replace a cluster's label (e.g. when matched to a saved voice profile).
    pub fn relabel(&mut self, old_label: &str, new_label: &str) {
        for cluster in &mut self.clusters {
            if cluster.label == old_label {
                cluster.label = new_label.to_string();
            }
        }
        if self.last_label.as_deref() == Some(old_label) {
            self.last_label = Some(new_label.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: Vec<f32>) -> Vec<f32> {
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.into_iter().map(|x| x / n).collect()
    }

    #[test]
    fn same_voice_same_cluster() {
        let mut c = SpeakerClusterer::new();
        let a = unit(vec![1.0, 0.1, 0.0]);
        let a2 = unit(vec![0.95, 0.15, 0.05]);
        assert_eq!(c.assign(&a), "Speaker 1");
        assert_eq!(c.assign(&a2), "Speaker 1");
    }

    #[test]
    fn different_voice_new_cluster() {
        let mut c = SpeakerClusterer::new();
        let a = unit(vec![1.0, 0.0, 0.0]);
        let b = unit(vec![0.0, 1.0, 0.0]);
        assert_eq!(c.assign(&a), "Speaker 1");
        assert_eq!(c.assign(&b), "Speaker 2");
        assert_eq!(c.last_label().as_deref(), Some("Speaker 2"));
    }

    #[test]
    fn last_profile_id_follows_the_last_matched_voice() {
        let mut c = SpeakerClusterer::new();
        c.seed_profile("profile-alice", "Alice", unit(vec![1.0, 0.0, 0.0]));
        assert_eq!(c.last_profile_id(), None);
        let alice = c.assign_detailed(&unit(vec![1.0, 0.02, 0.0]));
        assert_eq!(alice.profile_id.as_deref(), Some("profile-alice"));
        assert_eq!(c.last_profile_id().as_deref(), Some("profile-alice"));
        c.assign(&unit(vec![0.0, 1.0, 0.0]));
        assert_eq!(c.last_profile_id(), None, "an anonymous speaker has no remembered voice");
    }

    #[test]
    fn ambiguous_saved_profiles_remain_anonymous() {
        let mut clusterer = SpeakerClusterer::new();
        clusterer.seed_profile("alice", "Alice", unit(vec![1.0, 0.0]));
        clusterer.seed_profile("alicia", "Alicia", unit(vec![0.99, 0.01]));
        let assignment = clusterer.assign_detailed(&unit(vec![1.0, 0.0]));
        assert_eq!(assignment.label, "Speaker 1");
        assert_eq!(assignment.profile_id, None);
    }

    #[test]
    fn pathological_audio_stops_after_twenty_tentative_clusters() {
        let mut clusterer = SpeakerClusterer::new();
        for index in 0..MAX_TENTATIVE_SPEAKERS {
            let mut embedding = vec![0.0; MAX_TENTATIVE_SPEAKERS + 1];
            embedding[index] = 1.0;
            assert!(!clusterer.assign_detailed(&embedding).needs_review);
        }
        let mut extra = vec![0.0; MAX_TENTATIVE_SPEAKERS + 1];
        extra[MAX_TENTATIVE_SPEAKERS] = 1.0;
        let assignment = clusterer.assign_detailed(&extra);
        assert_eq!(assignment.label, "Unknown speaker");
        assert!(assignment.needs_review);
    }
}
