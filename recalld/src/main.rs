//! recalld — see lib.rs and docs/architecture.md.
//!
//!   recalld --root <data-root> [--bind <addr:port>]... [--tokens <file>]
//!           [--upstream <url>] [--frontend <dir>]
//!
//! `--bind` REPEATS. The fleet gives recalld both the ingest port recorders
//! already push to and the port the browser already uses, so neither the
//! recorders nor the registered OAuth redirect has to move for recalld to become
//! the front door. `--upstream` is the Python API beside it in the pod, which
//! serves whatever recalld has not ported yet; `--frontend` is the built Angular
//! app.
//!
//! `RECALLD_READ_TOKEN` (env, optional) gates the read side; per-source write
//! tokens come from `--tokens <file>` or the `RECALLD_INGEST_TOKENS` env var
//! (same line grammar). Everything unset = open, for dev and tests.
//!
//! `RECALL_SYNC_TOKEN` (env, optional) is different: it does not open or close a
//! gate, it decides whether the `/sync/*` routes are MOUNTED at all. Unset, they
//! stay with the Python upstream.

use recalld::app::{Config, DEFAULT_MAX_BODY, router};
use recalld::tokens::Tokens;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

fn usage() -> ExitCode {
    eprintln!(
        "usage: recalld --root <data-root> [--bind <addr:port>]... [--tokens <file>] \
         [--upstream <url>] [--frontend <dir>]"
    );
    ExitCode::FAILURE
}

/// Everything the command line says, or `None` if it does not parse.
struct Args {
    root: PathBuf,
    binds: Vec<String>,
    tokens_path: Option<PathBuf>,
    upstream: Option<String>,
    frontend: Option<PathBuf>,
}

fn parse_args() -> Option<Args> {
    let mut args = std::env::args().skip(1);
    let mut root: Option<PathBuf> = None;
    let mut binds: Vec<String> = Vec::new();
    let mut tokens_path: Option<PathBuf> = None;
    let mut upstream: Option<String> = None;
    let mut frontend: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        let value = args.next()?;
        match arg.as_str() {
            "--root" => root = Some(PathBuf::from(value)),
            "--bind" => binds.push(value),
            "--tokens" => tokens_path = Some(PathBuf::from(value)),
            "--upstream" => upstream = Some(value),
            "--frontend" => frontend = Some(PathBuf::from(value)),
            _ => return None,
        }
    }
    let root = root?;
    if binds.is_empty() {
        binds.push(String::from("127.0.0.1:8001"));
    }
    Some(Args {
        root,
        binds,
        tokens_path,
        upstream,
        frontend,
    })
}

