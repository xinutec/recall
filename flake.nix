{
  description = "recall — local household speech recall system";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";

    # The ML runtime from uv.lock's PyPI wheels, NOT nixpkgs' python packages:
    # nixpkgs has no mlx-whisper and lags on transformers/peft, and that stack from
    # source on aarch64-darwin is uncached.
    pyproject-nix = {
      url = "github:pyproject-nix/pyproject.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    uv2nix = {
      url = "github:pyproject-nix/uv2nix";
      inputs.pyproject-nix.follows = "pyproject-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    pyproject-build-systems = {
      url = "github:pyproject-nix/build-system-pkgs";
      inputs.pyproject-nix.follows = "pyproject-nix";
      inputs.uv2nix.follows = "uv2nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { self, nixpkgs, flake-utils, pyproject-nix, uv2nix, pyproject-build-systems }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        python = pkgs.python312;

        # --- ML runtime as a package (uv.lock -> wheels -> store path) ---------
        uvWorkspace = uv2nix.lib.workspace.loadWorkspace { workspaceRoot = ./.; };

        # "wheel", not "sdist": the point is to take PyPI's prebuilt binaries.
        uvOverlay = uvWorkspace.mkPyprojectOverlay { sourcePreference = "wheel"; };

        # mlx ships as TWO wheels that expect to share one directory: `mlx` has
        # mlx/core.cpython-312-darwin.so, `mlx-metal` has mlx/lib/libmlx.dylib.
        # uv installs both into one site-packages so the .so's @rpath resolves;
        # nix gives each wheel its own store path and @rpath resolves relative to
        # the .so's OWN output, where the dylib isn't — so mlx fails to import.
        # Link mlx-metal's lib/ into mlx's output, where the loader already looks.
        mlxMetalFix = final: prev: {
          mlx = prev.mlx.overrideAttrs (old: {
            postInstall = (old.postInstall or "") + ''
              for sp in $out/lib/python*/site-packages/mlx; do
                mkdir -p "$sp/lib"
                ln -sfn ${final."mlx-metal"}/lib/python*/site-packages/mlx/lib/* "$sp/lib/"
              done
            '';
          });
        };

        # antlr4-python3-runtime ships an sdist whose pyproject omits setuptools from
        # build-system.requires, so the isolated build has no backend. uv papers over
        # it with a fallback; nix does not.
        antlrBuildSystem = final: prev: {
          antlr4-python3-runtime = prev.antlr4-python3-runtime.overrideAttrs (old: {
            nativeBuildInputs =
              (old.nativeBuildInputs or [ ])
              ++ final.resolveBuildSystem { setuptools = [ ]; };
          });
        };

        # ⚠ NARROWED, because uv2nix installs the workspace's own package and this
        # `src` decides whether `recall-ml-env`'s store path moves. At the workspace
        # root, every commit hands the ML agents a different binary path — which is
        # the identity macOS attributes their /Volumes/Backup access to.
        #
        # ⚠ A COMMENT in pyproject.toml still moves it: hatchling reads the whole
        # file. Keep toolchain prose in flake.nix or gate.dhall, where it is free.
        wheelSrc = nixpkgs.lib.fileset.toSource {
          root = ./.;
          fileset = nixpkgs.lib.fileset.unions [ ./pyproject.toml ./src ];
        };
        recallWheelSrc = _final: prev: {
          recall = prev.recall.overrideAttrs (_: { src = wheelSrc; });
        };

        mlPythonSet =
          (pkgs.callPackage pyproject-nix.build.packages { inherit python; })
          .overrideScope (nixpkgs.lib.composeManyExtensions [
            pyproject-build-systems.overlays.default
            uvOverlay
            mlxMetalFix
            antlrBuildSystem
            recallWheelSrc
          ]);

        mlEnv = mlPythonSet.mkVirtualEnv "recall-ml-env" uvWorkspace.deps.default;

        # The same runtime plus the `dev` group, and THIS is what `.venv` is.
        #
        # A STORE PATH, not a directory uv builds from the same lock — otherwise the
        # agents run a store path while the gate runs a mutable tree outside every GC
        # root, and the two need a drift check one artifact does not.
        #
        # ⚠ `deps.all`, not `deps.default`: mypy resolves third-party imports
        # through `python_executable = ".venv/bin/python"`, so a pytest beside the
        # environment rather than IN it is a flood of unfollowed-import errors.
        #
        # NOT in the devshell — it would drag the whole ML closure into
        # `ruff check`. The gate builds it into `.venv` in one row (gate.dhall).
        devEnv = mlPythonSet.mkVirtualEnv "recall-dev-env" uvWorkspace.deps.all;

        # ⚠ The non-ML interpreter, defined ONCE for both the devshell and the launchd
        # agents. macOS attributes the microphone grant to the BINARY, so a leaner
        # interpreter here is a new store path and a re-prompt on capture. mypy and
        # pytest ride along for that reason, not because an agent needs them.
        devPython = python.withPackages (ps: [ ps.mypy ps.pytest ]);

        # The binaries the agents shell out to by bare name. ⚠ Exposed as a package
        # because home-manager evaluates deploy/hm-agents.nix against ITS OWN nixpkgs:
        # `pkgs.sox` there is a different sox from the one this repo tests against.
        agentTools = pkgs.buildEnv {
          name = "recall-agent-tools";
          paths = [ pkgs.sox pkgs.ffmpeg ];
        };

        # The Rust audio-plane daemon (audiod/, docs/audio-plane.md), built from the
        # workspace. The build RUNS THE TESTS, so a deployed audiod is one whose suite
        # passed in the sandbox. The fileset is exactly the Rust workspace, so a
        # Python or frontend edit does not rebuild it.
        audiodPkg = pkgs.rustPlatform.buildRustPackage {
          pname = "audiod";
          version = "0.1.0";
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./audiocore
              ./audiod
              ./recalld
              # Every workspace MEMBER, or cargo cannot even load the graph:
              # adding a crate to Cargo.toml and not to this list fails the
              # sandboxed build with "failed to read runner/Cargo.toml".
              ./doctor
              ./runner
              # The terminal client. Not deployed as an agent, but a workspace
              # member — cargo cannot load the graph without it.
              ./cli
              # The one licence-clean speech clip (#1433). ⚠ Not pointless: every
              # OTHER fixture here is gitignored audio, so this entry is the only
              # way a committed clip reaches the sandbox.
              ./tests/fixtures/speech
              # The worker/doctor contract, whose two halves are in different
              # languages. ⚠ The sandbox has no `tests/` beyond what is named here,
              # so leaving it out fails the BUILD rather than the test.
              ./tests/fixtures/worker-heartbeat.json
              # The ASR model contract, for the same reason one entry up: the
              # queue carries no model field, so `turns::SHIM_MODEL` has to name
              # what the shim will load, and `recall.asr.DEFAULT_MODEL` is what
              # the shim actually reads. A test compares them, and the sandbox
              # has no `src/` beyond what is named here — so leaving this out
              # fails the BUILD with a missing file rather than the test with a
              # mismatch, which is a much worse error to read.
              #
              # ⚠ ONE FILE, not `./src`. This is live Python in the Rust build's
              # inputs: naming the directory would make every Python edit in the
              # repo rebuild and re-test the whole Rust workspace.
              ./src/recall/asr.py
            ];
          };
          cargoLock.lockFile = ./Cargo.lock;
          # The whole workspace builds and tests (audiocore + recalld ride
          # along — they are audiod's own test dependencies anyway); the
          # installed output carries every workspace binary, of which the
          # agents run bin/audiod.
          doCheck = true;
          # The watchdog tests decode real files through ffmpeg — the same
          # binary the daemon spawns at runtime, so the sandboxed suite
          # exercises the real verdict path, not a stub of it.
          # ffmpeg for the decode path; onnxruntime because recalld's VAD now
          # dlopens the SYSTEM runtime rather than a bundled one (the prebuilt
          # binaries need AVX2, which the fleet's 2012 Xeons lack).
          nativeCheckInputs = [
            pkgs.ffmpeg
            pkgs.onnxruntime
            # The runner's tests drive a STUB SHIM — a few lines of python
            # speaking the real stdio protocol — so the sandbox needs an
            # interpreter. Substituting only the model is the point: everything
            # else in that test is the pair that ships.
            pkgs.python3
          ];
          ORT_DYLIB_PATH = "${pkgs.onnxruntime}/lib/libonnxruntime${pkgs.stdenv.hostPlatform.extensions.sharedLibrary}";
        };

        # Everything home-manager will actually run, as ONE buildable output: a farm
        # of the launchd wrappers named in deploy/hm-agents.nix, keyed by label.
        #
        # ⚠ Without this the gate proves the source tree healthy and builds nothing
        # the agents run, so an unbuildable ml-env or a wrapper failing shellcheck
        # stays invisible until `home-manager switch` — a different day, a different
        # repo, and a bare error with no commit attached.
        #
        # ⚠ Applied as a FUNCTION, not evaluated as a module, so launchd option types
        # are unchecked and a misspelled `KeepAlive` still gets through. What is
        # covered is every part that is a derivation.
        deployedAgents =
          let
            lib = nixpkgs.lib;
            hm = import ./deploy/hm-agents.nix {
              inherit pkgs lib;
              recall.packages.${system} = {
                ml-env = mlEnv;
                dev-python = devPython;
                agent-tools = agentTools;
                audiod = audiodPkg;
                onnxruntime = pkgs.onnxruntime;
              };
            };
          in
          pkgs.linkFarm "recall-agents" (
            lib.mapAttrsToList (label: agent: {
              name = label;
              path = lib.head agent.config.ProgramArguments;
            }) hm.launchd.agents
          );

        # Android toolchain for the recall-mic app (android/). Kept in its own pkgs
        # import + dev shell so the unfree SDK licence stays scoped to it and the
        # default Python shell is unaffected.
        androidPkgs = import nixpkgs {
          inherit system;
          config.allowUnfree = true;
          config.android_sdk.accept_license = true;
        };
        androidComposition = androidPkgs.androidenv.composeAndroidPackages {
          cmdLineToolsVersion = "13.0";
          platformToolsVersion = "37.0.1";
          buildToolsVersions = [ "36.0.0" ];
          platformVersions = [ "36" ];
          abiVersions = [ ];
          includeNDK = false;
          includeSystemImages = false;
          includeEmulator = false;
        };
        androidSdk = androidComposition.androidsdk;
        androidHome = "${androidSdk}/libexec/android-sdk";
      in
      {
        packages.audiod = audiodPkg;
        packages.ml-env = mlEnv;
        packages.dev-env = devEnv;
        packages.dev-python = devPython;
        packages.agent-tools = agentTools;
        # ⚠ The ONNX runtime the speech agent must dlopen, exported so
        # home-manager reaches the SAME one this flake's tests run silero
        # through. It is not enough to hand it to the internal `.#agents`
        # attrset: home-manager imports deploy/hm-agents.nix against these
        # PUBLIC outputs, so `nix build .#agents` can pass while the real
        # switch fails on a missing attribute — which is exactly what happened.
        packages.onnxruntime = pkgs.onnxruntime;
        packages.agents = deployedAgents;

        devShells.android = androidPkgs.mkShell {
          packages = [ androidPkgs.jdk17 androidSdk androidPkgs.ktlint ];
          shellHook = ''
            export ANDROID_HOME="${androidHome}"
            export ANDROID_SDK_ROOT="${androidHome}"
            export JAVA_HOME="${androidPkgs.jdk17.home}"
            echo "recall-mic android devshell — sdk: $ANDROID_HOME" >&2
          '';
        };

        devShells.default = pkgs.mkShell {
          packages = [
            # toolchain + type checking (also `packages.dev-python` — the agents run
            # this exact derivation, so the two can never drift apart)
            devPython
            pkgs.ruff
            # capture (Phase 0): sox captures the mic (CoreAudio, sample-accurate),
            # ffmpeg segments/encodes the stream.
            pkgs.sox
            pkgs.ffmpeg
            # uv still owns the LOCK — `uv lock` after a dependency change — but
            # no longer the venv: `.venv` is `packages.dev-env`, built from that
            # lock by uv2nix. Kept here for relocking and for `uv tree`.
            pkgs.uv
            # audiod/ — the Rust audio-plane daemon (docs/audio-plane.md)
            pkgs.cargo
            pkgs.rustc
            pkgs.rust-analyzer
            pkgs.rustfmt
            pkgs.clippy
            # recalld's VAD (stage D4) dlopens the ONNX runtime rather than
            # bundling one — ort's prebuilt binaries need AVX2 and the fleet's
            # 2012 Xeons lack it, so the daemon uses whatever baseline-built
            # runtime the host provides. ORT_DYLIB_PATH below points at this one.
            pkgs.onnxruntime
            # Angular front-end toolchain (Angular 22 needs Node >= 24.15)
            pkgs.nodejs_24
            pkgs.pnpm # the frontend's installer; node ships npm too, ignore it
            # dev-lint is invoked via `nix run git+file:../dev-lint?ref=HEAD` by the gate
            # (always-live, no pinned/stale copy) — not a devshell dependency.
          ];
          shellHook = ''
            export PYTHONPATH="$PWD/src''${PYTHONPATH:+:$PYTHONPATH}"
            export ORT_DYLIB_PATH="${pkgs.onnxruntime}/lib/libonnxruntime${pkgs.stdenv.hostPlatform.extensions.sharedLibrary}"
            echo "recall devshell — python: $(python --version), mypy: $(mypy --version)" >&2
          '';
        };
      }
    );
}
