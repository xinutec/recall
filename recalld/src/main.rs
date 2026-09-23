//! recalld — see lib.rs and docs/architecture.md.
//!
//!   recalld --root <data-root> [--bind <addr:port>]... [--tokens <file>]
//!           [--frontend <dir>]
//!
//! `--bind` repeats: the fleet gives recalld both the ingest port the recorders
//! push to and the port the browser uses. `--frontend` is the built Angular app.
//!
//! `RECALLD_READ_TOKEN` (env, optional) gates the read side; per-source write
//! tokens come from `--tokens <file>` or the `RECALLD_INGEST_TOKENS` env var
//! (same line grammar). Everything unset = open, for dev and tests.
//!
//! `RECALL_SYNC_TOKEN` (env, optional) decides whether the `/sync/*` routes are
//! mounted at all.

use recalld::app::{Config, DEFAULT_MAX_BODY, router};
use recalld::tokens::Tokens;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

fn usage() -> ExitCode {
    eprintln!(
        "usage: recalld --root <data-root> [--bind <addr:port>]... [--tokens <file>] \
         [--frontend <dir>]"
    );
    ExitCode::FAILURE
}

/// Everything the command line says, or `None` if it does not parse.
struct Args {
    root: PathBuf,
    binds: Vec<String>,
    tokens_path: Option<PathBuf>,
    frontend: Option<PathBuf>,
}

fn parse_args() -> Option<Args> {
    let mut args = std::env::args().skip(1);
    let mut root: Option<PathBuf> = None;
    let mut binds: Vec<String> = Vec::new();
    let mut tokens_path: Option<PathBuf> = None;
    let mut frontend: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        let value = args.next()?;
        match arg.as_str() {
            "--root" => root = Some(PathBuf::from(value)),
            "--bind" => binds.push(value),
            "--tokens" => tokens_path = Some(PathBuf::from(value)),
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
        frontend,
    })
}

/// Serve one bound listener with connect info. The capture-control plane carries
/// no credential, so the peer address is the only identity its audit records;
/// without it every pause is recorded against `unknown-host`.
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

