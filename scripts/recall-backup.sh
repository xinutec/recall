#!/usr/bin/env bash
# Nightly off-machine mirror of the recall archive (launchd agent, 23:30 local —
# clear of odin's own restic window at ~02:44 UTC).
#
# The archive is the ONE unrecoverable thing in recall: transcripts are derived
# views, but the raw audio + the human corrections exist only on this volume.
# This pushes them to odin (the Mac is a one-way WireGuard peer, so the server
# cannot pull — the Mac must initiate).
#
# Two consistency rules:
#  1. The live SQLite file is never copied directly (a mid-write copy is garbage).
#     A consistent snapshot is taken with sqlite3 .backup first and shipped as
#     the mirror's recall.sqlite.
#  2. rsync runs WITHOUT --delete: the remote is a superset. A local catastrophe
#     (or a bad script edit) can propagate no deletions. Segment files are
#     immutable, so the superset never diverges in content, it only keeps
#     compress_to_opus's replaced originals around remotely.
#
# On success the marker .last-backup-ok in the data root is touched — `recall
# doctor` alarms when it is missing or older than 48h.
set -euo pipefail

ROOT=/Volumes/Backup/recall
DEST=root@odin:/backup/recall-mirror
STAGING=/Volumes/Backup/recall-backup-staging
SSH="ssh -o BatchMode=yes -o ConnectTimeout=15"

# Consistent DB snapshot (the backup API copes with concurrent agent writes).
mkdir -p "$STAGING"
trap 'rm -rf "$STAGING"' EXIT
/usr/bin/sqlite3 "$ROOT/recall.sqlite" ".timeout 30000" ".backup '$STAGING/recall.sqlite'"

# Audio + everything else, excluding the live DB (snapshot replaces it), the
# refine scratch dir, and the backup marker itself (it must only ever reflect
# LOCAL success — a restored mirror must not carry a fresh-looking marker).
/usr/bin/rsync -a -e "$SSH" \
    --exclude 'recall.sqlite*' \
    --exclude 'work/' \
    --exclude '.last-backup-ok' \
    "$ROOT/" "$DEST/"

/usr/bin/rsync -a -e "$SSH" "$STAGING/recall.sqlite" "$DEST/recall.sqlite"

touch "$ROOT/.last-backup-ok"
echo "recall-backup: mirrored $(du -sh "$ROOT" | cut -f1) to $DEST at $(date '+%Y-%m-%d %H:%M')"
