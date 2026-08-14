#!/usr/bin/env bash
# Build recall-mic and deploy it to every phone in one shot. Run from android/:
#
#   nix develop ..#android --command ./deploy.sh
#
# The android devshell provides Gradle and the SDK's adb. `pm install -r` keeps
# each app's data (host/port config survives); we relaunch the activity afterwards
# because a reinstall stops the foreground service, so this restarts streaming
# with no manual taps.
#
# Each phone is tried at its LAN address first (a DHCP reservation on the router)
# and then at its WireGuard address. The VPN one is not a fallback for a broken
# router — it is how you reach a phone that is genuinely OUT OF THE HOUSE, which
# on 2026-08-14 was the only way the Pixel 9 could be deployed to at all. Both use
# the fixed adb port 5555, set once per phone via `adb tcpip 5555`; that does NOT
# survive a reboot, and no address helps once it is off.
set -euo pipefail
cd "$(dirname "$0")"

# name | LAN | VPN (nixos-config/network.nix) | adb serial (for the mDNS fallback)
PHONES=(
  "pixel9|192.168.1.253:5555|10.100.0.12:5555|4C070DLAQ001L1"   # living room, but carried
  "pixel5|192.168.1.242:5555|10.100.0.10:5555|15271FDD40043S"
)

ADB="$ANDROID_HOME/platform-tools/adb"

echo "building APK..."
./gradlew assembleDebug -q
APK="$PWD/app/build/outputs/apk/debug/app-debug.apk"
LOCAL_MD5=$(md5 -q "$APK")

# ⚠ If EVERY address below reports unreachable while the ports are demonstrably
# open (`nc -z <ip> 5555` succeeds), the LOCAL adb server is wedged from earlier
# failed connects rather than the phones being away — measured 2026-08-14, `nc`
# succeeded 3/3 while `adb connect` timed out, and a restarted server connected in
# 0.3 s. The remedy is `adb kill-server && adb start-server`, BY HAND.
#
# Deliberately not done here: `kill-server` itself hangs indefinitely when a
# server is holding a transport to an offline device, and there is no `timeout` in
# this devshell to bound it — so a script that opens with it can wedge before it
# has printed a word, which is exactly what this one did on its first run. A stale
# entry for one address is cleared with `disconnect`, which cannot block.

# Where Wireless debugging is listening right now, via mDNS. Echoes host:port, or nothing.
#
# `adb tcpip 5555` does NOT survive a reboot, and what comes back after one is the
# Settings > Wireless debugging toggle — which listens on a RANDOM high port, not 5555.
# So a phone that is awake, on the LAN and pingable can still refuse :5555 on every
# address it owns, which reads exactly like a phone that is away (measured 2026-08-14:
# pixel5 pinged on both addresses with :5555 shut, while it was in fact listening on
# :39345). The port is advertised, so look it up rather than asking Pippijn to.
#
# ⚠ mDNS is link-local, so this only rescues a phone on the HOME LAN — a phone that is
# out of the house does not advertise here, and for it the VPN address is the only way in.
mdns_addr() {
  local serial=$1
  "$ADB" mdns services 2>/dev/null |
    awk -v s="$serial" '$1 ~ ("^adb-" s "-") && $2 == "_adb-tls-connect._tcp" {print $3; exit}'
}

# Reach a phone at whichever address answers. Echoes it, or nothing.
reach() {
  local addr
  for addr in "$@"; do
    [ -n "$addr" ] || continue
    "$ADB" disconnect "$addr" >/dev/null 2>&1 || true
    if "$ADB" connect "$addr" 2>&1 | grep -qiE "connected|already"; then
      # `connect` can report success and then sit `offline`, which every later
      # command fails on with a message about the device rather than the link.
      sleep 1
      if [ "$("$ADB" devices | awk -v a="$addr" '$1 == a {print $2}')" = "device" ]; then
        echo "$addr"
        return 0
      fi
      "$ADB" disconnect "$addr" >/dev/null 2>&1 || true
    fi
  done
  return 1
}

# Push, verify, then install FROM THE PHONE. Over the VPN a phone on cellular
# drops mid-transfer (`failed to read copy response`, then `device offline`), and
# a bare `adb install` makes that one flaky link carry both the 15 MB copy and the
# install. Split, and a drop costs a retry instead of the attempt. The old app
# survives a failed transfer untouched — measured: same versionName, same
# ServiceRecord, the foreground service never even restarted.
stage_and_install() {
  local p=$1 staged=/data/local/tmp/recall-mic.apk try
  for try in 1 2 3; do
    "$ADB" -s "$p" push "$APK" "$staged" >/dev/null 2>&1 || { sleep 5; "$ADB" connect "$p" >/dev/null 2>&1; continue; }
    if [ "$("$ADB" -s "$p" shell md5sum "$staged" 2>/dev/null | awk '{print $1}')" = "$LOCAL_MD5" ]; then
      "$ADB" -s "$p" shell pm install -r "$staged"
      "$ADB" -s "$p" shell rm -f "$staged"
      return 0
    fi
    echo "  push $try/3 did not verify — retrying"
    sleep 5
    "$ADB" connect "$p" >/dev/null 2>&1 || true
  done
  return 1
}

ok=0
for entry in "${PHONES[@]}"; do
  IFS='|' read -r name lan vpn serial <<<"$entry"
  echo "=== $name ==="
  if ! p=$(reach "$lan" "$vpn"); then
    p=$(reach "$(mdns_addr "$serial")") || true
    if [ -z "$p" ]; then
      echo "  UNREACHABLE on $lan or $vpn, and not advertising over mDNS — skipped"
      echo "  (turn on Settings > Developer options > Wireless debugging)"
      continue
    fi
    # Put :5555 back, so the next run finds it at the address this file documents
    # instead of rediscovering a port that changes on every reboot.
    echo "  reached at $p over mDNS — restoring :5555"
    "$ADB" -s "$p" tcpip 5555 >/dev/null 2>&1 || true
    sleep 3
    p=$(reach "$lan" "$vpn") || {
      echo "  :5555 did not come back — skipped"
      continue
    }
  fi
  echo "  reached at $p"
  if ! stage_and_install "$p"; then
    echo "  INSTALL FAILED — the previous build is still installed and running"
    continue
  fi
  # -S force-stops first so the activity is recreated (onCreate -> resume the
  # service with the new build), rather than just fronting a stale task.
  "$ADB" -s "$p" shell am start -S -n org.recall.mic/.MainActivity >/dev/null
  echo "  installed + relaunched"
  ok=$((ok + 1))
done
echo "deployed to $ok/${#PHONES[@]} phone(s)."
