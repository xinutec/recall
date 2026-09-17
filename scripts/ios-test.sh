#!/usr/bin/env bash
# Run the iOS unit tests (RecallMicTests) on the iOS Simulator.
#
# Not part of the gate: it needs full Xcode + the iOS simulator runtime and takes
# ~1 min (simulator boot); run it after touching ios/Sources. The gate still
# lint-checks the Swift on every run (swift-format).
#
# One-time setup already done on this Mac: `xcodebuild -downloadPlatform iOS` and
# `xcrun simctl create recall-test "iPhone 16" com.apple.CoreSimulator.SimRuntime.iOS-26-5`.
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/ios"

# The Xcode project is GENERATED from project.yml and gitignored, and it lists
# every source file by name — so a fresh clone has no project at all, and a
# clone with a stale one cannot see a Swift file added since (#1381: two files
# lived only in one machine's hand-edited project). Regenerating is idempotent
# (byte-identical when nothing changed) and takes a second, so do it every run.
nix run nixpkgs#xcodegen -- generate --quiet

# xcrun/xcodebuild must resolve the REAL Xcode toolchain; a Nix devshell retargets
# DEVELOPER_DIR to its own SDK, so clear it (same dance as the gate's swift-format row).
env -u DEVELOPER_DIR xcodebuild test \
    -project RecallMic.xcodeproj \
    -scheme RecallMic \
    -destination 'platform=iOS Simulator,name=recall-test' \
    -derivedDataPath build \
    -quiet 2>&1 | grep -vE "^$" | tail -5
