#!/usr/bin/env bash
set -euo pipefail
# Run only on a disposable hosted runner. Keep a developer's app data intact.
[[ "${GITHUB_ACTIONS:-}" == true && "$(uname -m)" == arm64 ]]
app="$RUNNER_TEMP/meetily-app/Meetily Branch.app"
test -d "$app"
test ! -e "$HOME/Library/Application Support/com.meetily.ai.branch"
codesign --verify --deep --strict "$app"
export MEETILY_APP_PATH="$app"
mkdir -p "$RUNNER_TEMP/meetily-launch"
cp frontend/e2e/native/{project.yml,LaunchTests.swift} "$RUNNER_TEMP/meetily-launch/"
xcodegen generate --spec "$RUNNER_TEMP/meetily-launch/project.yml"

# XCTest does not keep the app's stderr. After a failure, relaunch the app
# with info logs so the job shows where startup stopped.
show_app_log() {
  local log="$RUNNER_TEMP/meetily-launch/app.log"
  RUST_LOG=info "$app/Contents/MacOS/meetily" >"$log" 2>&1 &
  local pid=$!
  sleep 45
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  echo "::group::App log after a failed launch test"
  cat "$log"
  echo "::endgroup::"
}
trap 'status=$?; [[ $status -eq 0 ]] || show_app_log; exit $status' EXIT

xcodebuild test \
  -project "$RUNNER_TEMP/meetily-launch/MeetilyLaunch.xcodeproj" \
  -scheme MeetilyLaunch \
  -destination 'platform=macOS,arch=arm64' \
  -derivedDataPath "$RUNNER_TEMP/meetily-launch/build" \
  -resultBundlePath "$RUNNER_TEMP/meetily-launch/Launch.xcresult" \
  -parallel-testing-enabled NO
