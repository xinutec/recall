#!/usr/bin/env bash
# Build recall-mic and deploy it to every phone in one shot. Run from android/:
#
#   nix develop ..#android --command ./deploy.sh
#
# `pm install -r` keeps each app's settings; the activity is relaunched after,
# since a reinstall stops the foreground service.
#
# Each phone is tried at its LAN address (a DHCP reservation), then its WireGuard
# address, which reaches it away from home. Both use adb port 5555, set per phone
# with `adb tcpip 5555`, which a reboot undoes.
set -euo pipefail
cd "$(dirname "$0")"

# name | LAN | VPN (nixos-config/network.nix) | adb serial (for the mDNS fallback)
PHONES=(
  "pixel9|192.168.1.253:5555|10.100.0.12:5555|4C070DLAQ001L1"   # living room, but carried
  "pixel5|192.168.1.242:5555|10.100.0.10:5555|15271FDD40043S"
  # LineageOS: Wireless debugging only (a random port, TLS-paired with this Mac),
  # so 5555 refuses and the mDNS lookup finds the port.
  "oneplus6t|192.168.1.28:5555|10.100.0.8:5555|83bf636e"
)

ADB="$ANDROID_HOME/platform-tools/adb"

echo "building APK..."
./gradlew assembleDebug -q
APK="$PWD/app/build/outputs/apk/debug/app-debug.apk"
LOCAL_MD5=$(md5 -q "$APK")

# If every address is unreachable while `nc -z <ip> 5555` succeeds, the local adb
# server is wedged; restart it by hand: `adb kill-server && adb start-server`. Not
# done here: `kill-server` can hang, and this devshell has no `timeout`.

# Where Wireless debugging listens now, by mDNS: host:port, or nothing. After a
# reboot only Wireless debugging is on, on a random port, so a phone at home can
# refuse :5555 everywhere. mDNS only works on the home LAN.
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
      # `connect` can succeed and then sit `offline`.
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

# Push, verify, then install on the phone, so a link that drops mid-copy (a phone on
# cellular over the VPN) costs a retry, not the install. A failed transfer leaves the
# old app running untouched.
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
    # Put :5555 back for next time.
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
  # -S force-stops first, so onCreate runs and restarts the service on the new
  # build.
  "$ADB" -s "$p" shell am start -S -n org.recall.mic/.MainActivity >/dev/null
  echo "  installed + relaunched"
  ok=$((ok + 1))
done
echo "deployed to $ok/${#PHONES[@]} phone(s)."
