//! Pinned Nemotron files. Sizes are bytes; digests are SHA-256.

/// One downloaded file.
pub struct Artifact {
    pub name: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

/// A decoder the user can choose. Every decoder uses the shared encoder.
pub struct Decoder {
    pub id: &'static str,
    pub display_name: &'static str,
    pub memory: &'static str,
    pub file: Artifact,
}

/// AtomicChat's GGUF conversion for NVIDIA's NeMo-Speech.cpp, at a fixed revision.
pub const REPO: &str = "AtomicChat/NVIDIA-Nemotron-3.5-Transcribe-30B-A4B-GGUF";
pub const REVISION: &str = "dbd06c5cad7a2f33c8e06be2fb3ef814dbd9aa59";

/// Speech encoder, speaker branch and CTC timestamp head. The decoder names it,
/// so the file name must not change.
pub const ENCODER: Artifact = Artifact {
    name: "NVIDIA-Nemotron-3.5-Transcribe-30B-A4B-Encoder-F16.gguf",
    size: 1_670_410_080,
    sha256: "4221029c8a565c4a80c507ffa760c716e595acf663f9f4273ce69910050e7ba2",
};

pub const DECODERS: &[Decoder] = &[
    Decoder {
        id: "nemotron-3.5-transcribe-q2_0_l",
        display_name: "Nemotron 3.5 Transcribe · Q2_0_L",
        memory: "24 GB memory or more",
        file: Artifact {
            name: "NVIDIA-Nemotron-3.5-Transcribe-30B-A4B-AD-Q2_0_L.gguf",
            size: 15_547_824_288,
            sha256: "67eee66a7bb205867e3f9b7c0944e287fc4d2661db2280030e1c26e6ebdea7b4",
        },
    },
    Decoder {
        id: "nemotron-3.5-transcribe-iq4_nl",
        display_name: "Nemotron 3.5 Transcribe · IQ4_NL",
        memory: "32 GB memory or more",
        file: Artifact {
            name: "NVIDIA-Nemotron-3.5-Transcribe-30B-A4B-AD-IQ4_NL.gguf",
            size: 19_678_656_672,
            sha256: "0c47d4ab6c783e7606ec03714b354fab0b2052027a7ea1991e7e2d3009c345ad",
        },
    },
];

/// NVIDIA's NeMo-Speech.cpp Metal build at commit 66d50d9 (nightly of
/// 2026-10-28). NVIDIA replaces its nightly archive each day, so the verified
/// archive is mirrored on the fork's releases.
pub const RUNTIME: Artifact = Artifact {
    name: "nemo-speech-nightly-66d50d9-macos-aarch64-metal.tar.gz",
    size: 7_234_179,
    sha256: "4621515b0050d283d3a2a3f0382631ee7f876eea46608fdb52f7a05f50c958fc",
};
pub const RUNTIME_URL: &str = "https://github.com/xyzzyy/meetily/releases/download/nemo-speech-66d50d9/nemo-speech-nightly-66d50d9-macos-aarch64-metal.tar.gz";
/// The folder inside the archive.
pub const RUNTIME_DIR: &str = "nemo-speech-nightly-macos-aarch64-metal";
pub const RUNTIME_VERSION: &str = "0.2.0+nightly.66d50d9";