/// Serve one bound listener, WITH connect info.
///
/// ⚠ The connect info is not decoration. The capture-control audit answers "was
/// that pause mine?" on a plane that deliberately carries no credential, so the
/// peer address is the ONLY identifying thing there is. Without it the extension
/// is absent and every pause is recorded against `unknown-host`.
///
/// recalld sees the REAL client here, which the Python never could: it sits
/// behind this proxy and only ever saw 127.0.0.1 (#1473).
fn serve_one(
    serving: &mut tokio::task::JoinSet<std::io::Result<()>>,
    listener: tokio::net::TcpListener,
    app: axum::Router,
) {
    let addr = listener.local_addr().ok();
    tracing::info!(?addr, "recalld: listening");
    serving.spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
    });
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let Some(Args {
        root,
        binds,
        tokens_path,
        upstream,
        frontend,
    }) = parse_args()
    else {
        return usage();
    };
    // A configured-but-unreadable token table fails closed at startup: an
    // open ingest plane must be a choice, never the residue of a typo.
    let tokens = match (tokens_path, std::env::var("RECALLD_INGEST_TOKENS")) {
        (Some(path), _) => match Tokens::load(&path) {
            Ok(tokens) => Some(tokens),
            Err(err) => {
                eprintln!("recalld: cannot read tokens file {}: {err}", path.display());
                return ExitCode::FAILURE;
            }
        },
        (None, Ok(text)) if !text.trim().is_empty() => match Tokens::parse(&text) {
            Ok(tokens) => Some(tokens),
            Err(err) => {
                eprintln!("recalld: RECALLD_INGEST_TOKENS does not parse: {err}");
                return ExitCode::FAILURE;
            }
        },
        _ => None,
    };
    let read_token = std::env::var("RECALLD_READ_TOKEN")
        .ok()
        .filter(|t| !t.is_empty());
    // The Mac→fleet sync plane. Unset = the routes are not mounted and `/sync/*`
    // still reaches the Python upstream, which is what makes shipping this code
    // and CUTTING OVER to it two separate acts.
    let sync_token = std::env::var("RECALL_SYNC_TOKEN")
        .ok()
        .filter(|t| !t.is_empty());
    if let Err(err) = recalld::store::open(&root) {
        eprintln!(
            "recalld: cannot open {}/ingest.sqlite: {err}",
            root.display()
        );
        return ExitCode::FAILURE;
    }
    // The browsing plane is mounted only when SSO is configured. Absent means
    // ABSENT, not open: these routes serve household transcripts, so an
    // unconfigured recalld must not answer them at all.
    let webauth = recalld::webauth::Config::from_env(&|k| std::env::var(k).ok()).map(|cfg| {
        recalld::webauth::GateState {
            cfg: std::sync::Arc::new(cfg),
            now: std::sync::Arc::new(|| chrono::Utc::now().timestamp()),
        }
    });
    if webauth.is_some() {
        tracing::info!("browsing plane mounted behind the Nextcloud SSO gate");
    }
    let config = Arc::new(Config {
        root,
        tokens,
        read_token,
        max_body_bytes: DEFAULT_MAX_BODY,
        webauth,
        sync_token,
        upstream: upstream.map(|base| recalld::proxy::Upstream { base }),
        frontend,
    });
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("recalld: runtime: {err}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(async move {
        let Some(listeners) = bind_all(&binds).await else {
            return ExitCode::FAILURE;
        };
        spawn_level_scanner(config.root.clone());
        spawn_speech_scanner(config.root.clone());
        spawn_room_builder(config.root.clone());
        spawn_room_registrar(config.root.clone());
        // ⚠ **OFF since 2026-09-11, MEASURED.** Its first 20 blocks produced turns
        // materially worse than the per-mic transcripts they hid, over the same
        // minutes:
        //
        //     repetition loops   room 16/73 (22%)   per-mic 0/160 (0%)
        //     median confidence  room 0.509         per-mic 0.683
        //     median chars/turn  room 20            per-mic 45
        //     languages          room en 56, nl 10  per-mic nl 111, en 45
        //
        // The language row is the finding: the microphones hear a DUTCH
        // household and the room stream reports mostly English, which is
        // Whisper's known failure on degraded audio — default to English and
        // invent. Whether the fault is the room AUDIO or the missing read-path
        // filters (#1410 sweeps the per-mic corpus of exactly these; the room
        // output was written raw) is the next question, and it is not answerable
        // by leaving this on.
        //
        // Re-enable only with that answered and a fresh comparison in hand.
        // spawn_room_turn_writer(config.root.clone());
        let app = router(config);
        let mut serving = tokio::task::JoinSet::new();
        for listener in listeners {
            serve_one(&mut serving, listener, app.clone());
        }
        // The FIRST listener to stop decides the exit: if one port dies the daemon
        // is half-serving, which is the state that hides a fault. Better to exit
        // and be restarted whole.
        match serving.join_next().await {
            Some(Ok(Ok(()))) => ExitCode::SUCCESS,
            Some(Ok(Err(err))) => {
                eprintln!("recalld: serve: {err}");
                ExitCode::FAILURE
            }
            Some(Err(err)) => {
                eprintln!("recalld: serve task failed: {err}");
                ExitCode::FAILURE
            }
            None => ExitCode::FAILURE,
        }
    })
}

/// Bind every address BEFORE serving any.
///
/// ⚠ A half-bound daemon — answering recorders but not the browser, or the
/// reverse — is worse than one that refuses to start, because it looks healthy
/// from whichever side you happen to check.
async fn bind_all(binds: &[String]) -> Option<Vec<tokio::net::TcpListener>> {
    let mut listeners = Vec::new();
    for addr in binds {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => listeners.push(listener),
            Err(err) => {
                eprintln!("recalld: cannot bind {addr}: {err}");
                return None;
            }
        }
    }
    Some(listeners)
}

/// Stage D2: the calibration scanner, measuring levels for every delivered
/// segment in bounded batches. Off the request path — its ffmpeg children and
/// sqlite writes ride `spawn_blocking`, and WAL keeps it from ever blocking
/// an upload.
fn spawn_level_scanner(root: PathBuf) {
    const BATCH: usize = 200;
    const IDLE: std::time::Duration = std::time::Duration::from_secs(30);
    const BACKOFF: std::time::Duration = std::time::Duration::from_mins(1);
    tokio::spawn(async move {
        loop {
            let batch_root = root.clone();
            let wrote =
                tokio::task::spawn_blocking(move || recalld::levels::scan_once(&batch_root, BATCH))
                    .await;
            match wrote {
                Ok(Ok(0)) => tokio::time::sleep(IDLE).await,
                Ok(Ok(n)) => tracing::info!(measured = n, "levels: batch complete"),
                Ok(Err(err)) => {
                    tracing::warn!(%err, "levels: scan failed; backing off");
                    tokio::time::sleep(BACKOFF).await;
                }
                Err(err) => {
                    tracing::error!(%err, "levels: task failed; backing off");
                    tokio::time::sleep(BACKOFF).await;
                }
            }
        }
    });
}

