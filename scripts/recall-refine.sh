#!/usr/bin/env bash
# Idle diarization-refinement daemon for the launchd agent. `recall refine` only
# diarizes while capture is *paused* (an idle window, e.g. overnight), so the heavy
# pyannote pass never competes with live capture. Uses recall.sh, which sources
# .env (HF_TOKEN — diarization is gated) plus the Nix tools + persistent venv.
set -euo pipefail

# Household LoRA adapter (adapter-current -> adapter-20260708b), deployed after it
# WON the whole-segment A/B gate on real audio (ab-compare, 2026-07-08 usb window:
# 0/74 garbling, mean WER 0.125 -> 0.064, wins 18 / trivial losses 6). Auto-detected
# as an adapter dir (adapter_config.json) and loaded on top of --base-model. Runs on
# the idle refine pass only, never live capture (turbo stays live). To roll back,
# drop the --model/--base-model args; to advance, repoint the adapter-current symlink
# after a fresh A/B win.
exec /Users/pippijn/Code/recall/scripts/recall.sh refine --out /Volumes/Backup/recall \
  --model /Volumes/Backup/recall/adapter-current \
  --base-model openai/whisper-large-v3
