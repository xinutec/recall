#!/usr/bin/env bash
# Run the iOS unit tests (RecallMicTests) on the iOS Simulator.
#
# Not in the gate (it needs Xcode and a simulator, and takes a minute); the gate
# only lints the Swift. Run it after changing ios/Sources.
#
# One-time setup already done on this Mac: `xcodebuild -downloadPlatform iOS` and
# `xcrun simctl create recall-test "iPhone 16" com.apple.CoreSimulator.SimRuntime.iOS-26-5`.
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/ios"

# The project is generated from project.yml, gitignored, and lists every source
# file, so a stale one misses new files (#1381). Regenerating takes a second.
nix run nixpkgs#xcodegen -- generate --quiet

# A Nix devshell points DEVELOPER_DIR at its own SDK; xcodebuild needs Xcode's.
env -u DEVELOPER_DIR xcodebuild test \
    -project RecallMic.xcodeproj \
    -scheme RecallMic \
    -destination 'platform=iOS Simulator,name=recall-test' \
    -derivedDataPath build \
    -quiet 2>&1 | grep -vE "^$" | tail -5
