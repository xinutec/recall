#!/usr/bin/env bash
# Idle diarization-refinement daemon for the launchd agent. `recall refine` only
# diarizes while capture is *paused* (an idle window, e.g. overnight), so the heavy
# pyannote pass never competes with live capture. Uses recall.sh, which sources
# .env (HF_TOKEN — diarization is gated) plus the Nix tools + persistent venv.
set -euo pipefail

# Refine transcribes with the same mlx large-v3-turbo as the live/worker path — its
# precision comes from the diarization + word-level speaker alignment, not the ASR model.
# The household LoRA adapter (adapter-current -> adapter-20260708b) was tried here for
# extra word accuracy, but on long recordings it is ~8x slower (full fp32 large-v3, a
# 32-layer decoder vs turbo's 4) for a WER win (2026-07-08 A/B: 0.125 -> 0.064) that was
# only ever measured on short clips — so refine stays on turbo. To re-enable the adapter,
# add back these args (it's auto-detected as an adapter dir via adapter_config.json and
# loaded on top of --base-model):
#   --model /Volumes/Backup/recall/adapter-current --base-model openai/whisper-large-v3
exec /Users/pippijn/Code/recall/scripts/recall.sh refine --out /Volumes/Backup/recall
