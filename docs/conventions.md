# Conventions

## Typing (strict)

The Python in this project is fully, strictly typed.

- **`mypy --strict` must pass with zero errors** on `src/` and `tests/`. Config
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
  with **`./scripts/verify.sh`**; for just the backend tests use
  `nix develop --command .venv/bin/python -m pytest` (the venv holds the ML deps —
  bare `pytest` can't import numpy/fastapi).

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

## Verify cycle

Before considering a unit of work done, run **`./scripts/verify.sh`** — the full
gate: `ruff check` + `ruff format --check`, `swift-format lint --strict` (the iOS
app, via the Xcode toolchain), `mypy --strict`, `dev-lint` (custom rules), the
frontend↔backend schema contract (`gen_models.py --check`), `pytest` (via the venv
that holds the ML deps), and the frontend build + vitest. All green.
A pre-push hook is installed to run it, though the repo is local-only so nothing
triggers it yet — run it by hand. Fix nearby warnings opportunistically; don't punt
them as "pre-existing".
