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
        spawn_background_passes(&config.root);
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

/// Start every background pass the daemon runs.
///
/// ⚠ **Extracted so the LIST is readable, not merely so `main` is short.** These
/// loops are what recalld does when nobody is asking it anything — scan, build,
/// register, derive, write — and WHICH OF THEM ARE ON is the single most
/// load-bearing fact about a deployed recalld. That belongs on one screen, with
/// the reasons beside it.
fn spawn_background_passes(root: &std::path::Path) {
    let root = root.to_path_buf();
    let root = &root;
    spawn_level_scanner(root.clone());
    spawn_speech_scanner(root.clone());
    spawn_room_builder(root.clone());
    spawn_room_registrar(root.clone());
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
    // spawn_turn_writer(root.clone(), recalld::turns::ROOM);
    //
    // The PER-MIC stream is a different decision and is not gated on that
    // one. It writes turns for microphone clips that have none — 14,078 of
    // 22,312 of them when this landed — and it neither hides nor supersedes
    // anything, so the worst case is a transcript where there was silence,
    // deletable by its provenance. It is the last thing between `worker.py`
    // and deletion (#1538).
    //
    // ⚠ Nothing feeds it until the runner leases `transcribe-segment`, which
    // is the separate switch: deriving jobs costs nothing, leasing them
    // spends GPU that the Mac's own worker is still spending on the same
    // clips. Turning both on at once is how the same minute gets transcribed
    // twice.
    spawn_turn_writer(root.clone(), recalld::turns::PER_MIC);
    //
    // ⚠ **THE ONLY LOOP HERE THAT REPLACES A TRANSCRIPT SOMEBODY READS**, and it
    // must never run beside `recall refine`, which wrote these same clips until
    // that agent was removed in the same change. Two writers over one corpus,
    // each HIDING what the other wrote, is a corpus nobody can reason about.
    //
    // (`diarized::ROOM` is the other stream and stays off; the reason is on the
    // constant.)
    spawn_diarized_writer(root.clone(), recalld::diarized::PER_MIC);
    spawn_segment_registrar(root.clone());
    spawn_segment_deriver(root.clone());
    spawn_enroller(root.clone());
}

/// Stage E4's last loop: turn a human-named turn into a reference voiceprint.
///
/// ⚠ **DELIBERATELY SLOW, and the reason is measured.** Adding 222 prints to the
/// fleet's 750 moved attribution +0.19 points, and 187 prints score within 1.3 of
/// 972 (#1648) — the corpus saturated long ago. So this exists to retire the
/// Mac's last Python loop, not to raise a number, and it must not outbid
/// diarization for the one GPU: a small batch on a slow cadence keeps up with new
/// labels, which arrive a handful a week, and lets the backlog trickle.
///
/// ⚠ Derivation and consumption are ONE switch here, unlike the transcription
/// port. Nothing else enrols — the Python that did was deleted with `refine` —
/// so there is no second writer to collide with, and a derived job nobody leases
/// would just be the diarize-room queue's mistake again.
fn spawn_enroller(root: PathBuf) {
    const EVERY: std::time::Duration = std::time::Duration::from_mins(10);
    const DERIVE: usize = 5;
    const WRITE: usize = 20;
    tokio::spawn(async move {
        loop {
            let pass_root = root.clone();
            let done = tokio::task::spawn_blocking(move || {
                let ingest = recalld::store::open(&pass_root)?;
                let meaning = recalld::work::open_write(&pass_root)?;
                let now = chrono::Utc::now();
                let queued = recalld::enrol::derive_jobs(&ingest, &meaning, now, DERIVE)?;
                let pass = recalld::enrol::write_pass(&meaning, &ingest, &now.to_rfc3339(), WRITE)?;
                Ok::<_, rusqlite::Error>((queued, pass))
            })
            .await;
            match done {
                // ⚠ `stale` is IN this guard. A pass that decides every clip's
                // spans are no longer wanted writes nothing and queues nothing,
                // and without it the one pass worth reading logs no line at all.
                Ok(Ok((queued, pass))) if queued + pass.prints + pass.stale > 0 => {
                    tracing::info!(
                        queued,
                        clips = pass.clips,
                        prints = pass.prints,
                        stale = pass.stale,
                        "enrol: pass"
                    );
                }
                Ok(Ok(_)) => {}
                Ok(Err(err)) => tracing::warn!(%err, "enrol: pass failed"),
                Err(err) => tracing::error!(%err, "enrol: task failed"),
            }
            tokio::time::sleep(EVERY).await;
        }
    });
}

