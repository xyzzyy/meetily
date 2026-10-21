# Pull request build checks

`.github/workflows/pr-build.yml` runs for pull requests to `main`, pushes to
`main`, and manual requests from the Actions tab.

The workflow has two checks:

- **Frontend build and tests** installs the locked dependencies, runs the
  build guard tests, the existing frontend suite one file per process, and the
  diarization benchmark tests, then builds the production frontend on Ubuntu.
- **macOS release build and tests** builds the summary helper, the speaker
  helper when present, and the app on an Apple Silicon Mac with GitHub's
  `macos-latest` runner. It also runs the diarization interval tests and the
  Rust unit tests. Xcode and the Apple SDKs are already installed on the
  runner.

The macOS job sets `SDKROOT` to the SDK from the active Xcode installation.
This keeps the Swift compiler and the speaker helper's C++ headers on the
same SDK when Command Line Tools are also installed. The Rust cache key also
includes the active Xcode version so build-script paths from an older runner
image are not reused.

The frontend is the React and Next.js interface shown in the Tauri window.
The frontend job exports its production files to `frontend/out`. The macOS
job uses those files when it compiles the native app with Cargo's release
settings.

The existing frontend tests cover model deletion commands, setup model
selection, transcription provider selection, summary state recovery,
transcript request handling, language preferences, summary parsing, Markdown
conversion error handling, and analytics event limits. They use simulated
native responses. They do not record audio, run model inference, remove real
model files, or start the desktop app. The summary parser accepts a summary
object or its JSON encoding and rejects malformed JSON.

The diarization benchmark tests use Python's unittest runner. They cover the
scorer's speaker mapping and overlap calculations and do not call the
ElevenLabs API or read audio files.

The diarization interval tests compile the pure interval processing source
with `swiftc` and run it as a small executable. They cover overlap splitting,
speaker candidates, and interval normalization without FluidAudio, downloaded
models, or audio devices.

The native tests run on macOS after the app build. They use temporary files
and fixtures; none require audio hardware or system permissions. The workflow
does not test Windows or Linux native builds, app packaging, signing, or
notarization. Use the existing manual build workflows for those build steps.

The checks use a read-only GitHub token and no repository secrets. They do not
publish releases or install the app. Frontend output is kept for three days
so the native job can use it. A new commit cancels an older run for the same
pull request.

To run the frontend build locally, install Node.js 22, pnpm 9.15.9, and
Bun 1.3.14. Then run:

```sh
cd frontend
pnpm install --frozen-lockfile
pnpm build
```

The test step in `.github/workflows/pr-build.yml` shows how the suite is run:
each file gets its own process and uses either Bun or Node's test runner,
depending on its imports. The build guard tests run from the repository root:

```sh
node --test .github/verify-portable-ggml.test.cjs
python3 -m unittest discover -s diarization-sidecar/Benchmarks -p 'test_*.py' -v
```

To run the diarization interval tests locally, compile the source and test
harness directly:

```sh
swiftc \
  diarization-sidecar/Sources/meetily-diarization/IntervalProcessing.swift \
  diarization-sidecar/Tests/IntervalProcessing/main.swift \
  -o /tmp/meetily-interval-tests
/tmp/meetily-interval-tests
```

To run the native tests locally, use the macOS job steps in the workflow. The
frontend output and the helper binaries must be built first. The tests then
run with:

```sh
cargo test --locked --release -p meetily --lib
cargo test --locked --release -p llama-helper --features metal
```
