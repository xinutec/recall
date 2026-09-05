{
  description = "recall — local household speech recall system";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";

    # The ML runtime as a nix package, built from uv.lock's PyPI wheels. NOT
    # from nixpkgs' python packages: nixpkgs has no mlx-whisper at all and is
    # several minors behind on transformers/peft, and compiling that stack from
    # source on aarch64-darwin is uncached (~377 derivations). Wheels make it a
    # download instead.
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

        # antlr4-python3-runtime (pulled in by omegaconf) publishes an sdist only, and
        # its pyproject omits setuptools from build-system.requires — so the isolated
        # build has no backend and fails. uv papers over this with its own fallback;
        # nix does not. Supply the build system rather than pin an older release: the
        # package is a dependency of a dependency, not something we chose.
        antlrBuildSystem = final: prev: {
          antlr4-python3-runtime = prev.antlr4-python3-runtime.overrideAttrs (old: {
            nativeBuildInputs =
              (old.nativeBuildInputs or [ ])
              ++ final.resolveBuildSystem { setuptools = [ ]; };
          });
        };

        # uv2nix installs the workspace's OWN package into the venv, so `recall`'s
        # `src` decides whether the ML env's store path moves. Left as the whole
        # workspace root, every commit produced a new `recall-ml-env` — a frontend
        # tweak or a docs line handed the eight ML agents a different binary path,
        # which is the path macOS attributes their /Volumes/Backup access to.
        #
        # The wheel is built by hatchling from `packages = ["src/recall"]`, so those
        # two files are all it can legitimately read. Narrowing `src` to them means
        # the env moves when the Python moves, and stays put otherwise.
        #
        # ⚠ **A COMMENT in pyproject.toml moves it too**, measured 2026-08-10:
        # rewording the `[tool.uv] environments` note took `recall-ml-env` from
        # `rcd89avp…` to `kmrbklrv…` with not one dependency changed, which on the
        # next `home-manager switch` is a new binary path for the agents that reach
        # /Volumes/Backup. Nothing can be trimmed here — hatchling reads the whole
        # file — so the rule is the practical one: edit pyproject.toml for a reason,
        # and keep prose that is really about the toolchain in flake.nix or
        # gate.dhall, where it is free.
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
        # It used to be a directory uv built from the same lock, which made the
        # checks the odd one out: the agents ran a store path, everything a
        # person or the gate ran came out of a mutable directory holding PyPI
        # wheels — outside the store, outside every GC root, and reconstructed by
        # hand after a fresh clone. `uv sync --check` existed only to report when
        # the two had drifted, which is a check that would not be needed if there
        # were one artifact.
        #
        # `deps.all` rather than `deps.default` is the whole difference: the dev
        # group is where `pytest` lives, and it has to be IN the environment
        # rather than beside it because mypy resolves third-party imports through
        # `python_executable = ".venv/bin/python"` — so a pytest it cannot see is
        # ~175 unfollowed-import errors under --strict.
        #
        # Deliberately NOT added to the devshell: it would drag the whole ML
        # closure into `ruff check`. The gate builds it into `.venv` in one row,
        # ahead of the rows that use it (see gate.dhall).
        devEnv = mlPythonSet.mkVirtualEnv "recall-dev-env" uvWorkspace.deps.all;

        # The non-ML interpreter, defined ONCE and used by both the devshell and the
        # launchd agents (deploy/hm-agents.nix). Same expression means the same store
        # path, and that is load-bearing: capture and ingest run this python, and
        # macOS attributes the microphone grant to the binary — a leaner interpreter
        # here would be a new path and a re-prompt on the one agent that must never
        # die. mypy/pytest ride along for that reason, not because an agent needs them.
        devPython = python.withPackages (ps: [ ps.mypy ps.pytest ]);

        # The external binaries the agents shell out to by bare name: sox captures
        # the mic, ffmpeg segments and encodes, ffprobe reads durations. Exposed as
        # a package because home-manager evaluates deploy/hm-agents.nix against ITS
        # OWN nixpkgs — writing `pkgs.sox` there would silently give the agents a
        # different sox from the one this repo pins and tests against. (The two locks
        # happen to agree today; that is not a guarantee.)
        agentTools = pkgs.buildEnv {
          name = "recall-agent-tools";
          paths = [ pkgs.sox pkgs.ffmpeg ];
        };

        # The Rust audio-plane daemon (audiod/, docs/audio-plane.md), built
        # from the WORKSPACE (docs/architecture.md, stage D1: one lockfile,
        # audiocore shared with recalld). Resolved from the committed lockfile,
        # and the build RUNS THE TESTS — a deployed audiod is one whose suite
        # passed inside the sandbox, same promise the agents row makes for the
        # Python side. The source is a fileset of exactly the Rust workspace,
        # so a Python or frontend edit does not rebuild the agents' daemon.
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
          nativeCheckInputs = [ pkgs.ffmpeg ];
        };

        # Everything home-manager will actually run, as ONE buildable output: a farm
        # of the launchd wrappers named in deploy/hm-agents.nix, keyed by label.
        #
        # The gate proved the source tree healthy and built nothing the agents run, so
        # an unbuildable ml-env or a wrapper that fails shellcheck stayed invisible
        # until `home-manager switch` — which is a different day, a different repo, and
        # a bare error with no commit attached. gamepads and thoth sat broken for weeks
        # in exactly that gap (a pnpm-deps FOD, cached green since long after it died).
        #
        # No home-manager dependency: that module is a plain function, so it is applied
        # here with the arguments home-manager passes it. The cost is that it is applied
        # rather than evaluated as a MODULE, so launchd option types are not checked —
        # a misspelled `KeepAlive` still gets through. What is covered is every part
        # that is a derivation, and that is where the breakage has actually been.
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
          platformToolsVersion = "35.0.2";
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
            # Angular front-end toolchain (Angular 22 needs Node >= 24.15)
            pkgs.nodejs_24
            pkgs.pnpm # the frontend's installer; node ships npm too, ignore it
            # dev-lint is invoked via `nix run git+file:../dev-lint?ref=HEAD` by the gate
            # (always-live, no pinned/stale copy) — not a devshell dependency.
          ];
          shellHook = ''
            export PYTHONPATH="$PWD/src''${PYTHONPATH:+:$PYTHONPATH}"
            echo "recall devshell — python: $(python --version), mypy: $(mypy --version)" >&2
          '';
        };
      }
    );
}