/// Stage E4: turn finished `diarize-room` results into speaker-split turns.
///
/// ⚠ **THE ONLY LOOP IN THIS DAEMON THAT REPLACES A TRANSCRIPT.** Everything
/// else derives, registers, or fills a gap; this hides turns somebody can read
/// and writes over them. `refine.py` — which it replaces — blanked 132 segments
/// of real household conversation doing exactly this, by applying its filters
/// AFTER hiding. `diarized::decide` is where that cannot happen: the whole
/// decision is made on data before a row is touched, and it either replaces or
/// keeps.
///
/// ⚠ **Nothing feeds it until a `voices` runner is deployed.** A diarize job is
/// derived for every transcribed block, but until something leases them there
/// are no results and this loop does nothing every two minutes. That is the
/// intended resting state, and the switch is on the Mac, not here.
///
/// ⚠ **HOW TO PUT IT BACK — TWO PLANES, and the hides are the half that matters.**
///
/// ```sql
/// -- meaning plane (recall.sqlite): restore what the pass covered, then drop
/// -- what it wrote. In this order: the second statement is what makes the
/// -- blocks eligible again, and doing it first leaves the originals hidden
/// -- while the pass re-runs.
/// UPDATE transcript_segments SET hidden_reason = NULL
///  WHERE hidden_reason = 'diarized (mlx-whisper/large-v3-turbo (room))';
/// DELETE FROM transcript_segments
///  WHERE provenance = 'diarized-aligned (mlx-whisper/large-v3-turbo (room))';
///
/// -- ingest plane (ingest.sqlite): the blocks it DECLINED wrote no rows, so
/// -- only the ledger holds them. Forget this and the reversal looks complete
/// -- while every refused block stays decided for ever.
/// DELETE FROM pass_ledger WHERE kind = 'diarize-room';
/// ```
///
/// ⚠ **EXACT equality, NOT `LIKE 'diarized-aligned (%'`.** That pattern also
/// matches `diarized-aligned (mlx-community/whisper-large-v3-turbo)` —
/// `refine.py`'s per-mic output, tens of thousands of rows of it. A reversal
/// written with a wildcard deletes this pass's turns and the archive's real
/// diarized corpus together. The model name is IN the provenance precisely so a
/// reversal can name ONE pass; a wildcard throws that away.
///
/// A SMALL batch on a slow cadence, for the reason the turn writer has one: the
/// queue drains over hours, so a bad verdict is noticed while it is dozens of
/// blocks rather than nine hundred.
fn spawn_diarized_writer(root: PathBuf, stream: recalld::diarized::Stream<'static>) {
    const EVERY: std::time::Duration = std::time::Duration::from_mins(2);
    const BATCH: usize = 20;
    tokio::spawn(async move {
        loop {
            let pass_root = root.clone();
            let done = tokio::task::spawn_blocking(move || {
                let ingest = recalld::store::open(&pass_root)?;
                let mut meaning = recalld::work::open_write(&pass_root)?;
                let now = chrono::Utc::now().to_rfc3339();
                recalld::diarized::write_pass(&mut meaning, &ingest, &stream, &now, BATCH)
            })
            .await;
            match done {
                // ⚠ `kept` is inside the guard, like `swept` next door: a pass
                // that declines every block is the one most worth seeing, and
                // without it the interesting case is the silent one.
                Ok(Ok(pass)) if pass.turns + pass.hidden + pass.kept > 0 => {
                    tracing::info!(
                        blocks = pass.blocks,
                        turns = pass.turns,
                        hidden = pass.hidden,
                        kept = pass.kept,
                        waiting = pass.waiting,
                        stream = stream.diarize_kind,
                        "diarized: written"
                    );
                }
                Ok(Ok(_)) => {}
                Ok(Err(err)) => {
                    tracing::warn!(%err, stream = stream.diarize_kind, "diarized: pass failed");
                }
                Err(err) => {
                    tracing::error!(%err, stream = stream.diarize_kind, "diarized: task failed");
                }
            }
            tokio::time::sleep(EVERY).await;
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
/// Stage E3a: turn stored transcription results into turns people actually read.
///
/// ⚠ **THE FIRST LOOP HERE THAT CHANGES A TRANSCRIPT SOMEBODY READS.** Everything
/// above it derives, measures or registers.
///
/// One function, two callers, because the difference between the streams is a
/// [`recalld::turns::Stream`] and not a loop:
///
/// - [`recalld::turns::ROOM`] — writes room turns AND HIDES the per-mic turns
///   they cover, thousands of rows at a time. **Off** pending #1461.
/// - [`recalld::turns::PER_MIC`] — fills gaps only. It hides nothing and
///   revisits nothing: a clip that already carries turns is refused before a
///   row is touched.
///
/// ⚠ **HOW TO PUT EITHER BACK. It is TWO PLANES, and one of them is easy to
/// miss.** Hiding is not deleting, so the meaning plane (`recall.sqlite`) undoes
/// cleanly — the provenance string is the stream's, and names its rows alone:
///
/// ```sql
/// UPDATE transcript_segments SET hidden_reason = NULL
///  WHERE hidden_reason = 'covered by the room stream';
/// DELETE FROM transcript_segments WHERE provenance = 'room';
/// -- or, for the per-mic stream:
/// DELETE FROM transcript_segments WHERE provenance = 'per-mic (runner)';
/// ```
///
/// That restores what anybody reads, and by itself it re-enables every clip
/// whose turns it just deleted — `write_pass` derives "already written" from
/// those very rows, deliberately, so this much needs no bookkeeping.
///
/// ⚠ But the clips that wrote NOTHING left no rows to delete, so they are held
/// in a ledger in the INGEST plane (`ingest.sqlite`) instead, and it has to go
/// too or they stay decided:
///
/// ```sql
/// DELETE FROM pass_ledger WHERE kind = 'transcribe-room';
/// ```
///
/// Forget it and the reversal LOOKS complete — the transcripts are back, the
/// stream's rows are gone — while every refused or swept clip silently never
/// gets reconsidered. `turns::ensure_ledger` carries the same warning from the
/// other side.
///
/// Written here rather than in a task because the person who needs it will be
/// reading this file, not searching for the note.
///
/// A SMALL batch on a slow cadence, deliberately: the queue drains over hours
/// instead of minutes, so a bad verdict is noticed while it is dozens of clips
/// rather than nine hundred.
fn spawn_turn_writer(root: PathBuf, stream: recalld::turns::Stream<'static>) {
    const EVERY: std::time::Duration = std::time::Duration::from_mins(2);
    const BATCH: usize = 20;
    tokio::spawn(async move {
        loop {
            let pass_root = root.clone();
            let done = tokio::task::spawn_blocking(move || {
                let ingest = recalld::store::open(&pass_root)?;
                let mut meaning = recalld::work::open_write(&pass_root)?;
                let now = chrono::Utc::now().to_rfc3339();
                recalld::turns::write_pass(&mut meaning, &ingest, &stream, &now, BATCH)
            })
            .await;
            match done {
                // ⚠ `swept` is IN this guard, and leaving it out is how the
                // interesting case goes quiet: a clip whose turns are all
                // repetition loops writes nothing, hides nothing and refuses
                // nothing, so without it the one pass that says the audio is bad
                // is the one pass that logs no line at all.
                Ok(Ok(pass)) if pass.turns + pass.hidden + pass.refused + pass.swept > 0 => {
                    tracing::info!(
                        stream = stream.provenance,
                        blocks = pass.blocks,
                        turns = pass.turns,
                        hidden = pass.hidden,
                        refused = pass.refused,
                        swept = pass.swept,
                        barren = pass.barren,
                        "turns: written"
                    );
                }
                Ok(Ok(_)) => {}
                Ok(Err(err)) => {
                    tracing::warn!(%err, stream = stream.provenance, "turns: pass failed");
                }
                Err(err) => tracing::error!(%err, stream = stream.provenance, "turns: task failed"),
            }
            tokio::time::sleep(EVERY).await;
        }
    });
}

/// Derive `transcribe-segment` jobs for microphone clips that have no turns.
///
/// Its own loop rather than a step inside `queue::lease`, and the reason is what
/// each costs. A lease is a REQUEST — the runner asks every 20 seconds — and
/// deriving spans both planes and scans the ingest one; paying that per request
/// would put a scan on the hot path to save a timer. `derive_jobs` (room) is in
/// `lease` because it is one indexed statement against one database.
///
/// ⚠ Deriving is free; LEASING is what spends. A queued job is a row until a
/// runner asks for its kind.
///
/// ⚠ BOUNDED, and that bound is the throttle: queuing the whole backlog in one
/// statement would hand a runner days of work the moment it learned the kind.
fn spawn_segment_deriver(root: PathBuf) {
    const EVERY: std::time::Duration = std::time::Duration::from_mins(10);
    const BATCH: usize = 50;
    tokio::spawn(async move {
        loop {
            let pass_root = root.clone();
            let done = tokio::task::spawn_blocking(move || {
                let ingest = recalld::store::open(&pass_root)?;
                let meaning = recalld::work::open_write(&pass_root)?;
                recalld::queue::derive_segment_jobs(&ingest, &meaning, chrono::Utc::now(), BATCH)
            })
            .await;
            match done {
                Ok(Ok(0)) => {}
                Ok(Ok(queued)) => tracing::info!(queued, "segments: transcribe jobs derived"),
                Ok(Err(err)) => tracing::warn!(%err, "segment derive: pass failed"),
                Err(err) => tracing::error!(%err, "segment derive: task failed"),
            }
            tokio::time::sleep(EVERY).await;
        }
    });
}

/// Register MICROPHONE clips in the meaning plane, so their turns have audio.
///
/// The room registrar's sibling. A clip with no `audio_segments` row can only
/// ever go barren at the write step, however often it is transcribed.
///
/// ⚠ Bounded tighter than the room's, because this one DECODES: `upload::probe`
/// reads the whole file, competing with the room builder and with capture.
///
/// ⚠ **HOW TO PUT IT BACK.** Registration alone plays no turn and hides
/// nothing; what it changes is that a clip becomes ELIGIBLE. Two planes:
///
/// ```sql
/// DELETE FROM audio_segments WHERE path LIKE '%/ingest/%'
///   AND id NOT IN (SELECT audio_segment_id FROM transcript_segments
///                  WHERE audio_segment_id IS NOT NULL);
/// -- and in ingest.sqlite:
/// DELETE FROM pass_ledger WHERE kind = 'register-segment';
/// ```
///
/// The `NOT IN` is the whole of it: a row a turn already hangs from must not go,
/// or the text stays and the audio behind it stops resolving.
fn spawn_segment_registrar(root: PathBuf) {
    const EVERY: std::time::Duration = std::time::Duration::from_mins(5);
    const BATCH: usize = 40;
    tokio::spawn(async move {
        loop {
            let pass_root = root.clone();
            let done = tokio::task::spawn_blocking(move || {
                let ingest = recalld::store::open(&pass_root)?;
                let meaning = recalld::work::open_write(&pass_root)?;
                let now = chrono::Utc::now().to_rfc3339();
                recalld::turns::register_segments(&meaning, &ingest, &pass_root, &now, BATCH)
            })
            .await;
            match done {
                // `waiting` is NOT in this guard. Every microphone is known
                // today so it is always zero; if a seventh recorder ever
                // appears it becomes a large constant number, and a line every
                // five minutes saying so would drown the log rather than inform
                // it. The count is there for whoever goes looking.
                //
                // ⚠ `covered` IS in it, and `retired` is not. A covered clip
                // cost a full decode to discover, so it is worth a line; a
                // retired one cost a hash lookup. The distinction matters
                // because `covered` is the counter that would have shown this
                // pass re-decoding 1,599 clips a day, and nothing did.
                Ok(Ok(pass)) if pass.added + pass.unreadable + pass.covered > 0 => {
                    tracing::info!(
                        added = pass.added,
                        covered = pass.covered,
                        retired = pass.retired,
                        probed = pass.probed,
                        unreadable = pass.unreadable,
                        waiting = pass.waiting,
                        "segments: registered for playback"
                    );
                }
                Ok(Ok(_)) => {}
                Ok(Err(err)) => tracing::warn!(%err, "segment register: pass failed"),
                Err(err) => tracing::error!(%err, "segment register: task failed"),
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
                recalld::turns::register_blocks(&meaning, &ingest, &room_dir)
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
