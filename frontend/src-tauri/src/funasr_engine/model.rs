use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const MODEL_REVISION: &str = "bf67c7b38744a60f738655f6a288c9ab47417670";
pub const MODEL_REPOSITORY: &str = "FunAudioLLM/Fun-ASR-Nano-GGUF";
pub const DEFAULT_FUNASR_MODEL: &str = crate::config::DEFAULT_FUNASR_MODEL;
pub const MODEL_DIR: &str = "funasr/fun-asr-nano-2512";
pub const ENCODER_FILE: &str = "funasr-encoder-f16.gguf";

#[derive(Debug, Clone, Copy)]
pub struct ArtifactSpec {
    pub filename: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

pub const ENCODER: ArtifactSpec = ArtifactSpec {
    filename: ENCODER_FILE,
    size: 469_331_008,
    sha256: "f92f91d01a24fbed6c863495b2ee8c6a6788144a02858b75743f0946668de8a2",
};

#[derive(Debug, Clone, Copy)]
pub struct FunAsrModelSpec {
    pub id: &'static str,
    pub display_name: &'static str,
    pub description: &'static str,
    pub decoder: ArtifactSpec,
}

pub const MODELS: &[FunAsrModelSpec] = &[
    FunAsrModelSpec {
        id: "funasr-nano-2512-q4km",
        display_name: "FUN-ASR Nano Q4_K_M",
        description: "Default · Smallest download (484 MB decoder)",
        decoder: ArtifactSpec {
            filename: "qwen3-0.6b-q4km.gguf",
            size: 484_219_776,
            sha256: "cc5057552aa9dddedcda73ea8889854e8a257eb07d0a561b7234465c1e856f22",
        },
    },
    FunAsrModelSpec {
        id: "funasr-nano-2512-q5km",
        display_name: "FUN-ASR Nano Q5_K_M",
        description: "5-bit decoder (551 MB) · Not yet compared in Meetily",
        decoder: ArtifactSpec {
            filename: "qwen3-0.6b-q5km.gguf",
            size: 551_377_792,
            sha256: "dc2e6e195c534cbaea208c030d3ab55be4d3385ae4b726966cff8639a69aa2a2",
        },
    },
    FunAsrModelSpec {
        id: "funasr-nano-2512-q8_0",
        display_name: "FUN-ASR Nano Q8_0",
        description: "8-bit decoder (805 MB) · Largest download · Not yet compared in Meetily",
        decoder: ArtifactSpec {
            filename: "qwen3-0.6b-q8_0.gguf",
            size: 804_753_280,
            sha256: "819f385dc0e035dccc3d9e7edaf6b7b044b8ba7ace63cbcbf84c7e397eecbf27",
        },
    },
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ModelStatus {
    Available,
    Missing,
    Downloading { progress: u8 },
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub name: String,
    pub display_name: String,
    pub path: PathBuf,
    pub encoder_available: bool,
    pub has_partial_download: bool,
    pub size_mb: u64,
    pub decoder_size_mb: u64,
    pub encoder_size_mb: u64,
    pub description: String,
    pub recommended: bool,
    pub status: ModelStatus,
}

#[derive(Debug, Clone, Serialize)]
pub struct DownloadProgress {
    pub model_name: String,
    pub phase: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub downloaded_mb: f64,
    pub total_mb: f64,
    pub speed_mbps: f64,
    pub percent: u8,
}

pub fn model_spec(model_id: &str) -> Option<&'static FunAsrModelSpec> {
    MODELS.iter().find(|model| model.id == model_id)
}

pub fn encoder_relative_path() -> PathBuf {
    Path::new(MODEL_DIR).join(ENCODER_FILE)
}

pub fn decoder_relative_path(model: &FunAsrModelSpec) -> PathBuf {
    Path::new(MODEL_DIR)
        .join("decoders")
        .join(model.decoder.filename)
}

pub fn artifact_url(artifact: ArtifactSpec) -> String {
    format!(
        "https://huggingface.co/{MODEL_REPOSITORY}/resolve/{MODEL_REVISION}/{}",
        artifact.filename
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn catalog_has_three_unique_models() {
        assert_eq!(MODELS.len(), 3);
        let ids = MODELS.iter().map(|model| model.id).collect::<HashSet<_>>();
        assert_eq!(ids.len(), MODELS.len());
        assert!(model_spec(DEFAULT_FUNASR_MODEL).is_some());
    }

    #[test]
    fn every_artifact_has_a_sha256_and_size() {
        for artifact in MODELS
            .iter()
            .map(|model| model.decoder)
            .chain(std::iter::once(ENCODER))
        {
            assert_eq!(artifact.sha256.len(), 64);
            assert!(artifact.size > 0);
        }
    }
}
