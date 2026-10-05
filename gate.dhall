{-
recall/gate.dhall: this repository's commit gate. Every check runs; none is
skipped for a missing tool or venv.

Rows are cheapest first, and run in lanes beside each other: cargo in the main
lane; python (the venv before everything that runs it); the frontend, in table
order; android; and the nix builds with dev-lint. No row reads another lane's
output.

The generated `gate.json` is committed; `the table matches its Dhall`
re-renders and diffs it, so running the gate needs no `dhall`.
-}

let G = ../dev-lint/gate/schema.dhall

let scratch = "dist/.verify-build"

in  { name = "recall"
    , checks =
      [ {-  The denylist lives in the encrypted data root, not the repo; a
            missing one fails.
        -}
        G.Check::{
        , name = "check-pii (no personal terms in tracked files)"
        , argv = G.inDevShell [ "scripts/check-pii.sh" ]
        , timeout_s = 300
        }
      , {-  A doc naming a deleted file reads like one naming a live one.
        -}
        G.Check::{
        , name = "every repo path the docs cite exists"
        , argv = G.inDevShell [ "scripts/check-doc-paths.sh" ]
        , timeout_s = 120
        }
      , G.Check::{
        , name = "ruff check (lint)"
        , lane = Some "python"
        , argv = G.inDevShell [ "ruff", "check" ]
        , timeout_s = 300
        }
      , G.Check::{
        , name = "ruff format --check (formatting)"
        , lane = Some "python"
        , argv = G.inDevShell [ "ruff", "format", "--check" ]
        , timeout_s = 300
        }
      , {-  swift-format ships with Xcode, not Nix. The devshell points
            DEVELOPER_DIR and SDKROOT at its own apple-sdk; with either set,
            xcrun picks the wrong toolchain or an SDK the compiler refuses.
        -}
        G.Check::{
        , name = "swift-format lint --strict (ios)"
        , argv =
            G.inDevShell
              [ "env"
              , "-u"
              , "DEVELOPER_DIR"
              , "-u"
              , "SDKROOT"
              , "/usr/bin/xcrun"
              , "swift-format"
              , "lint"
              , "--strict"
              , "--recursive"
              , "--configuration"
              , "ios/.swift-format"
              , "ios/Sources"
              , "ios/Tests"
              ]
        , timeout_s = 600
        }
      , G.Check::{
        , name = "cargo fmt --check (workspace)"
        , argv = G.inDevShell [ "cargo", "fmt", "--all", "--check" ]
        , timeout_s = 300
        }
      , {-  `.venv` is `packages.dev-env` (the agents' `ml-env` plus the `dev`
            group, from the same `uv.lock`), linked here and kept as a GC root.

            Before every python-lane row that runs `.venv/bin/python`, and
            before `mypy`, which resolves third-party imports through it.

            Not in the devshell, which would drag the ML closure into
            `ruff check`.
        -}
        G.Check::{
        , name = "the venv is a store path (nix builds it, uv does not)"
        , lane = Some "python"
        , argv =
            [ "nix", "build", "--no-warn-dirty", ".#dev-env", "--out-link", ".venv" ]
        , timeout_s = 1800
        }
      , {-  What home-manager deploys, built here rather than discovered at
            `home-manager switch`: `.#agents` is every launchd wrapper in
            `deploy/hm-agents.nix` with what it runs, including the
            shellcheck pass on the wrapper text. The other rows read source;
            elsewhere in the fleet, packaged builds stayed broken for weeks
            behind a green gate.
        -}
        G.Check::{
        , name = "the launchd agents build (what home-manager deploys)"
        , lane = Some "nix"
        , argv = [ "nix", "build", "--no-warn-dirty", "--no-link", ".#agents" ]
        , {-  A Rust change means a cold build, ~3 minutes alone but here beside
              `cargo test` and the frontend build for the same cores; 900 s
              timed out twice under that contention.
          -}
          timeout_s = 2400
        }
      , G.Check::{
        , name = "mypy --strict (types)"
        , lane = Some "python"
        , argv = G.inDevShell [ "mypy" ]
        , timeout_s = 900
        }
      , {-  The one check on the image build, which no row runs: cargo cannot
            load the workspace without every member, so a crate missing from
            the Dockerfile breaks the image, and with :latest-only images,
            every deploy until fixed. Compared as text, which is cheap.
        -}
        G.Check::{
        , name = "every Rust workspace member reaches the Dockerfile and flake"
        , argv =
            G.inDevShell [ "python", "scripts/check_workspace_members.py" ]
        , timeout_s = 60
        }
      , {-  The domain crate stays pure (#1911): its whole dependency graph is
            on an allowlist and its source does no IO and reads no clock.
        -}
        G.Check::{
        , name = "transcript is pure (no IO dependency, no IO in source)"
        , argv = G.inDevShell [ "python", "scripts/check_pure_crate.py" ]
        , timeout_s = 120
        }
      , {-  The ts-rs bindings, regenerated from recalld and compared with
            frontend/src/app/generated.
        -}
        G.Check::{
        , name = "generated types are current"
        , argv = G.inDevShell [ "scripts/gen-types.sh", "--check" ]
        , timeout_s = 900
        }
      , {-  The ML imports stay lazy. pytest cannot show it, since the .venv has
            every ML package; the devshell interpreter has none. A module-level
            ML import would make a missing dependency kill the shim at spawn.
        -}
        G.Check::{
        , name = "shim import surface (devshell python, no ML deps)"
        , lane = Some "python"
        , argv =
            G.inDevShell
              [ "python"
              , "-c"
              , "import recall.shim_asr, recall.shim_voices, recall.score_asr"
              ]
        , timeout_s = 300
        }
      , {-  The .venv interpreter: the devshell's has no numpy or pyannote.
        -}
        G.Check::{
        , name = "pytest (backend)"
        , lane = Some "python"
        , argv = G.inDevShell [ ".venv/bin/python", "-m", "pytest" ]
        , timeout_s = 3600
        }
      , {-  Its own target directory: clippy-driver and rustc fingerprint
            differently and would evict each other's builds.
        -}
        G.Check::{
        , name = "cargo clippy (workspace)"
        , argv =
            G.inDevShell
              [ "cargo"
              , "clippy"
              , "--workspace"
              , "--all-targets"
              , "--"
              , "-D"
              , "warnings"
              ]
        , env = G.clippyTarget
        , timeout_s = 1800
        }
      , G.Check::{
        , name = "cargo test (workspace)"
        , argv = G.inDevShell [ "cargo", "test", "--workspace" ]
        , timeout_s = 1800
        }
      , G.cargoDoc // { cwd = "audiocore" }
      , G.cargoDoc // { cwd = "audiod" }
      , G.cargoDoc // { cwd = "doctor" }
      , G.cargoDoc // { cwd = "recalld" }
      , G.cargoDoc // { cwd = "runner" }
      , {-  Every run: a stale node_modules still has a working .bin, so
            checking for one cannot tell it matches the lockfile.
        -}
        G.Check::{
        , name = "frontend deps match the lockfile"
        , lane = Some "frontend"
        , cwd = "frontend"
        , argv = G.inDevShell [ "pnpm", "install", "--frozen-lockfile" ]
        , env = G.nonInteractive
        , timeout_s = 900
        }
      , G.Check::{
        , name = "frontend lint (eslint, type-aware)"
        , lane = Some "frontend"
        , cwd = "frontend"
        , argv = G.inDevShell [ "pnpm", "run", "lint" ]
        , env = G.nonInteractive
        , timeout_s = 900
        }
      , G.Check::{
        , name = "frontend typecheck (e2e)"
        , lane = Some "frontend"
        , cwd = "frontend"
        , argv = G.inDevShell [ "pnpm", "run", "typecheck:e2e" ]
        , env = G.nonInteractive
        , timeout_s = 900
        }
      , {-  A scratch --output-path, so the gate never overwrites the bundle
            recall-build-frontend.sh serves.
        -}
        G.Check::{
        , name = "frontend build (Angular strict templates)"
        , lane = Some "frontend"
        , cwd = "frontend"
        , argv =
            G.ngBuild
              "../../"
              [ "${scratch}/browser" ]
              [ "pnpm", "run", "build", "--output-path=${scratch}" ]
        , env = G.nonInteractive
        , timeout_s = 1800
        }
      , {-  The macOS Piscina teardown abort can truncate the copy of public/,
            and a missing icon font fails the harness with a misleading
            message. ng-build checks only what index.html and its chunks
            reference, not a font named in CSS.
        -}
        G.Check::{
        , name = "restore public/ assets into the scratch build"
        , lane = Some "frontend"
        , cwd = "frontend"
        , argv = [ "cp", "-R", "public/.", "${scratch}/browser/" ]
        , timeout_s = 120
        }
      , {-  On the same scratch build, served by plain node, so the ng-cli
            teardown crash cannot hit it.
        -}
        G.Check::{
        , name = "frontend layout harness (playwright, phone width)"
        , lane = Some "frontend"
        , cwd = "frontend"
        , argv = G.inDevShell [ "pnpm", "run", "e2e" ]
        , env = G.nonInteractive # toMap { RECALL_E2E_DIST = "${scratch}/browser" }
        , timeout_s = 1800
        }
      , G.Check::{
        , name = "frontend unit tests (vitest, jsdom)"
        , lane = Some "frontend"
        , cwd = "frontend"
        , argv = G.inDevShell [ "pnpm", "test", "--watch=false" ]
        , env = G.nonInteractive # G.oneAngularWorker
        , timeout_s = 1800
        }
      , {-  ktlint expands the glob itself.
        -}
        G.Check::{
        , name = "ktlint (android/)"
        , lane = Some "android"
        , cwd = "android"
        , argv = G.inShell "..#android" [ "ktlint", "app/src/**/*.kt" ]
        , timeout_s = 900
        }
      , G.Check::{
        , name = "android :app assembleDebug"
        , lane = Some "android"
        , cwd = "android"
        , argv =
            G.inShell
              "..#android"
              [ "./gradlew", "--console=plain", ":app:assembleDebug" ]
        , timeout_s = 1800
        }
      , G.Check::{
        , name = "android :app unit tests"
        , lane = Some "android"
        , cwd = "android"
        , argv =
            G.inShell
              "..#android"
              [ "./gradlew", "--console=plain", ":app:testDebugUnitTest" ]
        , timeout_s = 1800
        }
      , {-  Strict, no baseline.
        -}
        G.devLint "../" // { lane = Some "nix" }
      , G.checkTable "../dev-lint"
      ]
    }
