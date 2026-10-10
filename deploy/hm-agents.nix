# hm-agents.nix — home-manager module: recall's launchd agents on the Mac mini.
#
# A pinned flake input: after editing, commit here, then in ~/.config/home-manager
# run `nix flake update recall && home-manager switch --flake .#$USER`.
#
# Each agent runs a store wrapper that names its binaries' store paths, so no
# flake evaluation sits in an agent's startup. The agents are Rust; the shims
# they drive run the uv2nix ML env (`nix build .#ml-env`).
#
# Secrets (HF_TOKEN, RECALL_SYNC_TOKEN, the ingest tokens) are read at runtime from
# ~/.config/recall/env (0600) and never enter the store.
#
# That file is on the internal disk, not under ~/Code/recall: the checkout is on
# /Volumes/Backup, which a launchd process cannot write, and whose first access
# can hang on a consent prompt nobody answers, wedging every agent that sources the
# file. An interactive shell never shows the problem.
#
# Logs go to ~/Library/Logs/recall, not the repo: launchd opens them before any
# code runs, so a log path in a checkout that moves kills the agent with exit 78
# and an empty log. recall-logrotate keeps each log to its last 2 MB.
#
# recall-capture opens the microphone and recall-live does not: live reads the UDP
# tap capture publishes (runner::live::TAP), because two CoreAudio clients on one
# device starve each other.
#
# home-manager writes each plist read-only, with a `Comment` key pointing here;
# change this file, never the plists.
{ config, pkgs, lib, recall, ... }:

