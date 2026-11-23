# PR checks and normal usage

The PR workflow is `.github/workflows/pr-build.yml`. Each matrix entry has its
own result. A failed test does not cancel an independent test.

The short Node, Python, C++, and Swift checks start at once on `ubuntu-24.04`,
alongside `scope` (which classifies PR diffs fail-open as `docs`, `frontend`, or
`all`). On non-docs PRs, frontend unit tests, the production frontend build
(with `.next/cache`), and—when native/Rust inputs change—stub-decoupled Rust
unit and doc tests start right away. The macOS app job needs only the frontend
artifact: it restores cached helper binaries (building any missed helper and
running `llama-helper` tests on a Llama cache miss), bundles and uploads the
`.app`, and runs the native launch check. A `[chromium, webkit]` browser matrix
runs all Playwright release specs (including `normal-usage.spec.ts`) in parallel
against the production frontend artifact. These are artifact dependencies; unit
test results do not block unrelated checks.

Native Rust tests (`pr-macos-rust-tests`) and the app build (`pr-macos-release`)
use separate release caches keyed on the active Xcode release, Xcode build, and
macOS SDK build, excluding workspace crates. The app uses
`ditto -c -k --keepParent` and `ditto -x -k` to keep permissions, symlinks, and
the ad-hoc signature. The two macOS jobs run on `macos-15` and verify arm64; the
app job runs `cargo tauri build --bundles app` and skips the DMG. See
`docs/ci.md` for full workflow and cache details.

`normal-usage.spec.ts` is one session, run in Chromium and WebKit. It starts on
Home with setup complete. It starts a recording, receives a live transcript,
pauses and resumes, stops and saves, generates a summary, follows a transcript
reference, edits and saves notes, renames and reloads the meeting, searches its
transcript, and deletes it. The fixture retains saved data across page reloads.
Unexpected IPC commands, page errors, and console errors fail the test.

The browser tests use the production static export. Only the Tauri IPC boundary
is replaced. React, navigation, recording state, transcript buffering, IndexedDB,
and the note editor run as built. Fixture code is in `e2e/`, outside the app's
imports. It provides fixed device, transcript, storage, and model responses.
It does not test native capture, transcription or summary models, SQLite, or
external providers.

The native check downloads the unchanged release `.app`, verifies its signature,
and uses XCTest to launch it. It checks for an app window and the real setup
button, clicks **Get Started**, and checks that **Setup Overview** loads. This
checks the bundled WKWebView page and its response to user input. No app test
hook, beta flag, mock IPC, permission bypass, or alternate build feature is used.
On failure, XCTest results, logs, and the accessibility tree are saved as a CI artifact.

Hosted runners have no audio devices or microphone/screen-recording permission.
The native check stops before model downloads and permission requests. It does
not test a full native recording session, import, audio playback, real model
execution, or device changes. Colors, spacing, and other visual judgment stay
manual. The browser session cannot replace those native checks.

Run browser checks from `frontend`:

```sh
npx pnpm@9.15.9 install --frozen-lockfile
npx pnpm@9.15.9 build
npx pnpm@9.15.9 exec playwright install chromium webkit
npx pnpm@9.15.9 test:release
```

The native shell script runs only on a disposable GitHub Actions arm64 runner.
It requires a fresh app data directory. It does not delete existing app data.
The XCTest project can be compiled locally with `xcodegen` and
`xcodebuild build-for-testing`; local native execution is not required.
