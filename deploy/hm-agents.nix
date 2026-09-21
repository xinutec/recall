# hm-agents.nix — home-manager module: recall launchd daemons (Mac mini).
#
# Apply after editing (a PINNED flake input, so the lock must be bumped): commit
# here, then in ~/.config/home-manager run
# `nix flake update recall && home-manager switch --flake .#pippijn`.
#
# The agents run a WRAPPER IN THE STORE whose PYTHONPATH is the store copy of this
# commit, not `~/Code/recall/src` — `./scripts/recall.sh …` is a development entry
# point only. The wrapper names the store paths of the interpreter, sox and ffmpeg
# directly rather than entering the devshell, so no flake evaluation sits in an
# agent's startup path. Same flake.lock, so the same store paths, including the
# mic-TCC-bearing python.
#
# ⚠ TWO INTERPRETERS. capture/ingest run the devshell python and the gate checks
# their import surface stays ML-free; everything else runs the uv2nix store env
# (`nix build .#ml-env`) that holds mlx/pyannote/torch.
#
# The agent env (HF_TOKEN, RECALL_SYNC_TOKEN, the ingest tokens) is read at runtime
# from ~/.config/recall/env, 0600, on the INTERNAL disk — secrets must never enter
# the store.
#
# ⚠ NOT under ~/Code/recall: that path is a symlink onto /Volumes/Backup, where a
# launchd-spawned process cannot write, and whose first touch can HANG rather than
# fail, waiting on a consent nobody is at the machine to give. That wedges every
# agent whose wrapper sources the file, in bash, before it starts. An interactive
# shell writes there fine, so it is invisible until something runs under launchd.
#
# ⚠ Logs live in ~/Library/Logs/recall, NOT in the repo: launchd opens the stdio
# paths before any code runs, so a log path inside a checkout that moves takes the
# agent down with exit 78 and an EMPTY log.
#
# Bounded hourly by the recall-logrotate agent below: each log keeps its last
# 2 MB. Before that nothing rotated them and the directory reached 112 MB.
#
# recall-capture opens the microphone; recall-live does NOT — it reads the UDP tap
# capture publishes, because two CoreAudio clients on one device starve each other
# (audiod segmenter's fanout; runner::live::TAP is the other end).
#
# home-manager writes each plist read-only with no native comment, so a provenance
# `Comment` key points back here. Do NOT hand-edit the generated plists.
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

  # The non-ML interpreter capture and ingest run — the SAME derivation the devshell
  # uses, so the store path, and the microphone grant macOS attributes to it, does
  # not move.
  devPython = "${recall.packages.${pkgs.stdenv.hostPlatform.system}.dev-python}/bin/python";

  # One store wrapper per agent; `python` selects the interpreter and the arguments
  # below are the single source of truth for what each daemon does.
  #
  # ⚠ A real package, NOT a devshell entry: `nix develop --command` puts a full flake
  # evaluation in every agent's startup path, which is catastrophic when nix's cache
  # is on the USB volume. `runtimeInputs` PREPENDS to PATH, so `say` and `launchctl`
  # still come from the system paths launchd provides.
  # Where the Hugging Face models live, DECLARED rather than symlinked.
  #
  # Declared, not inherited from a symlink: a symlink makes where tens of gigabytes
  # of models live invisible to every reader of this module (memview #645).
  #
  # ⚠ **The path is on the external volume ON PURPOSE**, and it is written by NAME:
  # `/Volumes/Backup` has survived a hardware swap underneath it because the name
  # did, which is why a replacement volume takes the name rather than the paths
  # being rewritten.
  #
  # The `cache/cache` doubling is a fossil of the era when `~/.cache` itself was
  # a symlink to `/Volumes/Backup/cache`. Kept because tidying it means moving
  # the models, and the point of this change is to stop the location being an
  # accident — not to pick a new one.
  hfHome = "/Volumes/Backup/cache/cache/huggingface";

  wrapper = { name, python, args, module ? "recall" }:
    pkgs.writeShellApplication {
      name = "recall-${name}";
      # sox captures the mic (CoreAudio, sample-perfect); ffmpeg segments and encodes,
      # and ffprobe reads durations. All are invoked by bare name. From RECALL's flake,
      # not `pkgs.sox`: this module is evaluated by home-manager against its own
      # nixpkgs, so naming them here would hand the agents binaries that no run of
      # recall's own test suite has ever seen.
      runtimeInputs = [ recall.packages.${pkgs.stdenv.hostPlatform.system}.agent-tools ];
      text = ''
        ENV_FILE="''${RECALL_ENV:-$HOME/.config/recall/env}"
        if [ -r "$ENV_FILE" ]; then
          set -a
          # shellcheck disable=SC1090  # a runtime path, deliberately not a fixed file
          . "$ENV_FILE"
          set +a
        fi

        exec env PYTHONPATH=${src}/src HF_HOME=${hfHome} ${python} -m ${module} ${lib.escapeShellArgs args}
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
  # doctor needs RECALL_SYNC_TOKEN to ask Isis how the live tier is running,
  # which is the one thing it cannot see from this volume. It needs no
  # agent-tools — the doctor spawns only ITSELF, as the bounded child that
  # reads the archive.
  doctorWrapper = { name, args }:
    pkgs.writeShellApplication {
      name = "recall-${name}";
      text = ''
        ENV_FILE="''${RECALL_ENV:-$HOME/.config/recall/env}"
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
  daemon = { label, name, python ? devPython, args, extra ? { }, program ? null, module ? "recall" }:
    let prog = if program != null then program else wrapper { inherit name python args module; };
    in {
      enable = true;
      config = {
        Label = label;
        Comment =
          "GENERATED by home-manager from recall/deploy/hm-agents.nix. Do NOT edit "
          + "this file. To change: edit that module + commit, then in "
          + "~/.config/home-manager run 'nix flake update recall && home-manager "
          + "switch --flake .#pippijn'. Runs: python -m " + module + " "
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
  # Mac is capture, all MLX, and the protected master archive. Browsers and the phone web
  # app point at Isis; pause/resume is mirrored down by recall-capture-mirror. Work that
  # needs the GPU reaches the Mac by its own poll of recalld's queue — Isis cannot dial a
  # one-way peer, so every path here is Mac-initiated.

  # Single-port audio ingest for the phone mics — audiod (Rust).
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
  # lifecycle would be shut precisely when it was needed.
  #
  # ⚠ NO `--root`: it FORWARDS and stores nothing. A second beat store would let two
  # places disagree about which mics are alive.
  launchd.agents."org.xinutec.recall-beat-relay" = daemon {
    label = "org.xinutec.recall-beat-relay";
    name = "beat-relay";
    args = [ ];
    program = audiodWrapper {
      name = "beat-relay";
      args = [ "beat-relay" "--url" fleet "--port" "8000" ];
    };
  };

  # ⚠ **`recall-worker` WAS HERE and is gone (#1538).** It indexed and
  # transcribed the Mac's own segments; `runner` now leases `transcribe-segment`
  # from Isis and drives the same shim with the same model, and recalld's
  # `turns::PER_MIC` pass writes the turns, including the live reconciliation.
  #
  # Nothing replaced its SCAN: it also discovered new source directories and
  # cleared dead-capture stubs. `audiod capture` registers its own source and
  # the ingest plane is authoritative for what exists, so the scan had no
  # remaining reader — but if a phantom source or an uncleared stub shows up,
  # that is where it came from.


  # The one process on this Mac that holds the LLM weights (src/recall/llmhost.py).
  # recall's summaries/Ask and life's emotion worker are clients over 127.0.0.1:8092;
  # neither loads a model of its own, so the ~4.3 GB is paid once and released after
  # five idle minutes.
  #
  # ProcessType overrides the Background default the other daemons take: an Ask has a
  # human waiting on it, and the throttled I/O made the cold weight read visibly
  # slower than the same load from a shell (104s vs 62s, measured). Idle it costs a
  # few MB, so it competes with capture only while it is actually answering.
  # ⚠ ENTERS THROUGH ITS OWN MODULE, not through `recall llm-host`. This is the
  # one Python agent that stays, and while it started via the CLI it held the
  # whole CLI substrate alive in production: `recall.cli` imports 28 `recall.*`
  # modules where this needs 4 (#1342).
  launchd.agents."org.xinutec.recall-llm-host" = daemon {
    label = "org.xinutec.recall-llm-host";
    name = "llm-host";
    python = venvPython;
    module = "recall.llmhost";
    args = [ ];
    extra = { ProcessType = "Standard"; };
  };

  # The instant feed. Reads the UDP tap capture
  # publishes, cuts it at the pauses with the same silero the archive uses,
  # drives the asr shim, and POSTs each turn to Isis, which shows it within
  # seconds and hides it once the archive pass reaches that minute.
  #
  # ⚠ It holds NO STORE — the push IS the write, so there is no `--out` and this
  # agent touches the archive volume nowhere (#1412's stalls cannot reach it).
  launchd.agents."org.xinutec.recall-live" = daemon {
    label = "org.xinutec.recall-live";
    name = "live";
    args = [ ];
    program = pkgs.writeShellApplication {
      name = "recall-live";
      runtimeInputs = [ recall.packages.${pkgs.stdenv.hostPlatform.system}.agent-tools ];
      text = ''
        # RECALL_SYNC_TOKEN lives in .env and must never enter the store.
        ENV_FILE="''${RECALL_ENV:-$HOME/.config/recall/env}"
        if [ -r "$ENV_FILE" ]; then
          set -a
          # shellcheck disable=SC1090  # a runtime path, deliberately not a fixed file
          . "$ENV_FILE"
          set +a
        fi

        # ORT_DYLIB_PATH for the same reason speechWrapper sets it: ort dlopens
        # the ONNX runtime by name and macOS has no system libonnxruntime, so
        # without this the detector never loads and the agent exits at once.
        exec env RUST_LOG=info \
          ORT_DYLIB_PATH=${
            recall.packages.${pkgs.stdenv.hostPlatform.system}.onnxruntime
          }/lib/libonnxruntime${pkgs.stdenv.hostPlatform.extensions.sharedLibrary} \
          ${
            recall.packages.${pkgs.stdenv.hostPlatform.system}.audiod
          }/bin/recall-live --url ${ingest} --api ${fleet} \
            --shim ${venvPython} -m recall.shim_asr
      '';
    };
  };

  # Mic agent — the critical continuous recording stream (USB mic → segments).
  # Devshell python (no ML deps): the one process that must never die. A renamed or
  # missing --device makes sox fail hard and the agent crash-loop, visibly, rather
  # than silently recording from the wrong mic.
  # ⚠ `Interactive`, overriding the `Background` default: Background is macOS's
  # THROTTLED class, and sox reads CoreAudio in real time — starve it and its buffer
  # overruns, samples are DROPPED, and the segment ring stretches. That is silent,
  # unrecoverable loss of household speech (#1330). Measured under load, a throttled
  # recorder loses roughly half the wall clock; ANY neighbour outranks it.
  launchd.agents."org.xinutec.recall-capture" = daemon {
    label = "org.xinutec.recall-capture";
    name = "capture";
    args = [ ];
    program = audiodWrapper {
      name = "capture";
      # ⚠ No `--codec` here, and that is deliberate: lossless is audiod's DEFAULT,
      # so every recorder gets it without a flag. Carrying it explicitly on this
      # one agent would read as "the condenser is special" and leave the others on a
      # default nobody revisited. The reasoning lives with the default, in
      # audiod/src/segmenter.rs; retention is docs/architecture.md.
      args = [ "capture" "--root" out "--id" "usb" "--device" "USB Condenser Microphone" ];
    };
    extra = { ProcessType = "Interactive"; };
  };

  # NO recall-backup here: odin's nightly restic already takes an
  # integrity-checked SQLite snapshot from inside the Isis pod plus an audio rsync
  # of the recall PVC, so every recording is protected server-to-server.
  #
  # The only content Isis lacks is the training corpora, derived from the archive
  # and deliberately NOT backed up — they can be regenerated.

  # Is recall actually working? Every 5 minutes, reported to fleetwatch.
  #
  # ⚠ launchd RESTARTS capture when it dies, so a persistent fault becomes a loop —
  # and a crash loop looks exactly like a quiet house.
  #
  # ⚠ The interval MUST match the doctor's declared INTERVAL_S (300): fleetwatch
  # derives staleness from the cadence the report declares, so this agent dying, or
  # the Mac dying, is itself the alarm.
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
      # ⚠ `--fleet` is what makes the live checks measurable at all: the tier
      # is a Mac agent that keeps no store, so its output is only on Isis. The
      # bearer is RECALL_SYNC_TOKEN, which doctorWrapper already sources.
      args = [ "--out" out "--post" "--fleet" fleet ];
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
  # Bounds the agents' own logs (#1656). Hourly, cheap, and it touches nothing
  # but `~/Library/Logs/recall`.
  launchd.agents."org.xinutec.recall-logrotate" = daemon {
    label = "org.xinutec.recall-logrotate";
    name = "logrotate";
    program = audiodWrapper { name = "logrotate"; args = [ "logrotate" ]; };
    args = [ ];
    extra = {
      KeepAlive = false;
      RunAtLoad = true;
      StartInterval = 3600;
      LowPriorityIO = true;
      Nice = 15;
    };
  };

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
  # ⚠ KeepAlive, NOT a StartInterval timer: the shim holds the whisper weights for
  # the life of the process, so a periodic agent would reload them per job instead
  # of per boot. The runner has its own idle sleep and respawns a dead shim.
  #
  # ⚠ It REFUSES TO START without the vocabulary (in the runner, not here):
  # transcribing without that biasing produces a corpus that has to be redone
  # (#1388). An EMPTY vocabulary is fine; an unreachable one is not.
  #
  # ⚠ Results are stored OPAQUE in ingest.sqlite's job rows, so running this
  # changes no transcript anybody reads.
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
        ENV_FILE="''${RECALL_ENV:-$HOME/.config/recall/env}"
        if [ -r "$ENV_FILE" ]; then
          set -a
          # shellcheck disable=SC1090  # a runtime path, deliberately not a fixed file
          . "$ENV_FILE"
          set +a
        fi

        exec env RUST_LOG=info \
          ${
            recall.packages.${pkgs.stdenv.hostPlatform.system}.audiod
          }/bin/runner --pulse ${out}/worker-heartbeat.json \
            --shim ${venvPython} -m recall.shim_asr
      '';
    };
    extra = {
      KeepAlive = true;
      RunAtLoad = true;
      LowPriorityIO = true;
      Nice = 10;
    };
  };

  # Stage E4's `voices` runner: same binary and loop as recall-runner, driving
  # `shim_voices` instead of `shim_asr`. The shim NAMES ITSELF over the protocol,
  # so the runner discovers it can do `diarize-segment` rather than being told.
  #
  # ⚠ It leases `diarize-segment` ONLY (`kinds_for`): `diarize-room` jobs are
  # derived for every transcribed block whether or not anything consumes them, and
  # `queue::lease` orders across kinds by capture time, so leasing both spends half
  # of every pass on results nothing reads.
  #
  # `Nice = 15`, below recall-runner's 10 — diarization is re-derivable where the
  # archive pass is not. ⚠ No `--pulse`: a second process stamping the archive
  # heartbeat would make a stalled transcriber look healthy.
  #
  # ⚠ It writes no turns. The result goes back to the queue and
  # `recalld::diarized` owns the only write, because that write REPLACES a
  # transcript and the guards against emptying one belong beside the database.
  launchd.agents."org.xinutec.recall-voices" = daemon {
    label = "org.xinutec.recall-voices";
    name = "voices";
    args = [ ];
    program = pkgs.writeShellApplication {
      name = "recall-voices";
      runtimeInputs = [ recall.packages.${pkgs.stdenv.hostPlatform.system}.agent-tools ];
      text = ''
        # RECALL_SYNC_TOKEN lives in .env and must never enter the store.
        ENV_FILE="''${RECALL_ENV:-$HOME/.config/recall/env}"
        if [ -r "$ENV_FILE" ]; then
          set -a
          # shellcheck disable=SC1090  # a runtime path, deliberately not a fixed file
          . "$ENV_FILE"
          set +a
        fi

        exec env RUST_LOG=info \
          ${
            recall.packages.${pkgs.stdenv.hostPlatform.system}.audiod
          }/bin/runner \
            --shim ${venvPython} -m recall.shim_voices
      '';
    };
    extra = {
      KeepAlive = true;
      RunAtLoad = true;
      LowPriorityIO = true;
      Nice = 15;
    };
  };

  # Store-and-forward delivery (docs/architecture.md, stage B): every closed
  # segment to recalld on Isis, sha-256 receipt verified against a local
  # re-hash before it is recorded delivered. A timer, not KeepAlive; each
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
        ENV_FILE="''${RECALL_ENV:-$HOME/.config/recall/env}"
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
        ENV_FILE="''${RECALL_ENV:-$HOME/.config/recall/env}"
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
