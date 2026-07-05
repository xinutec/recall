#!/usr/bin/env bash
# Run the iOS unit tests (RecallMicTests) on the iOS Simulator.
#
# Not part of verify.sh: it needs full Xcode + the iOS simulator runtime and takes
# ~1 min (simulator boot); run it after touching ios/Sources. verify.sh still
# lint-checks the Swift on every run (swift-format).
#
# One-time setup already done on this Mac: `xcodebuild -downloadPlatform iOS` and
# `xcrun simctl create recall-test "iPhone 16" com.apple.CoreSimulator.SimRuntime.iOS-26-5`.
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/ios"

# xcrun/xcodebuild must resolve the REAL Xcode toolchain; a Nix devshell retargets
# DEVELOPER_DIR to its own SDK, so clear it (same dance as verify.sh's swift-format).
env -u DEVELOPER_DIR xcodebuild test \
    -project RecallMic.xcodeproj \
    -scheme RecallMic \
    -destination 'platform=iOS Simulator,name=recall-test' \
    -derivedDataPath build \
    -quiet 2>&1 | grep -vE "^$" | tail -5
