#!/usr/bin/env bash
# Run any recall command with the full environment:
#   - Nix tools (sox, ffmpeg) on PATH
#   - the persistent .venv python (mlx-whisper, pyannote)
#   - the package on PYTHONPATH
#   - HF_TOKEN from .env (for pyannote, gated on HuggingFace)
#
# Usage: ./scripts/recall.sh transcribe --diarize --out /Volumes/Backup/recall
set -euo pipefail

# shellcheck disable=SC1091
source /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh

cd /Users/pippijn/Code/recall
if [ -f .env ]; then
  set -a
  # shellcheck disable=SC1091
  source .env
  set +a
fi

exec nix develop --command env PYTHONPATH=src .venv/bin/python -m recall "$@"
