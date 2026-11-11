#!/usr/bin/env bash
# Run the iOS unit tests (RecallMicTests) on the iOS Simulator.
#
# A gate row, outside the devshell. Needs the Mac's developer mode
# (`DevToolsSecurity -status`): without it a FAILING test hangs on XCTest's
# refused request to inspect the test process.
#
# One-time setup already done on this Mac: `xcodebuild -downloadPlatform iOS` and
# `xcrun simctl create recall-test "iPhone 16" com.apple.CoreSimulator.SimRuntime.iOS-26-5`.
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/ios"

# The project is generated from project.yml, gitignored, and lists every source
# file, so a stale one misses new files (#1381). Regenerating takes a second.
# From the flake's locked nixpkgs: the registry's moves, and a moved one
# builds the Swift toolchain from source.
nix run --inputs-from 'git+file:..?ref=HEAD' nixpkgs#xcodegen -- generate --quiet

# A Nix devshell points DEVELOPER_DIR at its own SDK, and LD at its own linker,
# which xcodebuild takes as its LD build setting; it needs Xcode's for both.
env -u DEVELOPER_DIR -u LD xcodebuild test \
    -project RecallMic.xcodeproj \
    -scheme RecallMic \
    -destination 'platform=iOS Simulator,name=recall-test' \
    -derivedDataPath build \
    -quiet 2>&1 | grep -vE "^$" | tail -5
