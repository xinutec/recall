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
  # recalld's ingest plane on the same host (docs/architecture.md, stage A).
  ingest = "http://10.100.0.2:8001";
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

  # The Rust audio-plane daemon (audiod/, docs/audio-plane.md). Its wrapper
  # sources no .env — the ingest path holds no secrets — but keeps agent-tools
  # on PATH: audiod spawns ffmpeg as its segmenter child, and it must be the
  # same ffmpeg the test suite and the Python agents run.
  audiodWrapper = { name, args }:
    pkgs.writeShellApplication {
      name = "recall-${name}";
      runtimeInputs = [ recall.packages.${pkgs.stdenv.hostPlatform.system}.agent-tools ];
      text = ''
        exec env RUST_LOG=info ${
          recall.packages.${pkgs.stdenv.hostPlatform.system}.audiod
        }/bin/audiod ${lib.escapeShellArgs args}
      '';
    };

  # The Rust health agent (doctor/). Sources .env, unlike audiodWrapper: the
  # presence of RECALL_SYNC_TOKEN is what tells the doctor this machine is half
  # of the Isis split, and so whether the fleet-mirror check applies at all. It
  # needs no agent-tools — the doctor spawns only ITSELF, as the bounded child
  # that reads the archive.
  doctorWrapper = { name, args }:
    pkgs.writeShellApplication {
      name = "recall-${name}";
      text = ''
        ENV_FILE="''${RECALL_ENV:-$HOME/Code/recall/.env}"
        if [ -r "$ENV_FILE" ]; then
          set -a
          # shellcheck disable=SC1090  # a runtime path, deliberately not a fixed file
          . "$ENV_FILE"
          set +a
        fi

        exec ${
          recall.packages.${pkgs.stdenv.hostPlatform.system}.audiod
        }/bin/doctor ${lib.escapeShellArgs args}
      '';
    };

  # The speech scanner (audiod speech). Needs TWO things on top of the plain
  # audiod wrapper, and both were learnt by the scanner failing without them:
  #
  #   ffmpeg  — audiocore's decode shells out to it, so without it EVERY segment
  #             "fails to decode". agent-tools carries it.
  #   ORT_DYLIB_PATH — ort dlopens the ONNX runtime by name, and macOS has no
  #             system libonnxruntime at all. Taken from RECALL's nixpkgs, which
  #             is the one the test suite runs silero through; naming the
  #             host's would hand the agent a runtime nothing here has tested.
  speechWrapper = { name, args }:
    pkgs.writeShellApplication {
      name = "recall-${name}";
      runtimeInputs = [ recall.packages.${pkgs.stdenv.hostPlatform.system}.agent-tools ];
      text = ''
        exec env RUST_LOG=info \
          ORT_DYLIB_PATH=${
            recall.packages.${pkgs.stdenv.hostPlatform.system}.onnxruntime
          }/lib/libonnxruntime${pkgs.stdenv.hostPlatform.extensions.sharedLibrary} \
          ${
            recall.packages.${pkgs.stdenv.hostPlatform.system}.audiod
          }/bin/audiod ${lib.escapeShellArgs args}
      '';
    };

  # A KeepAlive recall daemon at background priority. `extra` adds per-agent keys.
  # `program` overrides the python wrapper for agents that are not `recall <args>`.
  daemon = { label, name, python ? devPython, args, extra ? { }, program ? null }:
    let prog = if program != null then program else wrapper { inherit name python args; };
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
        ProgramArguments = [ "${prog}/bin/recall-${name}" ];
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

  # Single-port audio ingest for the phone mics — audiod (Rust) since 2026-09-04.
  # The Python server was deleted 2026-09-05; the rollback is git history.
  # Same reasoning as capture for the priority class: this holds the phones' live
  # PCM sockets and pumps them into ffmpeg in real time. A throttled reader drops
  # a phone's samples exactly as a throttled sox drops the USB mic's.
  launchd.agents."org.xinutec.recall-ingest" = daemon {
    label = "org.xinutec.recall-ingest";
    name = "ingest";
    args = [ ];
    program = audiodWrapper { name = "ingest"; args = [ "ingest" "--root" out ]; };
    extra = { ProcessType = "Interactive"; };
  };

  # LAN fallback for the mic heartbeat (#888). Independent of the capture agents ON
  # PURPOSE: a household pause closes the ingest listener, and a pause is exactly when
  # the heartbeat is the only signal there is — so a beat receiver that shared that
  # lifecycle would be shut precisely when it was needed. Devshell python, stdlib only,
  # so it stays inside the no-ML import surface the gate checks.
  launchd.agents."org.xinutec.recall-beat-relay" = daemon {
    label = "org.xinutec.recall-beat-relay";
    name = "beat-relay";
    args = [ "beat-relay" ];
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
  # ⚠ ProcessType overrides the Background default, and this is the agent that most
  # needs it: `Background` is macOS's THROTTLED class (reduced CPU share, deprioritised
  # I/O), and this process holds the always-on microphone. sox reads CoreAudio in real
  # time — starve it and its buffer overruns, samples are DROPPED, and the segment ring
  # stretches. That is silent, unrecoverable loss of household speech, which is the one
  # failure this system exists to prevent (#1330).
  #
  # Measured 2026-09-03, machine at load 42 (a Blender batch render at 565% CPU, other
  # sessions' builds, this repo's own gate): usb segment intervals went from a clean
  # 60.15 s mean in a quiet hour to 109.75 s with a 235 s worst case — roughly half the
  # wall clock unrecorded, while capture sat in the throttled class by configuration.
  # An earlier ablation had already cleared the transcription worker of causing it; the
  # cause was never a particular neighbour, it was that ANY load outranks the recorder.
  #
  # `Interactive` is the honest description: nothing on this machine is more
  # latency-critical than not missing what was said in the room.
  launchd.agents."org.xinutec.recall-capture" = daemon {
    label = "org.xinutec.recall-capture";
    name = "capture";
    args = [ ];
    program = audiodWrapper {
      name = "capture";
      # ⚠ No `--codec` here, and that is deliberate: lossless is audiod's
      # DEFAULT since 2026-09-11, so every recorder gets it without a flag.
      # Carrying it explicitly on this one agent read as "the condenser is
      # special", which is how the phones stayed Opus for a day after the
      # decision — `audiod ingest` (below) simply took the default nobody had
      # revisited. The reasoning lives with the default, in
      # audiod/src/segmenter.rs; the retention side is docs/architecture.md.
      args = [ "capture" "--root" out "--id" "usb" "--device" "USB Condenser Microphone" ];
    };
    extra = { ProcessType = "Interactive"; };
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
  # The interval MUST match the doctor's declared INTERVAL_S (300): fleetwatch
  # derives staleness from the cadence the report declares, and a producer that
  # stops reporting renders as failed. That is the point — this agent dying, or
  # the Mac dying, is itself the alarm. Nothing here has to detect it.
  #
  # ⚠ `KeepAlive = false` with a 300s interval is why the doctor reads the
  # archive in a child it can abandon: launchd starts no further run while one
  # is stuck, so a single wedged doctor silences every doctor after it.
  launchd.agents."org.xinutec.recall-doctor" = daemon {
    label = "org.xinutec.recall-doctor";
    name = "doctor";
    args = [ ];
    program = doctorWrapper {
      name = "doctor";
      args = [ "--out" out "--post" ];
    };
    extra = {
      KeepAlive = false;
      RunAtLoad = true;
      StartInterval = 300;
      LowPriorityIO = true;
    };
  };

  # How much of each archived segment is SPEECH — the evidence the quiet review
  # needs before it may propose deleting anything, and the thing that tells a
  # broken microphone from a quiet room (#1485 was found by its first pass).
  #
  # ⚠ It runs on the MAC, not against Isis, because deletion authority is
  # Mac-local (docs/architecture.md, "Deletion authority") and because the fleet
  # cannot answer for everything: 730 of 14,777 segments have never been
  # delivered, and those are the least replicated audio in the house.
  #
  # Bounded and low priority: this decodes audio on the machine that is also
  # recording, and delivery must never compete with the recorder (design.md §7).
  # ~0.5 s per segment measured, so 120 a pass is about a minute of CPU every
  # five — a 13k backlog drains over a day or so, behind live capture.
  launchd.agents."org.xinutec.recall-speech" = daemon {
    label = "org.xinutec.recall-speech";
    name = "speech";
    args = [ ];
    program = speechWrapper {
      name = "speech";
      args = [ "speech" "--root" out "--max" "120" ];
    };
    extra = {
      KeepAlive = false;
      RunAtLoad = true;
      StartInterval = 300;
      LowPriorityIO = true;
      Nice = 10;
    };
  };

  # The room transcription runner (docs/architecture.md, stage E3). Leases a
  # room block from recalld, drives the `asr` shim over stdio, pushes the result,
  # acks. Stateless: no watermark, no outbox, no mirror queue, so killing it
  # costs an expiring lease and nothing else.
  #
  # ⚠ KeepAlive, NOT a StartInterval timer, and the shim is why: it holds the
  # whisper weights for the life of the process, which is the whole reason the
  # protocol exists. A periodic agent would reload them every pass and pay that
  # cost per job instead of per boot. The runner has its own idle sleep and
  # backoff, and respawns a dead shim itself.
  #
  # ⚠ It REFUSES TO START without the vocabulary — deliberately, in the runner
  # rather than here. Transcribing without the biasing the vocabulary was built
  # for produces a corpus that has to be redone, and re-transcription is the cost
  # #1388 exists to reduce. An empty vocabulary is fine; an unreachable one is not.
  #
  # ⚠ What this does NOT do, which is what makes deploying it safe: results are
  # stored OPAQUE in ingest.sqlite's job rows. Nothing becomes a visible turn
  # until the results-to-turns step lands, so starting this changes no transcript
  # anybody reads. It transcribes the 2,539 queued blocks and stops.
  #
  # Nice + LowPriorityIO: transcription must never compete with the recorder
  # (design.md §7), and this one holds a GPU.
  launchd.agents."org.xinutec.recall-runner" = daemon {
    label = "org.xinutec.recall-runner";
    name = "runner";
    args = [ ];
    program = pkgs.writeShellApplication {
      name = "recall-runner";
      runtimeInputs = [ recall.packages.${pkgs.stdenv.hostPlatform.system}.agent-tools ];
      text = ''
        # RECALL_SYNC_TOKEN lives in .env and must never enter the store.
        ENV_FILE="''${RECALL_ENV:-$HOME/Code/recall/.env}"
        if [ -r "$ENV_FILE" ]; then
          set -a
          # shellcheck disable=SC1090  # a runtime path, deliberately not a fixed file
          . "$ENV_FILE"
          set +a
        fi

        exec env RUST_LOG=info \
          ${
            recall.packages.${pkgs.stdenv.hostPlatform.system}.audiod
          }/bin/runner --shim ${venvPython} -m recall.shim_asr
      '';
    };
    extra = {
      KeepAlive = true;
      RunAtLoad = true;
      LowPriorityIO = true;
      Nice = 10;
    };
  };

  # Store-and-forward delivery (docs/architecture.md, stage B): every closed
  # segment to recalld on Isis, sha-256 receipt verified against a local
  # re-hash before it is recorded delivered. A timer like recall-sync; each
  # pass is bounded (--max) and resumes from upload-state.sqlite, so the
  # historical backfill proceeds in bites and a killed pass costs nothing.
  # Reads RECALL_INGEST_TOKEN (the custodial `*` grant) from .env — the token
  # must never enter the store, the standing rule. Nice + LowPriorityIO:
  # delivery must never compete with the recorder (design.md §7).
  #
  # ⚠ NO EVICTION RIDES THIS. The Mac's archive stays the protected master
  # until stage F; this agent only ever adds copies.
  launchd.agents."org.xinutec.recall-upload" = daemon {
    label = "org.xinutec.recall-upload";
    name = "upload";
    args = [ ];
    program = pkgs.writeShellApplication {
      name = "recall-upload";
      text = ''
        ENV_FILE="''${RECALL_ENV:-$HOME/Code/recall/.env}"
        if [ -r "$ENV_FILE" ]; then
          set -a
          # shellcheck disable=SC1090  # a runtime path, deliberately not a fixed file
          . "$ENV_FILE"
          set +a
        fi
        exec env RUST_LOG=info ${
          recall.packages.${pkgs.stdenv.hostPlatform.system}.audiod
        }/bin/audiod upload --root ${out} --url ${ingest} --max 500
      '';
    };
    extra = {
      KeepAlive = false;
      RunAtLoad = true;
      StartInterval = 60;
      LowPriorityIO = true;
      Nice = 10;
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
  #
  # ⚠ **This is the household's pause control**, so it is the one agent where a
  # port is not a drive-by. `audiod pause-mirror` is edge-triggered on the same
  # `capture_intent_mirrored` marker the Python wrote, which is what makes the
  # swap a swap rather than a restart from zero: whichever binary runs, it reads
  # the state the other left. Sources .env for RECALL_SYNC_TOKEN the way
  # recall-upload does — audiodWrapper deliberately does not, because the ingest
  # path holds no secrets and this path does.
  launchd.agents."org.xinutec.recall-capture-mirror" = daemon {
    label = "org.xinutec.recall-capture-mirror";
    name = "capture-mirror";
    args = [ ];
    program = pkgs.writeShellApplication {
      name = "recall-capture-mirror";
      text = ''
        ENV_FILE="''${RECALL_ENV:-$HOME/Code/recall/.env}"
        if [ -r "$ENV_FILE" ]; then
          set -a
          # shellcheck disable=SC1090  # a runtime path, deliberately not a fixed file
          . "$ENV_FILE"
          set +a
        fi
        exec env RUST_LOG=info ${
          recall.packages.${pkgs.stdenv.hostPlatform.system}.audiod
        }/bin/audiod capture-mirror --root ${out} --url ${fleet}
      '';
    };
  };
}
