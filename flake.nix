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

        mlPythonSet =
          (pkgs.callPackage pyproject-nix.build.packages { inherit python; })
          .overrideScope (nixpkgs.lib.composeManyExtensions [
            pyproject-build-systems.overlays.default
            uvOverlay
            mlxMetalFix
            antlrBuildSystem
          ]);

        mlEnv = mlPythonSet.mkVirtualEnv "recall-ml-env" uvWorkspace.deps.default;

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
        packages.ml-env = mlEnv;

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
            # toolchain + type checking
            (python.withPackages (ps: [
              ps.mypy
              ps.pytest
            ]))
            pkgs.ruff
            # capture (Phase 0): sox captures the mic (CoreAudio, sample-accurate),
            # ffmpeg segments/encodes the stream.
            pkgs.sox
            pkgs.ffmpeg
            # ML deps (Phase 1+) live in a uv-managed venv — mlx-whisper /
            # pyannote are not cleanly in nixpkgs. uv provides the venv; the
            # interpreter stays the Nix one above for reproducibility.
            pkgs.uv
            # Angular front-end toolchain (Angular 22 needs Node >= 24.15)
            pkgs.nodejs_24
            pkgs.pnpm # the frontend's installer; node ships npm too, ignore it
            # dev-lint is invoked via `nix run ~/Code/dev-lint` in verify.sh
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
