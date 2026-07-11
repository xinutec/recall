#!/usr/bin/env bash
# Nightly off-machine mirror of the recall archive (launchd agent, 23:30 local —
# clear of odin's own restic window at ~02:44 UTC).
#
# The archive is the ONE unrecoverable thing in recall: transcripts are derived
# views, but the raw audio + the human corrections exist only on this volume.
# This pushes them to odin (the Mac is a one-way WireGuard peer, so the server
# cannot pull — the Mac must initiate).
#
# Runs as `recall backup`, NOT bare shell tools. On macOS the external archive
# volume is TCC-protected and the grant is attached to the recall python process
# (the other agents reach the volume through it). A launchd script calling
# /usr/bin/rsync directly has no grant, so after a volume remount reset TCC the old
# shell agent was denied the volume and silently stopped mirroring (2026-07-02..11).
# recall.sh runs the command in the granted context; its child rsync inherits it.
# The consistency rules (snapshot the DB, rsync without --delete) live in
# recall/backup.py. On success it touches .last-backup-ok — `recall doctor` alarms
# when that marker is missing or older than 48h.
set -euo pipefail

exec /Users/pippijn/Code/recall/scripts/recall.sh backup \
  --out /Volumes/Backup/recall \
  --dest root@odin:/backup/recall-mirror
