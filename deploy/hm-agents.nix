# hm-agents.nix — home-manager module: recall launchd daemons (Mac mini).
#
# Apply after editing (it is a PINNED flake input — the lock must be bumped):
#   1. commit this change in ~/Code/recall
#   2. cd ~/.config/home-manager
#   3. nix flake update recall && home-manager switch --flake .#pippijn
#
# Imported by the personal home-manager flake (~/.config/home-manager), so
# `home-manager switch` installs, reloads and removes these agents declaratively.
#
# WHAT THE AGENTS RUN (changed 2026-07-22): a wrapper in the nix store, whose
# PYTHONPATH is the store copy of THIS commit — not `~/Code/recall/src`. The
# module was always pinned by the flake lock; the code it ran was not, so an
# uncommitted edit in the working tree became the running daemon at its next
# restart. Now the two move together, and `./scripts/recall.sh …` in the tree is
# purely a development entry point.
#
# HOW THEY RUN IT (changed 2026-08-01): a real package. The wrapper names the store
# paths of the interpreter, sox and ffmpeg directly instead of entering the devshell
# (`nix develop path:${src} --command …`), which used to put a full flake evaluation
# in every agent's startup path. Same flake.lock, so the same store paths — including
# the mic-TCC-bearing python — but no eval, and no dependency on nix being reachable
# at spawn.
#
# What deliberately did NOT change:
#   - the toolchain. sox/ffmpeg/python are still the versions recall's own flake.lock
#     pins and tests against — the same store paths the working tree resolves to,
#     which is what keeps the mic-TCC identity stable. `flake.nix` defines the
#     interpreter once (`packages.dev-python`) and the devshell uses that same
#     derivation, so the two cannot drift.
#   - the interpreter split. capture/ingest run the DEVSHELL python (no ML deps —
#     the gate checks that their import surface stays ML-free); everything else runs
#     the python that holds mlx/pyannote/torch. What DID change (2026-07-31): that
#     second interpreter is now the uv2nix store env (`nix build .#ml-env`), not the
#     working tree's `.venv`, so nothing an agent imports lives in $HOME any more.
#   - `.env` (HF_TOKEN, RECALL_SYNC_TOKEN) is still read at runtime from
#     ~/Code/recall/.env. Secrets must never enter the store.
#
# Logs live in ~/Library/Logs/recall, NOT in the repo: launchd opens the stdio
# paths before any code runs, so a log path inside a checkout that moves takes the
# agent down with exit 78 and an empty log — the failure that hid 470 crash-loops
# for weeks. `recall.cli._LOG_DIR` points at the same place for rotation.
#
# NOTE: recall-capture opens the microphone. recall-live does NOT — it reads the UDP
# tap capture publishes, because two CoreAudio clients on one device starve each other
# (sources.live_input_argv); its `--device` argument is vestigial.
#
# home-manager writes each plist read-only into ~/Library/LaunchAgents with no
# native comment, so a provenance `Comment` key points back here. Do NOT
# hand-edit the generated plists.
{ pkgs, lib, recall, ... }:

