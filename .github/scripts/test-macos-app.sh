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
xcodebuild test \
  -project "$RUNNER_TEMP/meetily-launch/MeetilyLaunch.xcodeproj" \
  -scheme MeetilyLaunch \
  -destination 'platform=macOS,arch=arm64' \
  -derivedDataPath "$RUNNER_TEMP/meetily-launch/build" \
  -resultBundlePath "$RUNNER_TEMP/meetily-launch/Launch.xcresult" \
  -parallel-testing-enabled NO
