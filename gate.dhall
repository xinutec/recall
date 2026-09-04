{-
recall/gate.dhall — this repository's commit gate.

Was `scripts/verify.sh`, 195 lines, and the longest conversion in the fleet. Five
things changed beyond the mechanical port.

**The backend checks can no longer be skipped.** The script kept a `has_venv`
flag: when `uv sync` failed it printed "failed to restore virtual environment
(lack of credentials for private registry?)" and took a second branch that ran
dev-lint and nothing else — mypy, the venv/lock check, the model contract, both
import-surface checks and the whole pytest suite gone, and the run still green.
There is no private registry: all 573 URLs in `uv.lock` are
`files.pythonhosted.org`, and the only non-registry source is `editable = "."`.
The real origin is `af7b936` (2026-07-09), "make validation gates resilient to
unmounted volumes and credential blocks", which added three graceful bypasses at
once — the same commit that made `check-pii.sh` exit 0 when its denylist was
unreadable.

**The venv is built here, and there is nothing left to report on.** It was a
directory uv filled from `uv.lock`, and this gate carried a `uv sync --check` row
to say when the two had drifted — reporting rather than repairing, because the
script it replaced synced first and checked afterwards, which silently reverted
an ad-hoc `pip install` instead of reporting it. Both of those are answers to
having two artifacts. `.venv` is now `nix build .#dev-env --out-link .venv`: one
store path, built from the same lock the agents' `ml-env` is built from, and a GC
root rather than 1.5 GB of loose wheels that a fresh clone had to reconstruct by
hand. What is left to check is that it *builds*, which is the row itself.

**swift-format is a check rather than a courtesy.** `swift-format not found (no
Xcode) — Swift formatting NOT checked` was the loud-skip shape again. `env -u
DEVELOPER_DIR -u SDKROOT` is what makes `/usr/bin/xcrun` resolve the real Xcode
toolchain from inside a Nix devshell: the first variable is why `swift` reported
"tool not found", and the second is why it then reported "this SDK is not
supported by the compiler" — a nix apple-sdk 5.10 under a 6.3 compiler. Both had
to go; thoth found the second one.

**The `android/` app was ungated.** Not partly — entirely: no ktlint, no build,
and five Kotlin test classes under `app/src/test` that nothing ran. life's gate
builds its own Android app in THIS repository's `#android` dev shell, so the
toolchain was proven while the app it belongs to was not.

**The fleet import-surface check is a real file**, `scripts/check_import_surface.py`,
not a heredoc piped into `.venv/bin/python`. As a heredoc it was invisible to ruff
and mypy — the two rows immediately above it in the same gate.

`ng build`'s artifact judgement moved to `dev-lint#ng-build`, which keeps the
scratch `--output-path` (so a verify run can never clobber the bundle
`recall-build-frontend.sh` serves), keeps the retry for the macOS Piscina teardown
abort, and additionally follows chunk references and parses what it finds.

The generated `gate.json` is committed; `the table matches its Dhall` re-renders
and diffs it, so running the gate needs no `dhall`.

Rows are cheapest-first, so a fast failure does not wait behind the frontend
build — the ordering the script had, and the only thing about it that survives
unchanged.
-}

let G = ../dev-lint/gate/schema.dhall

let scratch = "dist/.verify-build"