let
  # The store copy of this commit: what the flake lock pins, and what the agents
  # import. Interpolating it into a wrapper also makes it a runtime dependency,
  # so it is GC-rooted by the home-manager generation.
  src = ../.;

  out = "/Volumes/Backup/recall";
  fleet = "http://10.100.0.2:8000";
  logs = "/Users/pippijn/Library/Logs/recall";

  # The ML stack (mlx-whisper, pyannote, torch) as a STORE PATH, built from uv.lock's
  # wheels by uv2nix (`nix build .#ml-env`) — no longer the working tree's uv venv.
  # It moves with the flake lock, so what these agents import is pinned by the same
  # commit as the code they run, and a `uv sync` in the tree can no longer change a
  # running daemon.
  #
  # Safe to move because NO agent on this interpreter opens the microphone: capture
  # owns the device and live consumes its UDP tap (sources.live_input_argv), so the
  # mic-TCC identity — the devshell python that capture and ingest run — is untouched.
  # The grant these need is /Volumes/Backup, re-established once for the new binary.
  venvPython = "${recall.packages.${pkgs.stdenv.hostPlatform.system}.ml-env}/bin/python";

  # The non-ML interpreter capture and ingest run. The SAME derivation the devshell
  # uses (flake.nix defines it once), so the store path — and with it the microphone
  # grant macOS attributes to that binary — is unchanged by this packaging.
  devPython = "${recall.packages.${pkgs.stdenv.hostPlatform.system}.dev-python}/bin/python";

  # One store wrapper per agent. `python` selects the interpreter; everything else
  # is identical, so the arguments below are the single source of truth for what
  # each daemon does (the old scripts/recall-*.sh wrappers duplicated them).
  #
  # A real package, not a devshell entry (changed 2026-08-01). Each wrapper used to
  # `exec nix develop path:${src} --command …`, which put a full flake evaluation in
  # every agent's startup path — including capture's. That was never free, and on
  # 2026-07-17/18 it was catastrophic: with nix's cache on the USB volume, evals went
  # from 15s to over 30 minutes machine-wide for nine hours. The devshell was only
  # ever there for three things — the interpreter, sox and ffmpeg — and all three are
  # store paths this can name directly, from the same flake.lock the devshell resolves
  # against. `runtimeInputs` PREPENDS to PATH, so `say` and `launchctl` still come
  # from the system paths launchd provides.
  # Where the Hugging Face models live, DECLARED rather than symlinked.
  #
  # This was `~/.cache/huggingface` -> here, a symlink nothing in this repo knew
  # about: the agents inherited it by accident of the filesystem, so the one
  # thing that decided where tens of gigabytes of models lived was invisible to
  # every reader of this module (memview #645). Config states it; a symlink only
  # implies it.
  #
  # ⚠ **The path is on the external volume ON PURPOSE**, and it moved hardware on
  # 2026-08-12: it was the 6 TB HDD, it is now the 2 TB SSD that took the name
  # `/Volumes/Backup`. Nothing here changed because the NAME did not — which is
  # exactly why the volume was renamed rather than the paths rewritten.
  #
  # The `cache/cache` doubling is a fossil of the era when `~/.cache` itself was
  # a symlink to `/Volumes/Backup/cache`. Kept because tidying it means moving
  # the models, and the point of this change is to stop the location being an
  # accident — not to pick a new one.
  hfHome = "/Volumes/Backup/cache/cache/huggingface";

  wrapper = { name, python, args }:
    pkgs.writeShellApplication {
      name = "recall-${name}";
      # sox captures the mic (CoreAudio, sample-perfect); ffmpeg segments and encodes,
      # and ffprobe reads durations. All are invoked by bare name. From RECALL's flake,
      # not `pkgs.sox`: this module is evaluated by home-manager against its own
      # nixpkgs, so naming them here would hand the agents binaries that no run of
      # recall's own test suite has ever seen.
      runtimeInputs = [ recall.packages.${pkgs.stdenv.hostPlatform.system}.agent-tools ];
      text = ''
        ENV_FILE="''${RECALL_ENV:-$HOME/Code/recall/.env}"
        if [ -r "$ENV_FILE" ]; then
          set -a
          # shellcheck disable=SC1090  # a runtime path, deliberately not a fixed file
          . "$ENV_FILE"
          set +a
        fi

        exec env PYTHONPATH=${src}/src HF_HOME=${hfHome} ${python} -m recall ${lib.escapeShellArgs args}
      '';
    };

  # A KeepAlive recall daemon at background priority. `extra` adds per-agent keys.
  daemon = { label, name, python ? devPython, args, extra ? { } }:
    let program = wrapper { inherit name python args; };
    in {
      enable = true;
      config = {
        Label = label;
        Comment =
          "GENERATED by home-manager from recall/deploy/hm-agents.nix. Do NOT edit "
          + "this file. To change: edit that module + commit, then in "
          + "~/.config/home-manager run 'nix flake update recall && home-manager "
          + "switch --flake .#pippijn'. Runs: recall "
          + builtins.concatStringsSep " " args + ".";
        ProgramArguments = [ "${program}/bin/recall-${name}" ];
        RunAtLoad = true;
        KeepAlive = true;
        ProcessType = "Background";
        StandardOutPath = "${logs}/${name}.out.log";
        StandardErrorPath = "${logs}/${name}.err.log";
      } // extra;
    };