/// Stage D4: the speech scanner — VAD over every delivered segment, so
/// "active" can mean someone is TALKING rather than bytes arrived, and the
/// quiet review has evidence before it proposes deleting anything.
///
/// A SMALLER batch than the level scanner's: this runs a neural network per
/// 32 ms window rather than an envelope, and Isis has four cores shared with
/// Nextcloud. The idle wait is longer for the same reason — speech evidence is
/// wanted within minutes, never within seconds.
fn spawn_speech_scanner(root: PathBuf) {
    // ⚠ Do not even start where the model cannot run. isis and amun are Ivy
    // Bridge (2012) and ort's prebuilt runtime needs AVX2 (Haswell, 2013):
    // calling it there raises SIGILL and kills the daemon that IS the system of
    // record. Refusing once, loudly, beats failing every batch for ever.
    const BATCH: usize = 40;
    const IDLE: std::time::Duration = std::time::Duration::from_mins(2);
    const BACKOFF: std::time::Duration = std::time::Duration::from_mins(5);
    if let Err(err) = audiocore::vad::self_test() {
        tracing::warn!(
            %err,
            "speech: DISABLED — the ONNX runtime could not run a trial inference. \
             Segments stay unmeasured until it can; the ingest plane is unaffected."
        );
        return;
    }
    tokio::spawn(async move {
        loop {
            let batch_root = root.clone();
            let wrote =
                tokio::task::spawn_blocking(move || recalld::speech::scan_once(&batch_root, BATCH))
                    .await;
            match wrote {
                Ok(Ok(0)) => tokio::time::sleep(IDLE).await,
                Ok(Ok(n)) => tracing::info!(measured = n, "speech: batch complete"),
                Ok(Err(err)) => {
                    tracing::warn!(%err, "speech: scan failed; backing off");
                    tokio::time::sleep(BACKOFF).await;
                }
                Err(err) => {
                    tracing::error!(%err, "speech: task failed; backing off");
                    tokio::time::sleep(BACKOFF).await;
                }
            }
        }
    });
}

