#!/usr/bin/env bash
# Single-port audio ingest for a launchd agent. Listens on ONE TCP port for every
# recall-mic phone (android/): each connection opens with a handshake announcing its
# device id, and the server segments its raw s16le PCM to the Backup volume under
# that id — the same segment files the USB mic produces. Replaces the old per-phone
# per-port listeners; the device id, not the port, is the identity now, so a new
# phone needs no new agent/port — it just connects.
set -euo pipefail

# shellcheck disable=SC1091
source /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh

cd /Users/pippijn/Code/recall
# Port is the hardcoded DEFAULT_INGEST_PORT (matches the app); no --port needed.
exec nix develop --command \
  python -m recall ingest --out /Volumes/Backup/recall