let
  # The store copy of this commit: what the flake lock pins, and what the agents
  # import. Interpolating it into a wrapper also makes it a runtime dependency,
  # so it is GC-rooted by the home-manager generation.
  src = ../.;

  out = "/Volumes/Backup/recall";
  # recalld on Isis, a name its front door serves on the VPN only.
  fleet = "https://recall.xinutec.org";
  # recalld's ingest plane, the same server (docs/architecture.md, stage A).
  ingest = "https://recall.xinutec.org";
  logs = "${config.home.homeDirectory}/Library/Logs/recall";

  # The ML stack (mlx-whisper, pyannote, torch) as a store path, built from uv.lock's
  # wheels by uv2nix (`nix build .#ml-env`). It moves with the flake lock, so what
  # the agents import is pinned by the same commit as the code they run, and a
  # `uv sync` in the tree cannot change a running agent.
  #
  # No agent on this interpreter opens the microphone, so the only access it needs
  # is to /Volumes/Backup.
  venvPython = "${recall.packages.${pkgs.stdenv.hostPlatform.system}.ml-env}/bin/python";

  # Where the Hugging Face models live, declared here rather than behind a symlink,
  # so the location of tens of gigabytes is visible to whoever reads this module.
  #
  # On the external volume on purpose, and by name: /Volumes/Backup has survived
  # a disk swap because a replacement takes the name. The `cache/cache` doubling is
  # a fossil, kept because removing it means moving the models.
  hfHome = "/Volumes/Backup/cache/cache/huggingface";

  # One store wrapper per agent; `python` selects the interpreter, and each agent's
  # arguments below are the whole of what it does.
  #
  # A package, not a devshell entry: `nix develop --command` would put a full
  # flake evaluation in every agent's startup. `runtimeInputs` prepends to PATH, so
  # `say` and `launchctl` still come from launchd's system paths.
  wrapper = { name, python, args, module ? "recall" }:
    pkgs.writeShellApplication {
      name = "recall-${name}";
      # sox, ffmpeg and ffprobe, called by bare name, from recall's flake rather than
      # `pkgs`: home-manager evaluates this module against its own nixpkgs, whose
      # binaries recall's test suite has never run.
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

  # The Rust audio-plane daemon (audiod/). Sources no env file, since the ingest
  # path holds no secrets, but keeps agent-tools on PATH: audiod's segmenter is an
  # ffmpeg child, and it must be the ffmpeg the test suite runs.
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

  # The Rust health agent (doctor/). Sources the env file, unlike audiodWrapper: it
  # needs RECALL_SYNC_TOKEN to ask Isis how the live tier is running. No
  # agent-tools: the only child it spawns is itself, to read the archive.
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

  # A KeepAlive recall daemon at background priority. `extra` adds per-agent keys.
  # `program` overrides the python wrapper for agents that are not `recall <args>`.
  daemon = { label, name, python ? venvPython, args, extra ? { }, program ? null, module ? "recall" }:
    let prog = if program != null then program else wrapper { inherit name python args module; };
    in {
      enable = true;
      config = {
        Label = label;
        Comment =
          "GENERATED by home-manager from recall/deploy/hm-agents.nix. Do NOT edit "
          + "this file. To change: edit that module + commit, then in "
          + "~/.config/home-manager run 'nix flake update recall && home-manager "
          + "switch --flake .#$USER'. Runs: python -m " + module + " "
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

  # No recall-api: the Mac serves no UI. Isis (recall.xinutec.org) is the system of
  # record and the only UI; the Mac records, runs the models and keeps the master
  # archive. Isis cannot dial the Mac, so every path between them starts here: the
  # runners poll recalld's queue, and recall-capture-mirror polls the pause state.

  # Audio ingest for the phone mics, one port. Interactive for the same reason as
  # capture: it pumps the phones' live PCM into ffmpeg in real time, and a
  # throttled reader drops samples.
  launchd.agents."org.xinutec.recall-ingest" = daemon {
    label = "org.xinutec.recall-ingest";
    name = "ingest";
    args = [ ];
    program = audiodWrapper { name = "ingest"; args = [ "ingest" "--root" out ]; };
    extra = { ProcessType = "Interactive"; };
  };

  # LAN fallback for the mic heartbeat (#888), separate from the capture agents: a
  # pause closes the ingest listener, and during a pause the heartbeat is the only
  # signal left.
  #
  # No `--root`: it only forwards. A second store of beats would let two places
  # disagree about which mics are alive.
  launchd.agents."org.xinutec.recall-beat-relay" = daemon {
    label = "org.xinutec.recall-beat-relay";
    name = "beat-relay";
    args = [ ];
    program = audiodWrapper {
      name = "beat-relay";
      args = [ "beat-relay" "--url" fleet "--port" "8000" ];
    };
  };

  # The instant feed: reads capture's UDP tap, cuts it at pauses with the archive's
  # speech detector, transcribes, and posts each turn to Isis, which shows it within
  # seconds and hides it when the archive pass reaches that minute.
  #
  # It keeps no store: the post is the write, so it never touches the archive
  # volume and its stalls (#1412).
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

        # ort loads the ONNX runtime by name and macOS has none, so without
        # ORT_DYLIB_PATH the detector never loads and the agent exits at once.
        # From recall's nixpkgs, the one the tests run the detector through.
        exec env RUST_LOG=info \
          ORT_DYLIB_PATH=${
            recall.packages.${pkgs.stdenv.hostPlatform.system}.onnxruntime
          }/lib/libonnxruntime${pkgs.stdenv.hostPlatform.extensions.sharedLibrary} \
          ${
            recall.packages.${pkgs.stdenv.hostPlatform.system}.audiod
          }/bin/recall-live --url ${ingest} \
            --shim ${venvPython} -m recall.shim_asr
      '';
    };
  };

  # The USB mic, the continuous recording. A renamed or missing --device makes sox
  # fail and the agent crash-loop, visibly, rather than record from the wrong mic.
  #
  # Interactive, overriding the Background default: Background is macOS's
  # throttled class, and sox reads CoreAudio in real time. Starved, its buffer
  # overruns and samples are lost for good (#1330); under load a throttled
  # recorder lost about half the wall clock.
  launchd.agents."org.xinutec.recall-capture" = daemon {
    label = "org.xinutec.recall-capture";
    name = "capture";
    args = [ ];
    program = audiodWrapper {
      name = "capture";
      # No `--codec`: lossless is audiod's default for every recorder
      # (audiod/src/segmenter.rs), and a flag here would suggest this mic is special.
      args = [ "capture" "--root" out "--id" "usb" "--device" "USB Condenser Microphone" ];
    };
    extra = { ProcessType = "Interactive"; };
  };

  # No recall-backup: odin's nightly restic takes an integrity-checked SQLite
  # snapshot inside the Isis pod and an rsync of the recall volume, so every
  # recording is backed up server to server. Training corpora are left out; they
  # can be regenerated from the archive.

  # Is recall working? Every 5 minutes, reported to fleetwatch. Needed because
  # launchd restarts capture when it dies, and a crash loop looks like a quiet house.
  #
  # The interval must equal the doctor's INTERVAL_S (300): fleetwatch derives
  # staleness from the cadence the report declares, so a silent doctor is itself
  # the alarm.
  #
  # With KeepAlive off, launchd starts no new run while one is stuck, so one
  # wedged doctor would silence every later one. That is why the doctor reads the
  # archive in a child it can abandon.
  launchd.agents."org.xinutec.recall-doctor" = daemon {
    label = "org.xinutec.recall-doctor";
    name = "doctor";
    args = [ ];
    program = doctorWrapper {
      name = "doctor";
      # `--fleet` is how the live tier gets checked at all: it keeps no store, so
      # its output exists only on Isis. Authenticated with RECALL_SYNC_TOKEN.
      args = [ "--out" out "--post" "--fleet" fleet ];
    };
    extra = {
      KeepAlive = false;
      RunAtLoad = true;
      StartInterval = 300;
      LowPriorityIO = true;
    };
  };

  # Keeps the agents' logs bounded (#1656). Hourly; it touches nothing but
  # ~/Library/Logs/recall.
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

  # The transcription runner: leases a clip from recalld's queue, transcribes it
  # with the `asr` shim, pushes the result. Stateless, so killing it costs an
  # expiring lease.
  #
  # KeepAlive, not a StartInterval timer: the shim holds the Whisper weights for
  # its whole life, and a periodic agent would reload them for every job.
  #
  # It refuses to start if the vocabulary is unreachable (an empty one is fine):
  # transcripts made without it would have to be redone.
  #
  # Nice and LowPriorityIO: transcription must never compete with the recorder.
  #
  # The same Whisper process also transcribes messages' voice messages when
  # recall's queue is empty (`runner --messages-url`), once their token is in
  # ~/.config/messages/transcriber.env: never in recall's env file, and never
  # recall's token.
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

        MESSAGES_TRANSCRIBER_TOKEN=""
        MESSAGES_ENV="$HOME/.config/messages/transcriber.env"
        if [ -r "$MESSAGES_ENV" ]; then
          set -a
          # shellcheck disable=SC1090  # a runtime path, deliberately not a fixed file
          . "$MESSAGES_ENV"
          set +a
        fi
        messages=()
        if [ -n "$MESSAGES_TRANSCRIBER_TOKEN" ]; then
          messages=(--messages-url https://messages.xinutec.org)
        fi

        exec env RUST_LOG=info \
          ${
            recall.packages.${pkgs.stdenv.hostPlatform.system}.audiod
          }/bin/runner --pulse ${out}/worker-heartbeat.json "''${messages[@]}" \
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

  # The speaker runner: the same binary and loop, driving `shim_voices`. The shim
  # names itself over the protocol, so the runner learns what it can do.
  #
  # Nice 15, below recall-runner's 10: speakers can wait, words cannot.
  #
  # No `--pulse`: a second process stamping the archive heartbeat would make a
  # stalled transcriber look healthy.
  #
  # It writes no lines. Results go back to the queue, and `recalld::diarized` owns
  # the write: it replaces a transcript, so the guards against emptying one live
  # beside the database.
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

  # Delivery: every closed segment to recalld on Isis, counted as delivered only
  # when its sha-256 receipt matches a local re-hash. A timer, with each pass
  # bounded (--max) and resuming from upload-state.sqlite, so a killed pass costs
  # nothing. Below the recorder in priority.
  #
  # It only adds copies: nothing here deletes from the Mac's archive, which stays
  # the master.
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

  # Mirrors the pause set on Isis onto this Mac. Isis holds the wanted capture state
  # but cannot dial the Mac, so the Mac polls every ~5 s and a pause takes hold
  # within seconds. Inert until RECALL_SYNC_TOKEN is set.
  #
  # It acts only when the wanted state changes, tracked in the `capture_intent_mirrored` marker, so a restarted agent
  # carries on from where the last one stopped.
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