/// Stage D3: the room builder — one settled block at a time, calibrated
/// selection, terminal verdicts only. Chases the level scanner: a block whose
/// evidence is incomplete defers and returns next pass.
/// Stage E3a: turn stored room results into turns people actually read.
///
/// ⚠ **THE FIRST LOOP HERE THAT CHANGES A TRANSCRIPT SOMEBODY READS.** Everything
/// above it derives, measures or registers. This one writes room turns and hides
/// the per-mic turns they cover — measured 2026-09-11 before it was switched on:
/// 933 transcribed blocks against 25,349 visible per-mic turns in the same span.
/// That ratio IS the point (four or five microphones transcribing one minute,
/// #1388), and it is still thousands of rows changing state.
///
/// ⚠ **HOW TO PUT IT BACK. It is TWO PLANES, and one of them is easy to miss.**
/// Hiding is not deleting, so the meaning plane (`recall.sqlite`) undoes cleanly:
///
/// ```sql
/// UPDATE transcript_segments SET hidden_reason = NULL
///  WHERE hidden_reason = 'covered by the room stream';
/// DELETE FROM transcript_segments WHERE provenance = 'room';
/// ```
///
/// That restores what anybody reads, and by itself it re-enables every block
/// whose turns it just deleted — `write_pass` derives "already written" from
/// those very rows, deliberately, so this much needs no bookkeeping.
///
/// ⚠ But the blocks that wrote NOTHING left no rows to delete, so they are held
/// in a ledger in the INGEST plane (`ingest.sqlite`) instead, and it has to go
/// too or they stay decided:
///
/// ```sql
/// DELETE FROM room_turn_ledger;
/// ```
///
/// Forget it and the reversal LOOKS complete — the transcripts are back, the
/// room rows are gone — while every refused or swept block silently never gets
/// reconsidered. `room_turns::ensure_ledger` carries the same warning from the
/// other side.
///
/// Written here rather than in a task because the person who needs it will be
/// reading this file, not searching for the note.
///
/// A SMALL batch on a slow cadence, deliberately: the queue drains over hours
/// instead of minutes, so a bad verdict is noticed while it is dozens of blocks
/// rather than nine hundred.
#[expect(
    dead_code,
    reason = "off since 2026-09-11 pending the quality answer above; kept whole so re-enabling is one line, not a rewrite"
)]
fn spawn_room_turn_writer(root: PathBuf) {
    const EVERY: std::time::Duration = std::time::Duration::from_mins(2);
    const BATCH: usize = 20;
    tokio::spawn(async move {
        loop {
            let pass_root = root.clone();
            let done = tokio::task::spawn_blocking(move || {
                let ingest = recalld::store::open(&pass_root)?;
                let mut meaning = recalld::work::open_write(&pass_root)?;
                let now = chrono::Utc::now().to_rfc3339();
                recalld::room_turns::write_pass(
                    &mut meaning,
                    &ingest,
                    recalld::room_turns::ROOM_MODEL,
                    &now,
                    BATCH,
                )
            })
            .await;
            match done {
                // ⚠ `swept` is IN this guard, and leaving it out is how the
                // interesting case goes quiet: a block whose room turns are all
                // repetition loops writes nothing, hides nothing and refuses
                // nothing, so without it the one pass that says the room audio
                // is bad is the one pass that logs no line at all.
                Ok(Ok(pass)) if pass.turns + pass.hidden + pass.refused + pass.swept > 0 => {
                    tracing::info!(
                        blocks = pass.blocks,
                        turns = pass.turns,
                        hidden = pass.hidden,
                        refused = pass.refused,
                        swept = pass.swept,
                        barren = pass.barren,
                        "room turns: written"
                    );
                }
                Ok(Ok(_)) => {}
                Ok(Err(err)) => tracing::warn!(%err, "room turns: pass failed"),
                Err(err) => tracing::error!(%err, "room turns: task failed"),
            }
            tokio::time::sleep(EVERY).await;
        }
    });
}

/// Register built room blocks in the meaning plane, so their turns have audio.
///
/// Its own loop rather than a step inside the builder's, because it spans BOTH
/// planes — `segments` in `ingest.sqlite` and `audio_segments` in
/// `recall.sqlite` — and the builder deliberately touches only its own.
///
/// Idempotent, so the first pass after a deploy backfills every block ever built
/// and each later pass costs one indexed scan. No IDLE branch: there is nothing
/// to back off from, and a pass that inserts nothing is the normal case.
fn spawn_room_registrar(root: PathBuf) {
    const EVERY: std::time::Duration = std::time::Duration::from_mins(5);
    tokio::spawn(async move {
        loop {
            let pass_root = root.clone();
            let done = tokio::task::spawn_blocking(move || {
                let ingest = recalld::store::open(&pass_root)?;
                let meaning = recalld::work::open_write(&pass_root)?;
                let room_dir = recalld::store::source_dir(&pass_root, recalld::room::ROOM_SOURCE);
                recalld::room_turns::register_blocks(&meaning, &ingest, &room_dir)
            })
            .await;
            match done {
                Ok(Ok(0)) => {}
                Ok(Ok(added)) => tracing::info!(added, "room: blocks registered for playback"),
                Ok(Err(err)) => tracing::warn!(%err, "room register: pass failed"),
                Err(err) => tracing::error!(%err, "room register: task failed"),
            }
            tokio::time::sleep(EVERY).await;
        }
    });
}

fn spawn_room_builder(root: PathBuf) {
    const IDLE: std::time::Duration = std::time::Duration::from_mins(1);
    tokio::spawn(async move {
        loop {
            let pass_root = root.clone();
            let config = recalld::room::RoomConfig::default();
            let built = tokio::task::spawn_blocking(move || {
                recalld::room::build_once(&pass_root, &config, chrono::Utc::now())
            })
            .await;
            match built {
                Ok(Ok(summary)) if summary.built + summary.silent > 0 => {
                    tracing::info!(
                        built = summary.built,
                        silent = summary.silent,
                        deferred = summary.deferred,
                        "room: pass complete"
                    );
                }
                Ok(Ok(_)) => tokio::time::sleep(IDLE).await,
                Ok(Err(err)) => {
                    tracing::warn!(%err, "room: pass failed; backing off");
                    tokio::time::sleep(IDLE).await;
                }
                Err(err) => {
                    tracing::error!(%err, "room: task failed; backing off");
                    tokio::time::sleep(IDLE).await;
                }
            }
        }
    });
}
