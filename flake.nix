{
  description = "recall — local household speech recall system";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";

    # The ML runtime from uv.lock's PyPI wheels, not nixpkgs' Python packages:
    # nixpkgs has no mlx-whisper, and that stack built from source on
    # aarch64-darwin is uncached.
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

    # Builds the Rust workspace's dependencies once, as their own derivation,
    # so a Rust edit recompiles only recall's own crates.
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      pyproject-nix,
      uv2nix,
      pyproject-build-systems,
      crane,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        python = pkgs.python312;

        # --- ML runtime as a package (uv.lock -> wheels -> store path) ---------
        uvWorkspace = uv2nix.lib.workspace.loadWorkspace { workspaceRoot = ./.; };

        # "wheel", not "sdist": the point is to take PyPI's prebuilt binaries.
        uvOverlay = uvWorkspace.mkPyprojectOverlay { sourcePreference = "wheel"; };

        # mlx ships as two wheels that expect one directory: `mlx` has the .so,
        # `mlx-metal` has libmlx.dylib, found through @rpath. nix gives each wheel its
        # own store path, so the .so looks in its own output and mlx fails to import.
        # Link mlx-metal's lib/ into mlx's output, where the loader looks.
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

        # Only pyproject.toml and src/: this decides whether the ML env's store path
        # moves, and macOS ties the agents' /Volumes/Backup access to that path. At
        # the workspace root, every commit would revoke it.
        #
        # Even a comment in pyproject.toml moves it (hatchling reads the whole
        # file), so toolchain notes go in flake.nix or gate.dhall.
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

        # The same runtime plus the `dev` group: `.venv`, built by the gate
        # (gate.dhall), so the tests run against what the agents run.
        #
        # `deps.all`: mypy resolves third-party imports through `.venv/bin/python`,
        # and test tools outside it would be unfollowed imports.
        #
        # Not in the devshell, which would carry the ML closure into `ruff check`.
        devEnv = mlPythonSet.mkVirtualEnv "recall-dev-env" uvWorkspace.deps.all;

        # The devshell's interpreter: no ML, mypy and pytest for the gate; also
        # `packages.dev-python` below.
        devPython = python.withPackages (ps: [ ps.mypy ps.pytest ]);

        # The binaries the agents call by bare name, exported because home-manager
        # evaluates deploy/hm-agents.nix against its own nixpkgs, whose sox is not
        # the one recall tests against.
        agentTools = pkgs.buildEnv {
          name = "recall-agent-tools";
          paths = [ pkgs.sox pkgs.ffmpeg ];
        };

        # The Rust workspace, from a fileset of exactly its sources, so a Python or
        # frontend edit does not rebuild it. Every workspace binary is installed; the
        # agents run audiod, runner, doctor and recall-live.
        craneLib = crane.mkLib pkgs;
        rustCommon = {
          pname = "audiod";
          version = "0.1.0";
          strictDeps = true;
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./audiocore
              ./audiod
              ./recalld
              # Every workspace member, the cli and the experiments included:
              # cargo cannot load the graph with one missing.
              ./doctor
              ./runner
              ./transcript
              ./cli
              ./experimental
              # The sandbox holds only the files named here, so every test input
              # outside the crates is listed.
              #
              # The committed speech clips (#1433).
              ./tests/fixtures/speech
              # The runner/doctor pulse contract.
              ./tests/fixtures/worker-heartbeat.json
              # The runner-shim contract (#1830).
              ./tests/fixtures/shim
              # A test checks `turns::SHIM_MODEL` against the model this file
              # loads. The file, not ./src, which would rebuild the Rust
              # workspace on every Python edit.
              ./src/recall/asr.py
            ];
          };
          # On here so the cached dependencies include the test-only ones
          # `sandboxTests` needs; `audiodPkg` turns it off.
          doCheck = true;
          nativeCheckInputs = [
            # The tests decode real files through the ffmpeg the daemon runs.
            pkgs.ffmpeg
            # The speech detector loads the system ONNX runtime: the prebuilt one
            # needs AVX2, which the fleet's 2012 Xeons lack.
            pkgs.onnxruntime
            # The runner's tests drive a stub shim, a few lines of Python speaking
            # the real protocol: only the model is faked.
            pkgs.python3
          ];
          ORT_DYLIB_PATH = "${pkgs.onnxruntime}/lib/libonnxruntime${pkgs.stdenv.hostPlatform.extensions.sharedLibrary}";
        };
        # Only Cargo.lock and the members' Cargo.toml reach this (crane stubs the
        # sources), so a Rust edit reuses it.
        cargoArtifacts = craneLib.buildDepsOnly rustCommon;
        # Runs no tests: the gate's `cargo test` runs the same suite on every
        # commit, and running it again here cost each Rust commit ~75 s.
        audiodPkg = craneLib.buildPackage (
          rustCommon
          // {
            inherit cargoArtifacts;
            # dev-lint: allow-docheck-false the gate's `cargo test` runs this suite on every commit; the sandbox arm is sandboxTests
            doCheck = false;
          }
        );
        # The workspace suite inside the nix sandbox (no network, no `ps`), where
        # environment assumptions surface; `scripts/sandbox_sampler.sh` builds it.
        sandboxTests = craneLib.cargoTest (rustCommon // { inherit cargoArtifacts; });

        # Everything home-manager will run, as one output the gate can build: the
        # launchd wrappers from deploy/hm-agents.nix, keyed by label. Without it, an
        # unbuildable ml-env or a wrapper failing shellcheck surfaces only at
        # `home-manager switch`, with no commit attached.
        #
        # The module is applied as a function, not evaluated as a module, so
        # launchd option types go unchecked: a misspelled `KeepAlive` gets through.
        deployedAgents =
          let
            lib = nixpkgs.lib;
            hm = import ./deploy/hm-agents.nix {
              inherit pkgs lib;
              # Only the log directory reads it; home-manager supplies the real one.
              config.home.homeDirectory = "/home/user";
              recall.packages.${system} = {
                ml-env = mlEnv;
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

        # The Android toolchain for recall-mic (android/), in its own pkgs import and
        # dev shell so the unfree SDK licence stays scoped to it.
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
        packages.sandbox-tests = sandboxTests;
        packages.ml-env = mlEnv;
        packages.dev-env = devEnv;
        # The volume repo's gate runs mypy and its Python tests with this
        # interpreter: removing it breaks that gate, not this one.
        packages.dev-python = devPython;
        packages.agent-tools = agentTools;
        # The ONNX runtime recall-live loads, exported so home-manager gets the
        # one the tests use. home-manager reads these public outputs, not the
        # attrset `.#agents` builds from, so `.#agents` passing does not prove it.
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
          # Playwright's browsers come from the lock, not ~/Library/Caches: the
          # driver's version must match @playwright/test's (tables/deps.dhall).
          PLAYWRIGHT_BROWSERS_PATH = pkgs.playwright-driver.browsers;
          PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS = "1";
          packages = [
            devPython
            pkgs.ruff
            pkgs.sox
            pkgs.ffmpeg
            # For `uv lock` and `uv tree` only: `.venv` is `packages.dev-env`,
            # built from the lock by uv2nix.
            pkgs.uv
            pkgs.cargo
            pkgs.rustc
            pkgs.rust-analyzer
            pkgs.rustfmt
            pkgs.clippy
            # The speech detector's runtime; ORT_DYLIB_PATH below points at it.
            pkgs.onnxruntime
            # Angular 22 needs Node 24.15 or later.
            pkgs.nodejs_24
            # The frontend's installer; ignore the npm that node ships.
            pkgs.pnpm
            # dev-lint is not here: the gate runs it from its live checkout.
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
