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

**This repository is Meetily Branch**, a fork of [Zackriya-Solutions/meetily](https://github.com/Zackriya-Solutions/meetily). You can install it **next to the official Meetily app**. It has its own app identity and data folder, so your meetings, recordings, and models stay separate from the official app's. See [How Meetily Branch differs from upstream](#how-meetily-branch-differs-from-upstream).

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
- **Read-only model sharing.** If official Meetily has already downloaded models, Branch can **read** them so you don't download them twice. Downloads and deletions are confined to Branch's own models folder. Absolute paths and paths containing a `..` component are rejected, so a lookup can't escape the models directories.

More updates coming soon.

## Getting started

Meetily Branch does not publish prebuilt installers, so you need to build it from source.

### Prerequisites

- **Rust** (stable) via [rustup](https://rustup.rs/)
- **Node.js** 18+ and **pnpm 9** (`npm install -g pnpm@9.15.9`)
- **Bun** *(only needed for the React hook tests)* via [bun.sh](https://bun.sh/)
- **Platform toolchain**
  - macOS: Xcode Command Line Tools (`xcode-select --install`) and `cmake`
  - Windows: Visual Studio Build Tools with the C++ workload, plus `cmake` and LLVM
  - Linux: `build-essential`, `cmake`, `llvm`/`clang`, `libomp`, and the Tauri system libraries (see [Building on Linux](docs/building_in_linux.md))

### Build and run

```bash
git clone https://github.com/xyzzyy/meetily.git meetily-branch
cd meetily-branch/frontend
pnpm install --frozen-lockfile

# Development build with hot reload
./dev-gpu.sh

# Production bundle (.app/.dmg on macOS, installer on Windows)
./build-gpu.sh
```

On Windows, use `dev-gpu.bat` / `build-gpu.bat` instead. The `.ps1` versions always use Vulkan and don't build the sidecar, so avoid them on a fresh clone.

These scripts pick a GPU backend for your hardware. They also build the `llama-helper` sidecar that runs the built-in summary model and copy it into `src-tauri/binaries/`. Running `pnpm run tauri:build` on its own skips that step, so on a fresh clone the bundle fails because the sidecar binary is missing. Run one of the GPU scripts at least once first.

> **AMD ROCm or OpenBLAS-only systems:** auto-detection picks `hipblas` or `openblas`, but the sidecar only supports `metal`, `cuda`, and `vulkan`, so its build fails before Tauri starts. Force a supported backend instead with `TAURI_GPU_FEATURE=vulkan ./build-gpu.sh` (needs the Vulkan SDK); `TAURI_GPU_FEATURE=metal` or `cuda` work the same way on hardware that supports them. `none` is not a sidecar feature, so it can't be used to force a CPU-only build. The Windows `.bat` scripts always re-run detection and overwrite a pre-set value, so this workaround applies to the shell scripts.

The repository root is a Cargo workspace, so the finished bundle is written to `target/release/bundle/` at the **repository root**, not under `frontend/`.

For detailed platform instructions, see [docs/BUILDING.md](docs/BUILDING.md).

### First launch on macOS

When you first start recording, grant **Microphone** and **Screen Recording** permissions. macOS needs Screen Recording permission for system audio capture through ScreenCaptureKit, which requires macOS 13 or later.

## Development

All app behavior lives in the Tauri app under `frontend/`. The Next.js UI calls the Rust core through Tauri commands, and the Rust core sends updates back to the UI as events.

### Picking a GPU backend

`dev-gpu.sh`, `build-gpu.sh`, `tauri:dev`, and `tauri:build` detect your hardware. To force a specific backend (after the sidecar has been built once):

```bash
pnpm run tauri:dev:metal    # macOS
pnpm run tauri:dev:cuda     # NVIDIA
pnpm run tauri:dev:vulkan   # AMD / Intel
pnpm run tauri:dev:cpu      # CPU only on Windows/Linux
```

On macOS, Whisper always builds with Metal and CoreML, so `tauri:dev:cpu` still uses the GPU there.

There are matching `tauri:build:*` scripts for each backend. For details, see [docs/GPU_ACCELERATION.md](docs/GPU_ACCELERATION.md).

### Logging

The app logs to the terminal you launched it from. The log level is fixed at `info` in code (`frontend/src-tauri/src/main.rs` and `lib.rs`), so setting `RUST_LOG` has no effect. To get more detailed logs, change the level there and rebuild.

To open DevTools in the app, press `Cmd+Shift+I` on macOS or `Ctrl+Shift+I` on Windows/Linux.

### Tests

```bash
# Run each from the repository root
(cd frontend/src-tauri && cargo test)                           # Rust core
node --test frontend/tests/components/model-deletion.test.mjs   # component tests
(cd frontend && bun test tests/hooks)                           # React hook tests (needs Bun)
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

Meetily Branch keeps its meeting database, settings, and models in the app-data directory for its own identifier:

| Platform | Location |
|---|---|
| macOS | `~/Library/Application Support/com.meetily.ai.branch/` |
| Windows | `%APPDATA%\com.meetily.ai.branch\` |
| Linux | `~/.local/share/com.meetily.ai.branch/` |

Models go in the `models/` subfolder. If official Meetily is installed, its `com.meetily.ai/models/` folder is used only as a read-only fallback.

Audio recordings are saved outside app-data in a `meetily-branch-recordings` folder. Official Meetily uses `meetily-recordings`, so the two apps keep separate recordings. The recording settings show this path and an **Open Folder** button; the location itself is fixed.

| Platform | Default recordings folder |
|---|---|
| macOS | `~/Movies/meetily-branch-recordings/` |
| Windows | `%USERPROFILE%\Music\meetily-branch-recordings\` |
| Linux | `~/Documents/meetily-branch-recordings/` |

Notification settings are the one exception: they are written to `meetily/notifications.json` in the OS config directory (`~/Library/Application Support/meetily/` on macOS, `%APPDATA%\meetily\` on Windows, `~/.config/meetily/` on Linux). Official Meetily uses the same file, so if both apps are installed they share notification settings.

## Documentation

- [Building from source](docs/BUILDING.md) · [Building on Linux](docs/building_in_linux.md)
- [GPU acceleration](docs/GPU_ACCELERATION.md)
- [Architecture](docs/architecture.md)
- [Privacy policy](PRIVACY_POLICY.md)

## Contributing

Issues and pull requests are welcome. Branch from `main`, using `fix/*` for bug fixes and `enhance/*` for features, then open a PR back into `main`. [CONTRIBUTING.md](CONTRIBUTING.md) covers code style and PR expectations. It was inherited from upstream, so wherever it says `devtest`, use `main` in this fork.

To contribute to the official app instead, open your change against [upstream Meetily](https://github.com/Zackriya-Solutions/meetily).

## License and acknowledgments

Released under the [MIT License](LICENSE.md).

Meetily is created by [Zackriya Solutions](https://meetily.ai). This fork builds on their work and on:

- [whisper.cpp](https://github.com/ggerganov/whisper.cpp)
- [Screenpipe](https://github.com/mediar-ai/screenpipe)
- [transcribe-rs](https://crates.io/crates/transcribe-rs)
- NVIDIA's **Parakeet** model, with the [ONNX conversion by istupakov](https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx)