/// Open both planes and bring the meaning schema up to date, or say what is wrong
/// in a line a human can act on. Failing to start is deliberate: every read route
/// assumes those tables exist.
fn prepare_planes(root: &std::path::Path) -> Result<(), String> {
    recalld::store::open(root)
        .map_err(|err| format!("cannot open {}/ingest.sqlite: {err}", root.display()))?;
    let conn = recalld::work::open_write(root)
        .map_err(|err| format!("cannot open {}/recall.sqlite: {err}", root.display()))?;
    recalld::meaning_schema::ensure(&conn)
        .map_err(|err| format!("cannot migrate {}/recall.sqlite: {err}", root.display()))
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
    // The Mac→fleet sync plane; unset means the routes are not mounted.
    let sync_token = std::env::var("RECALL_SYNC_TOKEN")
        .ok()
        .filter(|t| !t.is_empty());
    if let Err(complaint) = prepare_planes(&root) {
        eprintln!("recalld: {complaint}");
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

/// Bind every address before serving any: a half-bound daemon looks healthy from
/// whichever side you check, so refusing to start is better.
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

/// Measure levels for every delivered segment in bounded batches. Its ffmpeg
/// children and sqlite writes run under `spawn_blocking`, and WAL keeps it from
/// blocking an upload.
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

/// Re-derive stored speaker guesses when the voiceprint corpus has grown.
///
/// This rewrites the record, so it runs in bounded batches and logs every batch
/// that changed something. The work-list refills only when a voice is enrolled,
/// so the idle sleep is the usual state.
fn spawn_rematcher(root: PathBuf) {
    const BATCH: usize = 200;
    const IDLE: std::time::Duration = std::time::Duration::from_mins(5);
    const BACKOFF: std::time::Duration = std::time::Duration::from_mins(5);
    tokio::spawn(async move {
        loop {
            let batch_root = root.clone();
            let done = tokio::task::spawn_blocking(move || {
                let mut conn = recalld::work::open_write(&batch_root)?;
                let now = audiocore::instant::python_isoformat_utc(chrono::Utc::now());
                recalld::rematch::run_once(&mut conn, BATCH, &now)
            })
            .await;
            match done {
                Ok(Ok(pass)) if pass.examined == 0 => tokio::time::sleep(IDLE).await,
                Ok(Ok(pass)) => tracing::info!(
                    examined = pass.examined,
                    rewritten = pass.rewritten,
                    unchanged = pass.unchanged,
                    unmatched = pass.unmatched,
                    "rematch: batch complete"
                ),
                Ok(Err(err)) => {
                    tracing::warn!(%err, "rematch: pass failed; backing off");
                    tokio::time::sleep(BACKOFF).await;
                }
                Err(err) => {
                    tracing::error!(%err, "rematch: task failed; backing off");
                    tokio::time::sleep(BACKOFF).await;
                }
            }
        }
    });
}

/// Measure coverage for room blocks that have none recorded. Once every block is
/// measured, each pass finds nothing and sleeps.
fn spawn_coverage_backfill(root: PathBuf) {
    const BATCH: usize = 50;
    const IDLE: std::time::Duration = std::time::Duration::from_mins(30);
    tokio::spawn(async move {
        loop {
            let batch_root = root.clone();
            let done = tokio::task::spawn_blocking(move || {
                recalld::room::backfill_coverage(&batch_root, BATCH)
            })
            .await;
            match done {
                Ok(Ok(0)) => tokio::time::sleep(IDLE).await,
                Ok(Ok(n)) => tracing::info!(measured = n, "room: coverage backfilled"),
                Ok(Err(err)) => {
                    tracing::warn!(%err, "room: coverage backfill failed; backing off");
                    tokio::time::sleep(IDLE).await;
                }
                Err(err) => {
                    tracing::error!(%err, "room: coverage task failed; backing off");
                    tokio::time::sleep(IDLE).await;
                }
            }
        }
    });
}

/// Start every background pass the daemon runs. Which passes are on is the most
/// important fact about a deployed recalld, so the list lives on one screen with
/// the reasons beside it.
fn spawn_background_passes(root: &std::path::Path) {
    let root = root.to_path_buf();
    let root = &root;
    spawn_level_scanner(root.clone());
    spawn_speech_scanner(root.clone());
    spawn_rematcher(root.clone());
    spawn_coverage_backfill(root.clone());
    spawn_room_builder(root.clone());
    spawn_room_registrar(root.clone());
    // OFF. Room turns read worse than the per-mic turns they hid, mostly
    // English where the microphones heard Dutch. The cause is one language
    // label per clip, which fails a microphone's clip the same way. Re-enable
    // once language is decided per piece, and the room stream is shown to beat
    // per-mic on spontaneous speech.
    // spawn_turn_writer(root.clone(), recalld::turns::ROOM);
    //
    // The per-mic stream only fills clips that have no turns. It hides no
    // per-mic turn (only live guesses on the same span), so its worst case is a
    // transcript where there was silence, deletable by its provenance. Idle
    // until a runner leases `transcribe-segment`.
    spawn_turn_writer(root.clone(), recalld::turns::PER_MIC);
    //
    // The only running loop that replaces a transcript somebody reads, so it
    // must stay the only such writer: two writers each hiding what the other
    // wrote leave a corpus nobody can reason about. `diarized::ROOM` stays off;
    // the reason is on the constant.
    spawn_diarized_writer(root.clone(), recalld::diarized::PER_MIC);
    spawn_segment_registrar(root.clone());
    spawn_segment_deriver(root.clone());
    spawn_enroller(root.clone());
}

/// Turn human-named turns into reference voiceprints.
///
/// Deliberately slow: more prints barely move attribution, so this only has to
/// keep up with new labels (a handful a week) and must not outbid diarization
/// for the one GPU. Deriving and writing share one loop because nothing else
/// enrols.
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
                let pass = recalld::enrol::write_pass(
                    &meaning,
                    &ingest,
                    &audiocore::instant::python_isoformat_utc(now),
                    WRITE,
                )?;
                Ok::<_, rusqlite::Error>((queued, pass))
            })
            .await;
            match done {
                // `stale` is in the guard: a pass that only finds spans no
                // longer wanted writes and queues nothing, and must still log.
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

/// Turn finished diarize results into speaker-split turns.
///
/// The only loop that replaces a transcript: it hides turns somebody can read
/// and writes new ones over them. `diarized::decide` makes the whole decision on
/// data before a row is touched, and either replaces or keeps. Idle until a
/// runner leases the stream's diarize kind.
///
/// To reverse it, both planes (shown for [`recalld::diarized::PER_MIC`]; the room
/// stream's strings say `room runner` and its kind is `diarize-room`):
///
/// ```sql
/// -- recall.sqlite: un-hide first, then delete. Deleting first makes the
/// -- blocks eligible again while the originals are still hidden.
/// UPDATE transcript_segments SET hidden_reason = NULL
///  WHERE hidden_reason = 'diarized (per-mic runner)';
/// DELETE FROM transcript_segments
///  WHERE provenance = 'diarized-aligned (per-mic runner)';
///
/// -- ingest.sqlite: declined blocks wrote no rows, only a ledger entry.
/// -- Without this they stay decided for ever.
/// DELETE FROM pass_ledger WHERE kind = 'diarize-segment';
/// ```
///
/// ⚠ Use exact equality, not `LIKE 'diarized-aligned (%'`: that pattern also
/// matches the archive's older diarized corpus, and would delete it too.
///
/// A small batch on a slow cadence, so a bad verdict is noticed while it covers
/// dozens of blocks rather than hundreds.
fn spawn_diarized_writer(root: PathBuf, stream: recalld::diarized::Stream<'static>) {
    const EVERY: std::time::Duration = std::time::Duration::from_mins(2);
    const BATCH: usize = 20;
    tokio::spawn(async move {
        loop {
            let pass_root = root.clone();
            let done = tokio::task::spawn_blocking(move || {
                let ingest = recalld::store::open(&pass_root)?;
                let mut meaning = recalld::work::open_write(&pass_root)?;
                let now = audiocore::instant::python_isoformat_utc(chrono::Utc::now());
                recalld::diarized::write_pass(&mut meaning, &ingest, &stream, &now, BATCH)
            })
            .await;
            match done {
                // `kept` is in the guard: a pass that declines every block is
                // the one most worth seeing.
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

/// Run VAD over every delivered segment, so "active" can mean someone is
/// talking rather than that bytes arrived, and the quiet review has evidence
/// before it proposes deleting anything.
///
/// A smaller batch and longer idle than the level scanner: this runs a neural
/// network per 32 ms window on four cores shared with Nextcloud, and speech
/// evidence is wanted within minutes, not seconds.
fn spawn_speech_scanner(root: PathBuf) {
    // ⚠ Do not start where the model cannot run. isis and amun are Ivy Bridge
    // and ort's prebuilt runtime needs AVX2: calling it there raises SIGILL and
    // kills the daemon. Refuse once, loudly.
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

/// Turn stored transcription results into turns people read.
///
/// One function for both streams; they differ only in their
/// [`recalld::turns::Stream`]:
///
/// - [`recalld::turns::ROOM`] writes room turns and hides the per-mic turns they
///   cover. Off.
/// - [`recalld::turns::PER_MIC`] fills gaps only: a clip that already carries
///   turns is refused before a row is touched. It hides the live guesses on the
///   span it writes, as `live-reconciled`.
///
/// To reverse either, both planes. In `recall.sqlite` the provenance names the
/// stream's rows alone, and deleting them makes those clips eligible again:
///
/// ```sql
/// UPDATE transcript_segments SET hidden_reason = NULL
///  WHERE hidden_reason = 'covered by the room stream';
/// DELETE FROM transcript_segments WHERE provenance = 'room';
/// -- or, for the per-mic stream:
/// DELETE FROM transcript_segments WHERE provenance = 'per-mic (runner)';
/// ```
///
/// ⚠ Clips that wrote nothing are held only in the `ingest.sqlite` ledger, under
/// the stream's kind. Skip this and the reversal looks complete while every
/// refused or swept clip is never reconsidered:
///
/// ```sql
/// DELETE FROM pass_ledger WHERE kind = 'transcribe-room';  -- or 'transcribe-segment'
/// ```
///
/// A small batch on a slow cadence, so a bad verdict is noticed while it covers
/// dozens of clips rather than hundreds.
fn spawn_turn_writer(root: PathBuf, stream: recalld::turns::Stream<'static>) {
    const EVERY: std::time::Duration = std::time::Duration::from_mins(2);
    const BATCH: usize = 20;
    tokio::spawn(async move {
        loop {
            let pass_root = root.clone();
            let done = tokio::task::spawn_blocking(move || {
                let ingest = recalld::store::open(&pass_root)?;
                let mut meaning = recalld::work::open_write(&pass_root)?;
                let now = audiocore::instant::python_isoformat_utc(chrono::Utc::now());
                recalld::turns::write_pass(&mut meaning, &ingest, &stream, &now, BATCH)
            })
            .await;
            match done {
                // `swept` is in the guard: a clip whose turns are all repetition
                // loops writes, hides and refuses nothing, and that pass is the
                // one saying the audio is bad.
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
/// A timer rather than a step in `queue::lease`: a lease is a frequent request,
/// and deriving spans both planes and scans the ingest one. The room's
/// `derive_jobs` is in `lease` because it is one indexed statement.
///
/// The batch bound is the throttle: queuing the whole backlog at once would hand
/// a runner days of work the moment it learned the kind.
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

/// Register microphone clips in the meaning plane, so their turns have audio. A
/// clip with no `audio_segments` row goes barren at the write step however often
/// it is transcribed.
///
/// Bounded because it decodes: `upload::probe` reads the whole file, competing
/// with the room builder and with capture.
///
/// To reverse it (registration hides nothing, it only makes clips eligible),
/// both planes:
///
/// ```sql
/// DELETE FROM audio_segments WHERE path LIKE '%/ingest/%'
///   AND id NOT IN (SELECT audio_segment_id FROM transcript_segments
///                  WHERE audio_segment_id IS NOT NULL);
/// -- and in ingest.sqlite:
/// DELETE FROM pass_ledger WHERE kind = 'register-segment';
/// ```
///
/// Keep the `NOT IN`: deleting a row a turn hangs from leaves text whose audio no
/// longer resolves.
fn spawn_segment_registrar(root: PathBuf) {
    const EVERY: std::time::Duration = std::time::Duration::from_mins(5);
    const BATCH: usize = 40;
    tokio::spawn(async move {
        loop {
            let pass_root = root.clone();
            let done = tokio::task::spawn_blocking(move || {
                let ingest = recalld::store::open(&pass_root)?;
                let meaning = recalld::work::open_write(&pass_root)?;
                let now = audiocore::instant::python_isoformat_utc(chrono::Utc::now());
                recalld::turns::register_segments(&meaning, &ingest, &pass_root, &now, BATCH)
            })
            .await;
            match done {
                // `waiting` is not in the guard: an unknown recorder makes it a
                // large constant, and a line every five minutes would drown the
                // log. `covered` is, and `retired` is not: a covered clip cost a
                // full decode to discover, a retired one a hash lookup, so a
                // rising `covered` is how repeated decoding shows up.
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
/// Its own loop because it spans both planes, and the builder touches only
/// `ingest.sqlite`. Idempotent: the first pass backfills every block ever built,
/// and a pass that inserts nothing is the normal case.
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

/// Build settled room blocks, recording terminal verdicts only. Chases the level
/// scanner: a block whose evidence is incomplete defers to the next pass.
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
