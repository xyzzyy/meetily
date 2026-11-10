use super::clustering::{PROFILE_MATCH_MARGIN, PROFILE_MATCH_THRESHOLD};

/// One remembered identity. Its meeting samples compete as one profile, so
/// two close samples of the same person do not count as two different people.
#[derive(Debug, Clone)]
pub struct VoiceProfile {
    pub id: String,
    pub name: String,
    pub embedding: Vec<f32>,
    pub samples: Vec<Vec<f32>>,
}

fn similarity(left: &[f32], right: &[f32]) -> Option<f32> {
    if left.is_empty()
        || left.len() != right.len()
        || !left.iter().chain(right).all(|value| value.is_finite())
    {
        return None;
    }
    let dot: f64 = left
        .iter()
        .zip(right)
        .map(|(a, b)| f64::from(*a) * f64::from(*b))
        .sum();
    let norm = |values: &[f32]| {
        values
            .iter()
            .map(|value| f64::from(*value).powi(2))
            .sum::<f64>()
            .sqrt()
    };
    let denominator = norm(left) * norm(right);
    if denominator <= f64::EPSILON {
        return None;
    }
    Some((dot / denominator).clamp(-1.0, 1.0) as f32)
}

/// Keep the current threshold and margin. In live matching, an anonymous
/// cluster can also be a competing identity; final matching has no such score.
pub fn best_profile(
    embedding: &[f32],
    profiles: &[VoiceProfile],
    competing_score: Option<f32>,
) -> Option<(usize, f32)> {
    let mut ranked: Vec<_> = profiles
        .iter()
        .enumerate()
        .filter_map(|(index, profile)| {
            // A malformed sample must not change the embedding shape of its
            // profile and make a wrong-length query look like a valid match.
            if profile.embedding.len() != embedding.len() {
                return None;
            }
            std::iter::once(&profile.embedding)
                .chain(&profile.samples)
                .filter_map(|reference| similarity(embedding, reference))
                .max_by(f32::total_cmp)
                .map(|score| (index, score))
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then_with(|| profiles[a.0].id.cmp(&profiles[b.0].id))
    });
    let (index, score) = *ranked.first()?;
    let runner_up = ranked.get(1).map(|entry| entry.1).unwrap_or(-1.0).max(
        competing_score
            .filter(|score| score.is_finite())
            .unwrap_or(-1.0),
    );
    (score >= PROFILE_MATCH_THRESHOLD && score - runner_up >= PROFILE_MATCH_MARGIN)
        .then_some((index, score))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(id: &str, embedding: Vec<f32>, samples: Vec<Vec<f32>>) -> VoiceProfile {
        VoiceProfile {
            id: id.into(),
            name: id.into(),
            embedding,
            samples,
        }
    }

    #[test]
    fn a_saved_meeting_sample_can_match_when_the_blend_does_not() {
        let mut voices = vec![profile("alice", vec![0.5, 0.8660254], vec![])];
        assert!(best_profile(&[1.0, 0.0], &voices, None).is_none());
        voices[0].samples.push(vec![1.0, 0.0]);
        assert_eq!(best_profile(&[1.0, 0.0], &voices, None), Some((0, 1.0)));
    }

    #[test]
    fn samples_of_the_same_person_do_not_compete_for_the_margin() {
        let voices = vec![profile(
            "alice",
            vec![1.0, 0.0],
            vec![vec![0.99, 0.01], vec![1.0, 0.0]],
        )];
        assert_eq!(best_profile(&[1.0, 0.0], &voices, None), Some((0, 1.0)));
    }

    #[test]
    fn another_persons_sample_and_anonymous_clusters_keep_the_margin_in_force() {
        let mut voices = vec![
            profile("alice", vec![1.0, 0.0], vec![]),
            profile("bob", vec![0.0, 1.0], vec![]),
        ];
        assert!(best_profile(&[1.0, 0.0], &voices, Some(0.96)).is_none());
        voices[1].samples.push(vec![0.99, 0.01]);
        assert!(best_profile(&[1.0, 0.0], &voices, None).is_none());
    }

    #[test]
    fn legacy_blends_work_and_invalid_vectors_cannot_name_a_speaker() {
        let voices = vec![profile(
            "alice",
            vec![1.0, 0.0],
            vec![vec![f32::NAN, 1.0], vec![0.0, 0.0], vec![1.0]],
        )];
        assert_eq!(best_profile(&[2.0, 0.0], &voices, None), Some((0, 1.0)));
        for invalid in [vec![], vec![0.0, 0.0], vec![f32::INFINITY, 0.0], vec![1.0]] {
            assert!(best_profile(&invalid, &voices, None).is_none());
        }
    }
}
