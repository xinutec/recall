#!/usr/bin/env bash
# Is recall working? — every 5 minutes, reported to fleetwatch (launchd agent).
#
# The check that was missing when it mattered. On 22 June capture crash-looped for two
# hours and recorded nothing; it was found three weeks later, by hand. launchd restarts
# capture when it dies (KeepAlive), so a persistent fault becomes a loop — and a loop
# looks, from the outside, exactly like a quiet house.
#
# Runs via recall.sh (the granted python context): the archive volume is TCC-protected
# and a bare shell tool reading it is denied after a remount, which is how the nightly
# backup silently stopped for nine days.
#
# `--post` sends the verdicts to fleetwatch, so they appear beside the rest of the
# fleet's health. Crucially, fleetwatch treats a producer that has STOPPED reporting as
# a failure, not a silence: this agent not running is itself the alarm.
#
# The cadence here must match recall.fleetwatch.INTERVAL_S, or a healthy producer gets
# reported as overdue.
set -euo pipefail

exec /Users/pippijn/Code/recall/scripts/recall.sh doctor \
  --out /Volumes/Backup/recall \
  --post
