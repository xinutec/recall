#!/usr/bin/env bash
# Mirror the fleet's mic pause/resume onto this Mac (Isis split) for the launchd agent.
# Isis holds the desired capture state (its VPN-reachable UI) but runs no capture agent
# and can't dial this one-way WireGuard peer — so the Mac polls Isis every ~5s and applies
# pause/resume to its own capture, reporting back what it did. A KeepAlive loop: it stays
# resident and re-polls, so a pause pressed on Isis takes hold within seconds.
#
# Uses recall.sh (sources .env for RECALL_SYNC_TOKEN); the command exits non-zero if the
# token is unset, so KeepAlive would just respawn it — harmless on a stock LAN-only Mac.
set -euo pipefail
exec /Users/pippijn/Code/recall/scripts/recall.sh capture-mirror \
  --url http://10.100.0.2:8000 --out /Volumes/Backup/recall --loop --interval 5
