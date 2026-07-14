#!/usr/bin/env bash
# Push the local archive to the fleet's system of record (Isis split) for the launchd
# agent. Each run sends only what changed since the last pass (a transcript-id
# watermark), then exits — the launchd timer drives the cadence. Uses recall.sh, which
# sources .env for RECALL_SYNC_TOKEN (the bearer the fleet checks); the command is inert
# and exits non-zero if the token is unset, so a stock LAN-only Mac is untouched.
#
# --url is Isis's WireGuard address: the WG-bound hostPort is the only thing that answers
# there, and the Mac reaches it as a one-way peer (it dials in; nothing dials back).
set -euo pipefail
exec /Users/pippijn/Code/recall/scripts/recall.sh sync \
  --url http://10.100.0.2:8000 --out /Volumes/Backup/recall
