#!/usr/bin/env bash
# Build recall-mic and deploy it to every phone in one shot. Run from android/:
#
#   nix develop ..#android --command ./deploy.sh
#
# The android devshell provides Gradle and the SDK's adb. `install -r` keeps each
# app's data (host/port config survives); we relaunch the activity afterwards
# because a reinstall stops the foreground service, so this restarts streaming
# with no manual taps.
#
# Phones are at reserved IPs (DHCP reservations on the router) and a fixed adb
# port (5555, set once via `adb tcpip 5555`). Add a phone by appending to the list.
set -euo pipefail
cd "$(dirname "$0")"

PHONES=(
  192.168.1.133:5555   # Pixel 9 -> source 'pixel9'  (living room)
  192.168.1.242:5555   # Pixel 5 -> source 'pixel5'
)

ADB="$ANDROID_HOME/platform-tools/adb"

echo "building APK..."
./gradlew assembleDebug -q
APK="$PWD/app/build/outputs/apk/debug/app-debug.apk"

ok=0
for p in "${PHONES[@]}"; do
  echo "=== $p ==="
  if ! "$ADB" connect "$p" 2>&1 | grep -qiE "connected|already"; then
    echo "  UNREACHABLE — skipped (re-enable wireless debugging / re-run tcpip 5555)"
    continue
  fi
  "$ADB" -s "$p" install -r "$APK"
  # -S force-stops first so the activity is recreated (onCreate -> resume the
  # service with the new build), rather than just fronting a stale task.
  "$ADB" -s "$p" shell am start -S -n org.recall.mic/.MainActivity >/dev/null
  echo "  installed + relaunched"
  ok=$((ok + 1))
done
echo "deployed to $ok/${#PHONES[@]} phone(s)."