in  { name = "recall"
    , checks =
      [ {-  The denylist lives in the encrypted data root, never in the repo —
            committing it would be the violation it guards against. A missing
            one now fails; see the script's header for why that changed.
        -}
        G.Check::{
        , name = "check-pii (no personal terms in tracked files)"
        , argv = G.inDevShell [ "scripts/check-pii.sh" ]
        , timeout_s = 300
        }
      , G.Check::{
        , name = "ruff check (lint)"
        , argv = G.inDevShell [ "ruff", "check" ]
        , timeout_s = 300
        }
      , G.Check::{
        , name = "ruff format --check (formatting)"
        , argv = G.inDevShell [ "ruff", "format", "--check" ]
        , timeout_s = 300
        }
      , {-  The Swift counterpart of `ruff format --check`. swift-format ships
            with Xcode, not Nix, and a Nix devshell exports DEVELOPER_DIR and
            SDKROOT at its own apple-sdk — unset both or xcrun resolves the wrong
            toolchain. See the header.
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
      , {-  The Rust counterpart of `ruff format --check`, for the audio-plane
            daemon (audiod/, docs/audio-plane.md).
        -}
        G.Check::{
        , name = "cargo fmt --check (audiod)"
        , cwd = "audiod"
        , argv = G.inDevShell [ "cargo", "fmt", "--all", "--check" ]
        , timeout_s = 300
        }
      , {-  `.venv` IS a store path, and this row is what makes it one.

            It was a directory uv built from the same `uv.lock`, and the check
            here was `uv sync --check`: report drift, repair nothing. That check
            only existed because there were two artifacts to compare. There is
            one now — `packages.dev-env`, uv2nix over the same lock, which is
            `packages.ml-env` plus the `dev` group — so `--out-link` points
            `.venv` at it and drift has nowhere to come from. It is also a GC
            root, which the 1.5 GB of PyPI wheels it replaced was not.

            ⚠ **This row must come before every row that runs `.venv/bin/python`**
            (the model contract, the fleet import surface, pytest) and before
            `mypy`, which resolves third-party imports through
            `python_executable = ".venv/bin/python"`. The gate runs its rows in
            order; a reshuffle that moves this one down turns four rows into
            "no such file" with nothing saying why.

            NOT in the devshell, deliberately: putting it there would drag the
            whole ML closure into `ruff check`.
        -}
        G.Check::{
        , name = "the venv is a store path (nix builds it, uv does not)"
        , argv =
            [ "nix", "build", "--no-warn-dirty", ".#dev-env", "--out-link", ".venv" ]
        , timeout_s = 1800
        }
      , {-  What home-manager will actually run, built here instead of discovered
            at `home-manager switch`. `.#agents` is a farm of the launchd wrappers
            in `deploy/hm-agents.nix`, so one row builds `ml-env`, `dev-python`,
            `agent-tools` and every wrapper — including the shellcheck pass
            `writeShellApplication` does on the wrapper text.

            Every other row in this table reads the source tree. Nothing built the
            deployed outputs, and that gap is not theoretical: gamepads and thoth
            each carried a packaged build that had been dead for weeks with a green
            gate the whole time, found only when an unrelated edit invalidated a
            cached derivation.

            Measured 2026-08-06: 2.5s when nothing moved, 21s when `src/` changed.
            Only a `uv.lock` change makes it expensive, which is the change it most
            needs to catch.
        -}
        G.Check::{
        , name = "the launchd agents build (what home-manager deploys)"
        , argv = [ "nix", "build", "--no-warn-dirty", "--no-link", ".#agents" ]
        , timeout_s = 900
        }
      , {-  Real third-party types, resolved from the .venv above.
        -}
        G.Check::{
        , name = "mypy --strict (types)"
        , argv = G.inDevShell [ "mypy" ]
        , timeout_s = 900
        }
      , {-  Fails if frontend models.ts has drifted from src/recall/schemas.py
            (responses) or src/recall/api_models.py (request bodies). The
            cross-boundary contract as a build error rather than a convention.
            Regenerate with `.venv/bin/python scripts/gen_models.py --write`.
            The .venv interpreter, because it imports pydantic.
        -}
        G.Check::{
        , name = "contract: frontend models.ts is generated from the API shapes"
        , argv =
            G.inDevShell [ ".venv/bin/python", "scripts/gen_models.py", "--check" ]
        , timeout_s = 300
        }
      , {-  recall-capture/-ingest run `python -m recall` on the DEVSHELL
            interpreter, which has no ML deps, so every ML import reachable from
            the CLI must stay lazy. pytest cannot catch a new top-level ML import
            — it runs on the fully-stocked .venv — but one would crash-loop the
            capture agent, the one process that must never die. So: import the
            CLI on the exact interpreter the agents use.
        -}
        G.Check::{
        , name = "capture-agent import surface (devshell python, no ML deps)"
        , argv = G.inDevShell [ "python", "-c", "import recall.cli" ]
        , timeout_s = 300
        }
      , {-  The other import surface: nothing reachable from recall.api may pull
            in ML, or the fleet pod CrashLoopBackOffs. See the script.
        -}
        G.Check::{
        , name = "fleet import surface (no ML reachable from recall.api)"
        , argv =
            G.inDevShell [ ".venv/bin/python", "scripts/check_import_surface.py" ]
        , timeout_s = 300
        }
      , {-  The .venv interpreter: plain `pytest` is the nix one and cannot
            import fastapi/numpy/pyannote.
        -}
        G.Check::{
        , name = "pytest (backend)"
        , argv = G.inDevShell [ ".venv/bin/python", "-m", "pytest" ]
        , timeout_s = 3600
        }
      , {-  Clippy gets its own target directory: clippy-driver and rustc
            fingerprint the workspace differently and evict each other in a
            shared one, forcing a full recompile every gate run.
        -}
        G.Check::{
        , name = "cargo clippy (audiod)"
        , cwd = "audiod"
        , argv =
            G.inDevShell
              [ "cargo", "clippy", "--all-targets", "--", "-D", "warnings" ]
        , env = G.clippyTarget
        , timeout_s = 1800
        }
      , G.Check::{
        , name = "cargo test (audiod)"
        , cwd = "audiod"
        , argv = G.inDevShell [ "cargo", "test" ]
        , timeout_s = 1800
        }
      , G.cargoDoc // { cwd = "audiod" }
      , {-  Unconditional. The script's guard was `[ ! -x
            frontend/node_modules/.bin/eslint ]`, and its own comment says why
            that is not merely a speed-up: a node_modules left behind by npm
            still has a working .bin, so verify would pass against packages the
            lockfile no longer describes. A guard that has to be right about that
            is a guard that will one day be wrong; installing every time is not.
        -}
        G.Check::{
        , name = "frontend deps match the lockfile"
        , cwd = "frontend"
        , argv = G.inDevShell [ "pnpm", "install", "--frozen-lockfile" ]
        , env = G.nonInteractive
        , timeout_s = 900
        }
      , G.Check::{
        , name = "frontend lint (eslint, type-aware)"
        , cwd = "frontend"
        , argv = G.inDevShell [ "pnpm", "run", "lint" ]
        , env = G.nonInteractive
        , timeout_s = 900
        }
      , G.Check::{
        , name = "frontend typecheck (e2e)"
        , cwd = "frontend"
        , argv = G.inDevShell [ "pnpm", "run", "typecheck:e2e" ]
        , env = G.nonInteractive
        , timeout_s = 900
        }
      , {-  A scratch --output-path, so the gate can never clobber the bundle in
            dist/recall-web that recall-build-frontend.sh serves: deploying is
            that script's job, not this one's.
        -}
        G.Check::{
        , name = "frontend build (Angular strict templates)"
        , cwd = "frontend"
        , argv =
            G.ngBuild
              "../../"
              [ "${scratch}/browser" ]
              [ "pnpm", "run", "build", "--output-path=${scratch}" ]
        , env = G.nonInteractive
        , timeout_s = 1800
        }
      , {-  Re-sync public/ into the scratch build before the harness serves it.
            The Piscina teardown abort can truncate the verbatim public/** copy,
            and a dropped Material Icons woff2 fails the harness's icon-font
            check with ligature text — a real failure with a misleading name.
            ng-build judges the assets index.html references and the chunks those
            reach; a font referenced only from CSS is outside that, so this stays
            a step of its own rather than a claim the build tool makes.
        -}
        G.Check::{
        , name = "restore public/ assets into the scratch build"
        , cwd = "frontend"
        , argv = [ "cp", "-R", "public/.", "${scratch}/browser/" ]
        , timeout_s = 120
        }
      , {-  Phone-width layout harness against that same scratch build — no
            second `ng build`. serve.mjs serves it and the specs mock every /api
            call; both are plain node, so this run does not trip the ng-cli
            teardown crash.
        -}
        G.Check::{
        , name = "frontend layout harness (playwright, phone width)"
        , cwd = "frontend"
        , argv = G.inDevShell [ "pnpm", "run", "e2e" ]
        , env = G.nonInteractive # toMap { RECALL_E2E_DIST = "${scratch}/browser" }
        , timeout_s = 1800
        }
      , G.Check::{
        , name = "frontend unit tests (vitest, jsdom)"
        , cwd = "frontend"
        , argv = G.inDevShell [ "pnpm", "test", "--watch=false" ]
        , env = G.nonInteractive # G.oneAngularWorker
        , timeout_s = 1800
        }
      , {-  ktlint does its own pattern matching, so the glob is its to expand.
        -}
        G.Check::{
        , name = "ktlint (android/)"
        , cwd = "android"
        , argv = G.inShell "..#android" [ "ktlint", "app/src/**/*.kt" ]
        , timeout_s = 900
        }
      , G.Check::{
        , name = "android :app assembleDebug"
        , cwd = "android"
        , argv =
            G.inShell
              "..#android"
              [ "./gradlew", "--console=plain", ":app:assembleDebug" ]
        , timeout_s = 1800
        }
      , {-  Five test classes under app/src/test that the shell gate never ran.
        -}
        G.Check::{
        , name = "android :app unit tests"
        , cwd = "android"
        , argv =
            G.inShell
              "..#android"
              [ "./gradlew", "--console=plain", ":app:testDebugUnitTest" ]
        , timeout_s = 1800
        }
      , {-  Strict, no baseline: the bare-dict-route debt was cleared via
            TypedDicts, so any new violation fails.
        -}
        G.devLint "../"
      , G.checkTable "../dev-lint"
      ]
    }
