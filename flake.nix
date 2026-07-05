{
  description = "recall — local household speech recall system";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        python = pkgs.python312;

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