in
{
  # launchd cannot create this: it opens the stdio paths at spawn, and a missing
  # parent directory is exit 78 with nothing written anywhere to say so.
  home.file."Library/Logs/recall/.keep".text = "";

  # NO recall-api here — the Mac serves no UI or control plane (the Isis split). Isis
  # (10.100.0.2:8000) is the system of record and the only web UI / control surface; the
  # Mac is capture + all MLX + push (recall-sync) + the protected master archive. Browsers
  # and the phone web app point at Isis; pause/resume is mirrored down by recall-capture-
  # mirror. Interactive MLX endpoints (refine, ab-compare, /api/sessions upload) are NOT
  # reachable from Isis under the one-way WireGuard model and need a Mac-initiated job-pull
  # (like capture-mirror) — tracked as Phase 2, not served from the Mac.

  # Single-port audio ingest for the phone mics. Devshell python: this path must
  # stay free of ML imports (it is one of the two agents the gate checks for that).
  launchd.agents."org.xinutec.recall-ingest" = daemon {
    label = "org.xinutec.recall-ingest";
    name = "ingest";
    args = [ "ingest" "--out" out ];
  };

  launchd.agents."org.xinutec.recall-worker" = daemon {
    label = "org.xinutec.recall-worker";
    name = "worker";
    python = venvPython;
    args = [ "worker" "--loop" "--basic" "--out" out ];
    # Heavy continuous loop — yield I/O and CPU to interactive work.
    extra = { LowPriorityIO = true; Nice = 10; };
  };

  # The one process on this Mac that holds the LLM weights (src/recall/llmhost.py).
  # recall's summaries/Ask and life's emotion worker are clients over 127.0.0.1:8092;
  # neither loads a model of its own, so the ~4.3 GB is paid once and released after
  # five idle minutes.
  #
  # ProcessType overrides the Background default the other daemons take: an Ask has a
  # human waiting on it, and the throttled I/O made the cold weight read visibly
  # slower than the same load from a shell (104s vs 62s, measured). Idle it costs a
  # few MB, so it competes with capture only while it is actually answering.
  launchd.agents."org.xinutec.recall-llm-host" = daemon {
    label = "org.xinutec.recall-llm-host";
    name = "llm-host";
    python = venvPython;
    args = [ "llm-host" ];
    extra = { ProcessType = "Standard"; };
  };

  # Idle diarization-refinement. `recall refine` only diarizes while capture is
  # *paused* (e.g. overnight), so the heavy pyannote pass never competes with live
  # capture. It also drains Ask jobs and day-summaries (via the llm-host).
  #
  # Refine transcribes with the same mlx large-v3-turbo as the live/worker path — its
  # precision comes from the diarization + word-level speaker alignment, not the ASR
  # model. The household LoRA adapter (adapter-current -> adapter-20260708b) was tried
  # here for extra word accuracy, but on long recordings it is ~8x slower (full fp32
  # large-v3, a 32-layer decoder vs turbo's 4) for a WER win (2026-07-08 A/B:
  # 0.125 -> 0.064) that was only ever measured on short clips — so refine stays on
  # turbo. To re-enable the adapter, add back these args (it is auto-detected as an
  # adapter dir via adapter_config.json and loaded on top of --base-model):
  #   "--model" "/Volumes/Backup/recall/adapter-current"
  #   "--base-model" "openai/whisper-large-v3"
  launchd.agents."org.xinutec.recall-refine" = daemon {
    label = "org.xinutec.recall-refine";
    name = "refine";
    python = venvPython;
    args = [ "refine" "--out" out ];
  };

  # Mic agent. --device pins the exact CoreAudio input: the system default input
  # follows whatever connects, e.g. a Bluetooth speaker's hands-free mic.
  # --fleet-url pushes the instant feed to Isis on a background thread (the Isis split):
  # the fleet UI shows live turns within seconds, reconciled when the archive segment
  # lands. Token is RECALL_SYNC_TOKEN (from .env); the push is best-effort and off the
  # VAD loop, so it never affects capture.
  launchd.agents."org.xinutec.recall-live" = daemon {
    label = "org.xinutec.recall-live";
    name = "live";
    python = venvPython;
    args = [ "live" "--out" out "--device" "USB Condenser Microphone"
             "--fleet-url" fleet ];
  };

  # Mic agent — the critical continuous recording stream (USB mic → segments).
  # Devshell python (no ML deps): the one process that must never die. A renamed or
  # missing --device makes sox fail hard and the agent crash-loop, visibly, rather
  # than silently recording from the wrong mic.
  launchd.agents."org.xinutec.recall-capture" = daemon {
    label = "org.xinutec.recall-capture";
    name = "capture";
    args = [ "record" "--out" out "--id" "usb" "--device" "USB Condenser Microphone" ];
  };

  # NO recall-backup here — the off-machine backup is odin's job, not the Mac's.
  # odin's nightly restic takes an integrity-checked SQLite snapshot from inside the
  # Isis pod plus an audio rsync of the recall PVC (nixos-config
  # machines/odin/backup-prepare.sh), so every recording is already protected
  # server-to-server. The Mac used to push its whole archive here too — a pre-split
  # leftover from when the Mac was the system of record. Its only content Isis lacks
  # is the training corpora (finetune-corpus, pilot-*), which are derived from the
  # archive + corrections and are deliberately NOT backed up: they can be regenerated.
  # Retiring it also drops the /Volumes/Backup TCC fragility that broke it before.

  # Is recall actually working? Every 5 minutes, reported to fleetwatch.
  #
  # The check that was missing when it mattered: capture crash-looped on 22 June,
  # recorded nothing for ninety minutes, and was found three weeks later by hand.
  # launchd restarts capture when it dies, so a persistent fault becomes a loop —
  # and a loop looks exactly like a quiet house.
  #
  # The interval MUST match recall.fleetwatch.INTERVAL_S (300): fleetwatch derives
  # staleness from the cadence the report declares, and a producer that stops
  # reporting renders as failed. That is the point — this agent dying, or the Mac
  # dying, is itself the alarm. Nothing here has to detect it.
  launchd.agents."org.xinutec.recall-doctor" = daemon {
    label = "org.xinutec.recall-doctor";
    name = "doctor";
    python = venvPython;
    args = [ "doctor" "--out" out "--post" ];
    extra = {
      KeepAlive = false;
      RunAtLoad = true;
      StartInterval = 300;
      LowPriorityIO = true;
    };
  };

  # Push the archive to Isis, the system of record (the Isis split). A timer, not
  # KeepAlive: each run sends only what changed since the last (a transcript-id
  # watermark) and exits. The Mac must push — it is a one-way WireGuard peer the fleet
  # cannot reach. Inert until RECALL_SYNC_TOKEN is set in .env.
  launchd.agents."org.xinutec.recall-sync" = daemon {
    label = "org.xinutec.recall-sync";
    name = "sync";
    python = venvPython;
    args = [ "sync" "--url" fleet "--out" out ];
    extra = {
      KeepAlive = false;
      RunAtLoad = true;
      StartInterval = 120;
      LowPriorityIO = true;
      Nice = 10;
    };
  };

  # Run on-demand ML the fleet asked for but can't do (the Isis split). A timer, not
  # KeepAlive: each run pulls Isis's refine queue (a refine requested from its UI) into the
  # Mac's local queue and exits; the refine daemon then does the ML while the mic is idle,
  # and the refined turns sync back via recall-sync. The Mac must poll — it is a one-way
  # WireGuard peer the fleet cannot reach. Inert until RECALL_SYNC_TOKEN is set in .env.
  launchd.agents."org.xinutec.recall-jobs" = daemon {
    label = "org.xinutec.recall-jobs";
    name = "jobs";
    python = venvPython;
    args = [ "jobs" "--url" fleet "--out" out ];
    extra = {
      KeepAlive = false;
      RunAtLoad = true;
      StartInterval = 60;
      LowPriorityIO = true;
      Nice = 10;
    };
  };

  # Mirror Isis's mic pause/resume onto this Mac (the Isis split). A KeepAlive loop that
  # polls Isis every ~5s: Isis holds the desired capture state (its VPN UI) but cannot
  # dial this one-way peer, so control is inverted to a Mac-initiated poll and a pause
  # pressed on the VPN UI takes hold within seconds. Lightweight — an HTTP round trip,
  # no ML. Inert until RECALL_SYNC_TOKEN is set in .env.
  launchd.agents."org.xinutec.recall-capture-mirror" = daemon {
    label = "org.xinutec.recall-capture-mirror";
    name = "capture-mirror";
    python = venvPython;
    args = [ "capture-mirror" "--url" fleet "--out" out "--loop" "--interval" "5" ];
  };
}
