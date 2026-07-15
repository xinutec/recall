#!/usr/bin/env bash
# Run on-demand ML the fleet requested but can't do itself (the Isis split), for the
# launchd agent. Each run pulls Isis's refine queue into this Mac's local queue and exits
# — the launchd timer drives the cadence; the refine daemon then does the ML while the mic
# is idle, and the refined turns sync back on their own. Uses recall.sh, which sources
# .env for RECALL_SYNC_TOKEN (the bearer the fleet checks); the command is inert and exits
# non-zero if the token is unset, so a stock LAN-only Mac is untouched.
#
# --url is Isis's WireGuard address: the WG-bound hostPort is the only thing that answers
# there, and the Mac reaches it as a one-way peer (it dials in; nothing dials back).
set -euo pipefail
exec /Users/pippijn/Code/recall/scripts/recall.sh jobs \
  --url http://10.100.0.2:8000 --out /Volumes/Backup/recall
