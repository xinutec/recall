#!/usr/bin/env bash
# Idle diarization-refinement daemon for the launchd agent. `recall refine` only
# diarizes while capture is *paused* (an idle window, e.g. overnight), so the heavy
# pyannote pass never competes with live capture. Uses recall.sh, which sources
# .env (HF_TOKEN — diarization is gated) plus the Nix tools + persistent venv.
set -euo pipefail

# Base model, NOT the household LoRA adapter: the A/B comparison on real audio
# (ab-compare, 2026-06) showed the trained adapter REGRESSED against the base —
# deploying it here was premature. Re-point at an adapter (--model
# /Volumes/Backup/recall/adapter-current --base-model openai/whisper-large-v3)
# only after an A/B run shows it winning.
exec /Users/pippijn/Code/recall/scripts/recall.sh refine --out /Volumes/Backup/recall
