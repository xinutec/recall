# Conventions

## Rust

`unsafe` forbidden; clippy `pedantic` as warnings, `dbg!`, `todo!` and
`unimplemented!` denied, every target linted. Tests are integration tests
against public APIs, one binary per crate (`recalld/tests/integration/main.rs`),
never `#[cfg(test)]` modules. A rule two crates need lives in `audiocore`. Doc
comments say what a thing is for today; history is in git. An unresolved
intra-doc link fails the gate.

recalld's SQL is declared, never inline: each statement is a constant in its
module's `statements!` block, naming its database (`recalld/src/sql.rs`).
Clippy refuses rusqlite's text-taking methods elsewhere, and a test prepares
every statement against the migrated schema. A statement whose shape varies is
one statement per shape, not built at run time.

## Python

The model floor (`src/recall`), `mypy --strict` with extra codes
(`pyproject.toml`), every function annotated (ruff `ANN`), `# type: ignore` only
with a code, missing-import waivers per module (mlx-whisper, silero-vad), no
`Any` from unimported types. `ruff check` and `ruff format`.

`.venv` holds the ML dependencies and is built by nix
(`nix build .#dev-env --out-link .venv`), never `uv sync`; run tests with
`.venv/bin/python -m pytest`, since the devshell's python lacks numpy.

## Frontend

Angular 22: zoneless, signals, standalone, `OnPush`, flat file names
(`foo.ts`, `foo.html`, `foo.scss`), external templates and styles. Reads use
`httpResource`; mutations go through `RecallApi`. Angular Material for
primitives. Strict TypeScript and `strictTemplates`. Wire types are generated
from recalld (`scripts/gen-types.sh`), never hand-written.

## Tests

Test first. Pipeline code gets real-audio fixtures (`tests/fixtures/speech/`).
Tools come from the flake, never brew or global installs.

## Reading the real archive

Opening a database through recalld migrates it (`store::open`,
`meaning_schema::ensure`), which changes production under agents still on the
previous revision. To look: open read-only (`sqlite3 -readonly`,
`SQLITE_OPEN_READ_ONLY`). To compare or test a migration: a `.backup` snapshot,
never the live file, and check the human-authored row counts afterwards.

## The gate

`nix run ../dev-lint#gate -- . gate.json` runs every row of `gate.dhall` and
names each failure. The pre-commit hook (`scripts/setup-hooks.sh`) runs it, so
a landed commit has passed it. CI (`.github/workflows/build.yml`) only builds
and smoke-tests the image.
