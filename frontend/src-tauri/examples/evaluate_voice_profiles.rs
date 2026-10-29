//! Offline comparison of profile blends with saved meeting samples.
//! cargo run --release -p meetily --example evaluate_voice_profiles -- MODEL MANIFEST REPORT
use anyhow::{bail, Context, Result};
use app_lib::{
    audio::decoder::decode_audio_file,
    database::repositories::speaker_profile::blend_embeddings,
    diarization::{
        embedding::EmbeddingExtractor,
        models,
        profile_matching::{best_profile, VoiceProfile},
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
struct Clip {
    path: PathBuf,
    sha256: String,
}
#[derive(Deserialize)]
struct Enrollment {
    id: String,
    clips: Vec<Clip>,
}
#[derive(Deserialize)]
struct Probe {
    expected_profile: Option<String>,
    clip: Clip,
}
#[derive(Deserialize)]
struct Manifest {
    profiles: Vec<Enrollment>,
    probes: Vec<Probe>,
}

#[derive(Debug, Default, PartialEq, Serialize)]
struct Metrics {
    known_probes: usize,
    correct_names: usize,
    wrong_names: usize,
    missed_names: usize,
    unknown_probes: usize,
    correctly_unmatched: usize,
    false_names: usize,
}
impl Metrics {
    fn observe(&mut self, expected: Option<&str>, actual: Option<&str>) {
        if let Some(expected) = expected {
            self.known_probes += 1;
            match actual {
                Some(actual) if actual == expected => self.correct_names += 1,
                Some(_) => self.wrong_names += 1,
                None => self.missed_names += 1,
            }
        } else {
            self.unknown_probes += 1;
            if actual.is_some() {
                self.false_names += 1;
            } else {
                self.correctly_unmatched += 1;
            }
        }
    }
}
#[derive(Serialize)]
struct Prediction {
    profile: Option<String>,
    score: Option<f32>,
}
fn predict(embedding: &[f32], profiles: &[VoiceProfile]) -> Prediction {
    match best_profile(embedding, profiles, None) {
        Some((index, score)) => Prediction {
            profile: Some(profiles[index].id.clone()),
            score: Some(score),
        },
        None => Prediction {
            profile: None,
            score: None,
        },
    }
}
#[derive(Serialize)]
struct Case {
    path: PathBuf,
    expected_profile: Option<String>,
    seconds_used: f64,
    blend_only: Prediction,
    saved_samples: Prediction,
}
#[derive(Serialize)]
struct Report {
    manifest_sha256: String,
    model_sha256: String,
    embedding_version: String,
    profile_threshold: f32,
    profile_margin: f32,
    blend_only: Metrics,
    saved_samples: Metrics,
    cases: Vec<Case>,
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn validate(manifest: &Manifest, root: &Path) -> Result<HashMap<PathBuf, PathBuf>> {
    if manifest.profiles.is_empty() || manifest.probes.is_empty() {
        bail!("Provide enrolment profiles and probe recordings.");
    }
    let mut ids = HashSet::new();
    for profile in &manifest.profiles {
        if profile.id.trim().is_empty()
            || !ids.insert(profile.id.as_str())
            || profile.clips.is_empty()
        {
            bail!(
                "Profile IDs must be unique and non-empty, with at least one enrolment recording."
            );
        }
    }
    for probe in &manifest.probes {
        if probe
            .expected_profile
            .as_deref()
            .is_some_and(|id| !ids.contains(id))
        {
            bail!("A probe refers to a profile that has no enrolment recordings.");
        }
    }
    let mut paths = HashMap::new();
    let mut contents = HashSet::new();
    for clip in manifest
        .profiles
        .iter()
        .flat_map(|profile| &profile.clips)
        .chain(manifest.probes.iter().map(|probe| &probe.clip))
    {
        let canonical = fs::canonicalize(root.join(&clip.path))
            .with_context(|| format!("Missing recording: {}", clip.path.display()))?;
        let actual = digest(&fs::read(&canonical)?);
        if actual != clip.sha256 {
            bail!("Checksum mismatch: {}", clip.path.display());
        }
        if !contents.insert(actual) {
            bail!(
                "A recording is repeated in enrolment or probes: {}",
                clip.path.display()
            );
        }
        paths.insert(clip.path.clone(), canonical);
    }
    Ok(paths)
}

fn extract(extractor: &mut EmbeddingExtractor, path: &Path) -> Result<(Vec<f32>, f64)> {
    let decoded = decode_audio_file(path)?;
    let audio = decoded.to_whisper_format();
    let audio = &audio[..audio.len().min(30 * 16_000)];
    if audio.len() < 16_000 {
        bail!(
            "Use recordings with at least one second of audio: {}",
            path.display()
        );
    }
    Ok((extractor.compute(audio)?, audio.len() as f64 / 16_000.0))
}

fn run(model: &Path, manifest_path: &Path) -> Result<Report> {
    models::validate_model_file(model).map_err(|error| anyhow::anyhow!(error))?;
    let bytes = fs::read(manifest_path)?;
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    let paths = validate(&manifest, manifest_path.parent().unwrap_or(Path::new(".")))?;
    let mut extractor = EmbeddingExtractor::new(model)?;
    let mut profiles = Vec::new();
    for profile in &manifest.profiles {
        let mut blend = Vec::new();
        let mut seconds = 0.0;
        let mut samples = Vec::new();
        for clip in &profile.clips {
            eprintln!("Enrolment: {}", clip.path.display());
            let (embedding, duration) = extract(&mut extractor, &paths[&clip.path])?;
            blend = if blend.is_empty() {
                embedding.clone()
            } else {
                blend_embeddings(&blend, seconds, &embedding, duration)
            };
            seconds += duration;
            samples.push(embedding);
        }
        profiles.push(VoiceProfile {
            id: profile.id.clone(),
            name: profile.id.clone(),
            embedding: blend,
            samples,
        });
    }
    let blends: Vec<_> = profiles
        .iter()
        .cloned()
        .map(|mut profile| {
            profile.samples.clear();
            profile
        })
        .collect();
    let mut report = Report {
        manifest_sha256: digest(&bytes),
        model_sha256: models::EMBEDDING_MODEL_SHA256.into(),
        embedding_version: models::EMBEDDING_MODEL_VERSION.into(),
        profile_threshold: app_lib::diarization::clustering::PROFILE_MATCH_THRESHOLD,
        profile_margin: app_lib::diarization::clustering::PROFILE_MATCH_MARGIN,
        blend_only: Metrics::default(),
        saved_samples: Metrics::default(),
        cases: Vec::new(),
    };
    for probe in &manifest.probes {
        eprintln!("Probe: {}", probe.clip.path.display());
        let (embedding, seconds_used) = extract(&mut extractor, &paths[&probe.clip.path])?;
        let blend_only = predict(&embedding, &blends);
        let saved_samples = predict(&embedding, &profiles);
        report.blend_only.observe(
            probe.expected_profile.as_deref(),
            blend_only.profile.as_deref(),
        );
        report.saved_samples.observe(
            probe.expected_profile.as_deref(),
            saved_samples.profile.as_deref(),
        );
        report.cases.push(Case {
            path: probe.clip.path.clone(),
            expected_profile: probe.expected_profile.clone(),
            seconds_used,
            blend_only,
            saved_samples,
        });
    }
    Ok(report)
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        bail!("Usage: evaluate_voice_profiles MODEL.onnx MANIFEST.json REPORT.json");
    }
    if Path::new(&args[2]).exists() {
        bail!("The report already exists. Choose another output path.");
    }
    let report = run(Path::new(&args[0]), Path::new(&args[1]))?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args[2])?;
    serde_json::to_writer_pretty(&mut file, &report)?;
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"blend_only": report.blend_only, "saved_samples": report.saved_samples})
        )?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wrong_names_and_missed_names_are_counted_separately_from_unknown_speakers() {
        let mut scores = Metrics::default();
        for (expected, actual) in [
            (Some("alice"), Some("alice")),
            (Some("alice"), Some("bob")),
            (Some("alice"), None),
            (None, None),
            (None, Some("alice")),
        ] {
            scores.observe(expected, actual);
        }
        assert_eq!(
            scores,
            Metrics {
                known_probes: 3,
                correct_names: 1,
                wrong_names: 1,
                missed_names: 1,
                unknown_probes: 2,
                correctly_unmatched: 1,
                false_names: 1
            }
        );
    }
    #[test]
    fn a_copied_recording_cannot_be_used_in_both_enrolment_and_probe_sets() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("first.wav"), b"audio").unwrap();
        fs::write(directory.path().join("copy.wav"), b"audio").unwrap();
        let clip = |path: &str| Clip {
            path: path.into(),
            sha256: digest(b"audio"),
        };
        let manifest = Manifest {
            profiles: vec![Enrollment {
                id: "alice".into(),
                clips: vec![clip("first.wav")],
            }],
            probes: vec![Probe {
                expected_profile: Some("alice".into()),
                clip: clip("copy.wav"),
            }],
        };
        assert!(validate(&manifest, directory.path())
            .unwrap_err()
            .to_string()
            .contains("repeated"));
    }
}
