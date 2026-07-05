#!/usr/bin/env bash
# Continuous capture wrapper for the launchd agent. Sources the Nix profile
# (launchd agents start with a minimal PATH), then records the USB mic to the
# encrypted Backup volume via the project's Nix devshell (sox + ffmpeg + recall).
set -euo pipefail

# shellcheck disable=SC1091
source /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh

cd /Users/pippijn/Code/recall
# --device pins the exact CoreAudio input: the system *default* input follows
# whatever connects (a Bluetooth speaker's hands-free mic once hijacked it,
# chiming into call mode and recording at telephone quality).
exec nix develop --command \
  python -m recall record --out /Volumes/Backup/recall --id usb \
  --device "USB Condenser Microphone"
