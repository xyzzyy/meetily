<div align="center">
    <img src="docs/Meetily-6.png" alt="Meetily" width="420" />
    <h1>Meetily Branch</h1>
    <p><strong>A privacy-first AI meeting assistant that runs entirely on your machine.</strong></p>
    <p>
        <img src="https://img.shields.io/badge/version-0.4.1-yellow" alt="Version 0.4.1" />
        <img src="https://img.shields.io/badge/license-MIT-blue" alt="MIT License" />
        <img src="https://img.shields.io/badge/platforms-macOS%20%7C%20Windows%20%7C%20Linux-lightgrey" alt="Platforms" />
        <img src="https://img.shields.io/badge/built%20with-Tauri%202%20%2B%20Rust-orange" alt="Tauri and Rust" />
    </p>
    <img src="docs/meetily_demo.gif" width="650" alt="Meetily demo" />
</div>

---

Meetily records your meetings, transcribes them live, and writes summaries without sending audio or transcripts to the cloud. Transcription models run locally, and summaries can stay local too if you use Ollama or the built-in model.

**This repository is Meetily Branch**, a fork of [Zackriya-Solutions/meetily](https://github.com/Zackriya-Solutions/meetily). You can install it **next to the official Meetily app**. It has its own app identity and data folder, and it is built so it can never write to the official app's data. See [How Meetily Branch differs from upstream](#how-meetily-branch-differs-from-upstream).

## Contents

- [Features](#features)
- [How Meetily Branch differs from upstream](#how-meetily-branch-differs-from-upstream)
- [Getting started](#getting-started)
- [Development](#development)
- [Architecture](#architecture)
- [Where data lives](#where-data-lives)
- [Documentation](#documentation)
- [Contributing](#contributing)
- [License and acknowledgments](#license-and-acknowledgments)

## Features

| | |
|---|---|
| **Local transcription** | Live speech-to-text with **Whisper** (whisper.cpp) or **Parakeet** models running on your own hardware. |
| **Microphone + system audio** | Captures both sources at once and mixes them with RMS-based ducking and clipping prevention. Voice activity detection sends only speech to the transcriber. |
| **AI summaries** | Summarize with **Ollama** (local), a built-in local model, **Claude**, **Groq**, **OpenRouter**, **OpenAI**, or any OpenAI-compatible endpoint. Summaries are template-based and can be edited. |
| **Import & enhance** *(beta)* | Transcribe existing audio files, or re-transcribe a recorded meeting with a different model or language. |
| **Model management** | Download, switch between, and delete Whisper and Parakeet models from inside the app. Deleting asks for confirmation first. |
| **GPU acceleration** | Metal/CoreML on macOS, Vulkan or CUDA on Windows/Linux, with a CPU fallback. |

<p align="center">
    <img src="docs/home.png" width="49%" alt="Live transcription" />
    <img src="docs/summary.png" width="49%" alt="AI summary" />
</p>

## How Meetily Branch differs from upstream

Meetily Branch tracks upstream Meetily 0.4.1. It adds a few protections so you can run it on the same machine as the official app without risk to the official app's data:

- **Separate identity.** The app is named `Meetily Branch` and uses the bundle identifier `com.meetily.ai.branch`, so the operating system treats it as a different application from `com.meetily.ai`.
- **Startup identity check.** Before it opens any database or app-data file, the binary checks the identity it was compiled with. If a build carries the official `com.meetily.ai` identity, or any identity other than the branch one, it refuses to start.
- **Read-only model sharing.** If official Meetily has already downloaded models, Branch can **read** them so you don't download them twice. Downloads and deletions only happen in Branch's own models folder. Paths that are absolute, contain `..`, or point to symlinked storage are rejected.
- **No auto-updater.** Update checks are turned off on purpose, because an official Meetily update installed over Branch would bring back the official identity and storage paths. To update, rebuild from source.

## Getting started

Meetily Branch does not publish prebuilt installers, so you need to build it from source.

### Prerequisites

- **Rust** (stable) via [rustup](https://rustup.rs/)
- **Node.js** 18+ and **pnpm 9** (`npm install -g pnpm@9.15.9`)
- **Platform toolchain**
  - macOS: Xcode Command Line Tools (`xcode-select --install`) and `cmake`
  - Windows: Visual Studio Build Tools with the C++ workload, plus `cmake` and LLVM
  - Linux: `build-essential`, `cmake`, `llvm`/`clang`, `libomp`, and the Tauri system libraries (see [Building on Linux](docs/building_in_linux.md))

### Build and run

```bash
git clone https://github.com/xyzzyy/meetily.git meetily-branch
cd meetily-branch/frontend
pnpm install --frozen-lockfile

# Development build with hot reload. GPU features are picked automatically.
pnpm run tauri:dev

# Production bundle (.app/.dmg on macOS, installer on Windows)
pnpm run tauri:build
```

The finished bundle is written to `frontend/src-tauri/target/release/bundle/`.

There are also wrapper scripts that clean and rebuild:

| Platform | Run (dev) | Build (release) |
|---|---|---|
| macOS / Linux | `./clean_run.sh` (add `debug` for verbose logs) | `./clean_build.sh` |
| Windows | `clean_run_windows.bat` | `clean_build_windows.bat` |

For detailed platform instructions, see [docs/BUILDING.md](docs/BUILDING.md).

### First launch on macOS

When you first start recording, grant **Microphone** and **Screen Recording** permissions. macOS needs Screen Recording permission for system audio capture through ScreenCaptureKit, which requires macOS 13 or later.

## Development

All app behavior lives in the Tauri app under `frontend/`. The Next.js UI calls the Rust core through Tauri commands, and the Rust core sends updates back to the UI as events.

### Picking a GPU backend

`tauri:dev` and `tauri:build` detect your hardware. To force a specific backend:

```bash
pnpm run tauri:dev:metal    # macOS
pnpm run tauri:dev:cuda     # NVIDIA
pnpm run tauri:dev:vulkan   # AMD / Intel
pnpm run tauri:dev:cpu      # no GPU
```

There are matching `tauri:build:*` scripts for each backend. For details, see [docs/GPU_ACCELERATION.md](docs/GPU_ACCELERATION.md).

### Logging

```bash
RUST_LOG=debug ./clean_run.sh                  # everything
RUST_LOG=app_lib::audio=debug ./clean_run.sh   # audio pipeline only
```

To open DevTools in the app, press `Cmd+Shift+I` on macOS or `Ctrl+Shift+I` on Windows/Linux.

### Tests

```bash
cd frontend/src-tauri && cargo test                              # Rust core
node --test frontend/tests/components/model-deletion.test.mjs    # from repo root
cd frontend && bun test tests/hooks                              # React hook tests
```

## Architecture

```
┌──────────────────────────── Meetily Branch (Tauri 2) ────────────────────────────┐
│                                                                                   │
│   Next.js 14 / React 18 UI  ◄── events ──  Rust core  ──►  Whisper / Parakeet     │
│        (frontend/src)        ── commands ►  (src-tauri)     (local STT)           │
│                                               │                                   │
│                              SQLite storage · summary engine · LLM providers      │
└───────────────────────────────────────────────────────────────────────────────────┘
```

The audio pipeline splits into two paths. The **recording path** mixes microphone and system audio and saves it to disk. The **transcription path** runs voice activity detection and passes only speech segments to the transcription engine.

| Area | Location |
|---|---|
| Command registration and app setup | `frontend/src-tauri/src/lib.rs` |
| Audio capture, mixing, VAD | `frontend/src-tauri/src/audio/` |
| Transcription engines | `frontend/src-tauri/src/whisper_engine/`, `parakeet_engine/` |
| Summaries and LLM clients | `frontend/src-tauri/src/summary/`, `ollama/`, `anthropic/`, `groq/`, `openrouter/`, `openai/` |
| Storage and identity safety | `frontend/src-tauri/src/database/`, `storage_safety.rs`, `model_storage.rs` |
| UI | `frontend/src/` |

For more detail, see [docs/architecture.md](docs/architecture.md).

> **Note:** The `backend/` directory holds the old Python/FastAPI server. It is archived and **not supported**. The current app doesn't use it, so don't run or deploy it.

## Where data lives

Meetily Branch stores everything in the app-data directory for its own identifier:

| Platform | Location |
|---|---|
| macOS | `~/Library/Application Support/com.meetily.ai.branch/` |
| Windows | `%APPDATA%\com.meetily.ai.branch\` |
| Linux | `~/.local/share/com.meetily.ai.branch/` |

Models go in the `models/` subfolder. If official Meetily is installed, its `com.meetily.ai/models/` folder is used only as a read-only fallback.

## Documentation

- [Building from source](docs/BUILDING.md) · [Building on Linux](docs/building_in_linux.md)
- [GPU acceleration](docs/GPU_ACCELERATION.md)
- [Architecture](docs/architecture.md)
- [Privacy policy](PRIVACY_POLICY.md)

## Contributing

Issues and pull requests are welcome. Branch from `main`, using `fix/*` for bug fixes and `enhance/*` for features, then open a PR back into `main`. Read [CONTRIBUTING.md](CONTRIBUTING.md) before you start.

To contribute to the official app instead, open your change against [upstream Meetily](https://github.com/Zackriya-Solutions/meetily).

## License and acknowledgments

Released under the [MIT License](LICENSE.md).

Meetily is created by [Zackriya Solutions](https://meetily.ai). This fork builds on their work and on:

- [whisper.cpp](https://github.com/ggerganov/whisper.cpp)
- [Screenpipe](https://github.com/mediar-ai/screenpipe)
- [transcribe-rs](https://crates.io/crates/transcribe-rs)
- NVIDIA's **Parakeet** model, with the [ONNX conversion by istupakov](https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx)
