# Pull request build checks

`.github/workflows/pr-build.yml` runs for pull requests, pushes to
`main`, and manual requests from the Actions tab. Pushes to `main` touching
only ignored documentation or template paths (`docs/**`, `**.md`,
`.github/ISSUE_TEMPLATE/**`) skip the workflow.

The workflow splits Ubuntu checks (`ubuntu-24.04`) from native builds and tests
on pinned Apple Silicon runners (`macos-15`):

- **Ubuntu checks (`ubuntu-24.04`):** `scope` classifies changed PR paths
  fail-open as `docs`, `frontend`, or `all` (`docs` skips frontend unit tests,
  `frontend-build`, browser tests, and macOS jobs; `frontend` skips
  `rust-tests`; errors or unrecognized paths run `all`). Fast unit suites
  (`node-quick`, diarization benchmarks, FUN-ASR embedding tests, Swift interval
  tests, and per-file frontend unit tests) run alongside `frontend-build` (with
  `.next/cache`). After `frontend-build` succeeds, a `[chromium, webkit]` matrix
  runs all Playwright release specs (`fullyParallel`, 4 CI workers), including
  the normal-usage walkthrough.
- **Rust unit and doc tests (`macos-15`):** run `--lib` then `--doc` in their
  own job alongside the app build, using lightweight `frontend/out` and sidecar
  stubs for `tauri_build` and a dedicated Cargo cache.
- **macOS app build, helpers, and launch (`macos-15`):** restores
  content-addressed binary caches for the diarization, FUN-ASR, and Llama
  helpers (building any missed helper cleanly and running `llama-helper` unit
  tests on a Llama binary cache miss before bundling), bundles and verifies the
  ad hoc signed release arm64 `.app` with the standalone `cargo-tauri` CLI,
  uploads `meetily-macos-arm64-app`, and runs the unpacked `.app` launch and
  setup UI test (`test-macos-app.sh`). Xcode and the Apple SDKs are already
  installed on the runner.
- **OpenCode review (`ubuntu-24.04`):** runs after all non-skipped build and
  test jobs pass on pull requests (skipped on `push: main`, `workflow_dispatch`,
  when any build/test job fails or is cancelled, or when `OPENCODE_API_KEY` is
  unset).

The workspace `.cargo/config.toml` and `.github/actions/setup-native` align
`MACOSX_DEPLOYMENT_TARGET=14.2` and `SDKROOT` so Cargo fingerprints match
across repository-root and `frontend/` invocations and keep Swift and C++
headers on the active Xcode SDK. Rust cache keys include the active Xcode
release, Xcode build, and macOS SDK build identifier.

The frontend is the React and Next.js interface shown in the Tauri window.
The frontend job exports its production files to `frontend/out`. The macOS
job uses those files when it compiles the native app with Cargo's release
settings.

The existing frontend tests cover model deletion commands, setup model
selection, transcription provider selection, summary state recovery,
transcript request handling, language preferences, summary parsing, and
Markdown conversion error handling. They use simulated native responses. They
do not record audio, run model inference, remove real model files, or start
the desktop app. The summary parser accepts a summary object or its JSON
encoding and rejects malformed JSON.

The diarization benchmark tests use Python's unittest runner. They cover the
scorer's speaker mapping and overlap calculations and do not call the
ElevenLabs API or read audio files.

The diarization interval tests compile the pure interval processing source
with `swiftc` and run it as a small executable. They cover overlap splitting,
speaker candidates, and interval normalization without FluidAudio, downloaded
models, or audio devices.

The Rust native tests run in their own macOS job (`rust-tests`) using temporary
files, fixtures, and placeholder `frontend/out` and sidecar stubs for
`tauri_build`; none require audio hardware or system permissions. The summary
template documentation example also runs as a Rust documentation test in the
same job. The macOS job packages and verifies an ad hoc signed `.app` bundle.
The workflow does not test Windows or Linux native builds, DMG packaging,
Developer ID signing, or notarization. Use the existing manual build workflows
for those steps.

The build and test checks use a read-only GitHub token and no repository
secrets; `OpenCode review` requests `pull-requests: write` and `issues: write`
(and reads `OPENCODE_API_KEY` when configured) to post review comments. They do
not publish releases or install the app. Frontend output is kept for three days
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
cargo test --locked --release -p meetily --doc
cargo test --locked --release -p llama-helper --features metal
```
