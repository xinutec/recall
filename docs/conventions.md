# Conventions

## Typing (strict)

The Python in this project is fully, strictly typed.

- **`mypy --strict` must pass with zero errors** on `src/`, `tests/` and `scripts/`. Config
  lives in `pyproject.toml` (`[tool.mypy]`), with several extra error codes
  enabled on top of `strict` (`possibly-undefined`, `explicit-override`,
  `ignore-without-code`, etc.).
- **Every function and method is annotated** — parameters and return type.
  ruff's `ANN` rules enforce presence; mypy enforces correctness.
- **No bare `# type: ignore`.** If a suppression is unavoidable, it carries a
  code: `# type: ignore[attr-defined]`. `warn_unused_ignores` removes them once
  they're stale.
- **No blanket `ignore_missing_imports`.** mypy resolves third-party imports
  from the venv (`python_executable`), so typed libraries (FastAPI, Pydantic,
  numpy, pyannote, torch, transformers, peft) are *really* checked.
  Only the genuinely-stubless libraries (mlx-whisper, silero-vad, datasets) are
  waived *per-module* — never globally, so missing types in our code are never
  hidden. (Checking the real types caught real bugs, e.g. a pyannote-4.0 API
  change.)
- **Avoid `Any`.** `disallow_any_unimported` is on. Prefer precise types;
  reach for `typing.Protocol`, `TypedDict`, `dataclass`, and generics over
  loose dicts. Untyped third-party return values get narrowed at the boundary,
  not propagated.
- Prefer `from __future__ import annotations` and PEP 604 (`X | None`) syntax.

## Linting & formatting

- `ruff check` and `ruff format` are the linter/formatter. Selected rule sets
  in `pyproject.toml`. Keep the tree warning-free (a standing project rule).

## Testing

- **TDD-first**: write the failing test before the implementation, even for
  small changes. Pipeline/geometry code gets real-data fixtures (captured audio
  clips), not just synthetic units.
- Tests live in `tests/` (backend) plus the frontend specs. Run the whole gate
  with **`nix run ../dev-lint#gate -- . gate.json`**; for just the backend tests use
  `nix develop --command .venv/bin/python -m pytest` (the venv holds the ML deps —
  bare `pytest` can't import numpy/fastapi). `.venv` is a symlink into the store,
  built by `nix build .#dev-env --out-link .venv`; if it is missing, that is the
  command, not `uv sync`.

## Toolchain

- Nix `devShell` provides python, mypy, ruff, pytest, sox, ffmpeg, uv, and
  node: `nix develop` (or `nix-shell`). The interpreter is the Nix one; ML deps
  go in a uv-managed venv against that interpreter (they aren't cleanly in
  nixpkgs).
- Don't reach for brew/global pip/global npm. Tools come from the flake.

## Frontend (Angular)

The web app in `frontend/` is Angular 22, kept on the most modern footing:

- **Zoneless** (no zone.js), **signals** for state, **standalone** components,
  the flat naming convention (`foo.ts` / `foo.html` / `foo.scss`, no `.component`
  suffix). Reactive reads use `httpResource`; mutations go through the typed
  `RecallApi` service.
- **External template and style files** — never inline `template:`/`styles:` in
  the `@Component` decorator.
- Use **Angular Material** components for primitives that exist (form fields,
  cards, chips, buttons, snackbar) rather than hand-rolled CSS.
- **Strict TypeScript**: `strict` plus `noUnusedLocals/Parameters`,
  `exactOptionalPropertyTypes`, and `strictTemplates` in `tsconfig.json`. The
  build must be error-free (strict templates catch real bugs).
- `ChangeDetectionStrategy.OnPush` on components; prefer `readonly` and precise
  interfaces in `models.ts` over loose shapes.

## Reading the real archive

⚠ **`Store.open()` RUNS MIGRATIONS. It is not a read.**

On 2026-09-06 a harness written to *read* `/Volumes/Backup/recall/recall.sqlite`
called `Store.open()` on it and silently migrated production to a new schema,
while the deployed agents still ran the previous revision. The doctor's archive
check failed for hours on `no such table: sweep_refusals`, and the skew was
primed to widen — the deployed code referenced five tables the new migrations
drop, so the next working-tree command would have taken four more out from under
running agents.

So, for anything that only needs to look:

- open read-only — `sqlite3.connect("file:...?mode=ro", uri=True)` — not `Store`;
- to compare implementations or test a migration, work on a **snapshot**
  (`sqlite3.backup()`), never the live file. Four daemons write that database, so
  a moving target cannot be diffed either way;
- if a migration must be exercised, run it against a `.backup` copy and check the
  row counts of everything human-authored before believing it.

## Verify cycle

Before considering a unit of work done, run **`nix run ../dev-lint#gate -- . gate.json`**
— the full gate, every row in `gate.dhall` (the count lives there, not here): `ruff check` + `ruff format
--check`, `swift-format lint --strict` (the iOS app, via the Xcode toolchain),
the venv store-path build, `mypy --strict`, `dev-lint` (custom rules), the
frontend↔backend schema contract (`gen_models.py --check`), both import-surface
checks, `pytest` (via the venv that holds the ML deps), the frontend build +
layout harness + vitest, and the Android app. All green. It runs every row and
names every one that failed, rather than stopping at the first.
A pre-commit hook runs it on every commit (`scripts/setup-hooks.sh`); there is no
separate pre-push step, so a commit that landed has already passed the gate. CI (`.github/workflows/build.yml`) builds the
image and is the gate that must stay green, but it does *not* run the full local gate
(no mypy/pytest/dev-lint there), so the local gate is the real one. Fix nearby
warnings opportunistically; don't punt them as "pre-existing".
