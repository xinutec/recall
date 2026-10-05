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
//!
//! `RECALLD_TRUSTED_PROXIES` (env, optional, comma-separated) names the peers
//! whose `X-Real-IP` the capture audit records instead of their own address.

use clap::Parser;
use recalld::app::{Config, DEFAULT_MAX_BODY, router};
use recalld::tokens::Tokens;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

/// The recall server: the archive, its API and the web app.
#[derive(Parser)]
#[command(name = "recalld")]
struct Args {
    /// The data root.
    #[arg(long, value_name = "DATA_ROOT")]
    root: PathBuf,
    /// An address to serve on; repeatable.
    #[arg(
        long = "bind",
        value_name = "ADDR:PORT",
        default_value = "127.0.0.1:8001"
    )]
    binds: Vec<String>,
    /// The ingest token table [default: `RECALLD_INGEST_TOKENS`].
    #[arg(long = "tokens", value_name = "FILE")]
    tokens_path: Option<PathBuf>,
    /// The built web app to serve.
    #[arg(long, value_name = "DIR")]
    frontend: Option<PathBuf>,
    /// A one-off task to run instead of serving.
    #[command(subcommand)]
    task: Option<Task>,
}

#[derive(Clone, Copy, clap::Subcommand)]
enum Task {
    /// Give every stored file a clip and say whether all have one (#1911).
    ClipCensus,
    /// Recover the edit log from today's tables and account for every row (#1912).
    EditCensus,
    /// Render every clip with a stored result and compare with today's lines (#1916).
    ShadowDiff,
    /// One clip, today's lines beside rendered ones (holds transcript text).
    ShadowClip {
        #[arg(long)]
        id: i64,
    },
    /// Queue a transcription for each shown clip with no stored result,
    /// leaving its lines as they are (`recalld::backfill`).
    BackfillResults {
        /// Queue; without this, only say what would be queued.
        #[arg(long)]
        apply: bool,
    },
    /// Store the phones' WAV copies as FLAC (`recalld::phone_flac`).
    PhoneFlac {
        /// Convert; without this, only say what would be converted.
        #[arg(long)]
        apply: bool,
        /// Leave copies received within this many hours, which a phone may
        /// still be sending again.
        #[arg(long, default_value_t = 24)]
        settled_hours: i64,
    },
}

/// Open both planes as the server does (which gives every stored file a clip),
/// run `report` over them, and print what it returns as JSON.
fn report<T: serde::Serialize, E: std::fmt::Display>(
    root: &std::path::Path,
    report: impl FnOnce(&rusqlite::Connection, &rusqlite::Connection) -> Result<T, E>,
) -> ExitCode {
    if let Err(complaint) = prepare_planes(root) {
        eprintln!("recalld: {complaint}");
        return ExitCode::FAILURE;
    }
    let done = recalld::work::open_write(root)
        .map_err(|err| err.to_string())
        .and_then(|meaning| {
            let ingest = recalld::store::open(root).map_err(|err| err.to_string())?;
            report(&meaning, &ingest).map_err(|err| err.to_string())
        })
        .and_then(|value| serde_json::to_string_pretty(&value).map_err(|err| err.to_string()));
    match done {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("recalld: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Run `task` against the planes under `root`, printing what it did as JSON.
fn run_task(root: &std::path::Path, task: Task) -> ExitCode {
    let (apply, settled_hours) = match task {
        Task::PhoneFlac {
            apply,
            settled_hours,
        } => (apply, settled_hours),
        Task::ClipCensus => {
            return report(root, |meaning, ingest| {
                recalld::clips::census(meaning, ingest, root)
            });
        }
        Task::EditCensus => {
            return report(root, |meaning, ingest| {
                recalld::legacy_edits::census(meaning, ingest, root)
            });
        }
        Task::ShadowDiff => {
            return report(root, |meaning, ingest| {
                recalld::shadow::run(meaning, ingest, root)
            });
        }
        Task::BackfillResults { apply } => {
            return report(root, |meaning, ingest| {
                ingest.busy_timeout(std::time::Duration::from_mins(2))?;
                recalld::backfill::queue(meaning, ingest, root, apply)
            });
        }
        Task::ShadowClip { id } => {
            return report(root, |meaning, ingest| {
                recalld::shadow::detail(meaning, ingest, root, id)
            });
        }
    };
    let before = (chrono::Utc::now() - chrono::Duration::hours(settled_hours))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let done = recalld::work::open_write(root).and_then(|meaning| {
        let mut ingest = recalld::store::open(root)?;
        // The daemon's passes hold the ingest lock past the 5 s default; a task
        // that gives up stops halfway.
        ingest.busy_timeout(std::time::Duration::from_mins(2))?;
        recalld::phone_flac::convert(root, &meaning, &mut ingest, &before, apply)
    });
    match done.map(|done| serde_json::to_string_pretty(&done)) {
        Ok(Ok(json)) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Ok(Err(err)) => {
            eprintln!("recalld: {err}");
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("recalld: {err}");
            ExitCode::FAILURE
        }
    }
}

/// `RECALLD_TRUSTED_PROXIES`. `None` on a typo, failing startup: a silently
/// empty list would name every pause after the node.
fn trusted_proxies() -> Option<Vec<std::net::IpAddr>> {
    let Ok(text) = std::env::var("RECALLD_TRUSTED_PROXIES") else {
        return Some(Vec::new());
    };
    match text
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::parse)
        .collect()
    {
        Ok(list) => Some(list),
        Err(err) => {
            eprintln!("recalld: RECALLD_TRUSTED_PROXIES does not parse: {err}");
            None
        }
    }
}

/// Serve one listener with connect info: the peer address is the only identity
/// the capture audit can record.
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

/// Keep every request answered with a 500 beside the app's own client log,
/// where the doctor counts them (`GET /sync/record/health`).
fn keep_faults(root: &std::path::Path) -> Result<(), String> {
    let faults = recalld::record_health::fault_log(root);
    if let Some(logs) = faults.parent() {
        std::fs::create_dir_all(logs)
            .map_err(|err| format!("cannot create {}: {err}", logs.display()))?;
    }
    recalld::route::keep_faults_in(faults);
    Ok(())
}

/// Open both planes and bring the meaning schema up to date, or say what is
/// wrong. Every read route assumes those tables exist.
fn prepare_planes(root: &std::path::Path) -> Result<(), String> {
    recalld::store::open(root)
        .map_err(|err| format!("cannot open {}/ingest.sqlite: {err}", root.display()))?;
    let conn = recalld::work::open_write(root)
        .map_err(|err| format!("cannot open {}/recall.sqlite: {err}", root.display()))?;
    recalld::meaning_schema::ensure(&conn)
        .map_err(|err| format!("cannot migrate {}/recall.sqlite: {err}", root.display()))?;
    // Uploads stored before the ingest plane existed get their clips (#1911).
    let ingest = recalld::store::open(root)
        .map_err(|err| format!("cannot open {}/ingest.sqlite: {err}", root.display()))?;
    recalld::clips::adopt_outside_ingest(&conn, &ingest, root)
        .map(|_| ())
        .map_err(|err| format!("cannot adopt clips outside ingest/: {err}"))
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let Args {
        root,
        binds,
        tokens_path,
        frontend,
        task,
    } = Args::parse();
    if let Some(task) = task {
        return run_task(&root, task);
    }
    // A configured but unreadable token table fails startup: an open ingest
    // plane must be a choice, not a typo.
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
    let Some(trusted_proxies) = trusted_proxies() else {
        return ExitCode::FAILURE;
    };
    if let Err(complaint) = prepare_planes(&root) {
        eprintln!("recalld: {complaint}");
        return ExitCode::FAILURE;
    }
    if let Err(complaint) = keep_faults(&root) {
        eprintln!("recalld: {complaint}");
        return ExitCode::FAILURE;
    }
    // The browsing plane is mounted only with SSO configured: unconfigured
    // means absent, not open.
    let webauth =
        recalld::webauth::Config::from_process_env().map(|cfg| recalld::webauth::GateState {
            cfg: std::sync::Arc::new(cfg),
            now: std::sync::Arc::new(|| chrono::Utc::now().timestamp()),
        });
    if webauth.is_some() {
        tracing::info!("browsing plane mounted behind the Nextcloud SSO gate");
    }
    let config = Arc::new(Config {
        root,
        tokens,
        read_token,
        max_body_bytes: DEFAULT_MAX_BODY,
        trusted_proxies,
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
        // The first listener to stop ends the daemon: half-serving hides a
        // fault, and a restart brings it back whole.
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

/// Bind every address before serving any: a half-bound daemon looks healthy
/// from whichever side you check.
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

/// Re-derive stored speaker guesses when a voice is enrolled, in bounded,
/// logged batches.
fn spawn_rematcher(root: PathBuf) {
    const BATCH: usize = 200;
    const IDLE: std::time::Duration = std::time::Duration::from_mins(5);
    const BACKOFF: std::time::Duration = std::time::Duration::from_mins(5);
    tokio::spawn(async move {
        loop {
            let batch_root = root.clone();
            let done = tokio::task::spawn_blocking(move || {
                let mut conn = recalld::work::open_write(&batch_root)?;
                let now = audiocore::instant::Stamp::now();
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

/// Every background pass the daemon runs, in one place.
fn spawn_background_passes(root: &std::path::Path) {
    let root = root.to_path_buf();
    let root = &root;
    spawn_speech_scanner(root.clone());
    spawn_rematcher(root.clone());
    // Fills clips that have no turns; hides only live guesses on its span.
    spawn_turn_writer(root.clone());
    // The one loop that replaces a transcript somebody reads; keep it the only
    // one, or two writers hide each other's lines.
    spawn_diarized_writer(root.clone());
    spawn_segment_registrar(root.clone());
    spawn_segment_deriver(root.clone());
    spawn_enroller(root.clone());
}

/// Turn human-named turns into voiceprints. Slow on purpose: it only has to
/// keep up with a handful of labels a week, and must not outbid diarization
/// for the GPU.
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
                    &audiocore::instant::Stamp::of(now),
                    WRITE,
                )?;
                Ok::<_, rusqlite::Error>((queued, pass))
            })
            .await;
            match done {
                // A pass that only finds stale spans must still log.
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

/// Turn finished diarize results into speaker-split turns (`diarized::decide`).
/// Small batches on a slow cadence, so a bad verdict is noticed early.
///
/// To reverse it, both planes:
///
/// ```sql
/// -- recall.sqlite: un-hide first, so the blocks never become eligible
/// -- while the originals are hidden.
/// UPDATE transcript_segments SET hidden_reason = NULL
///  WHERE hidden_reason = 'diarized (per-mic runner)';
/// DELETE FROM transcript_segments
///  WHERE provenance = 'diarized-aligned (per-mic runner)';
///
/// -- ingest.sqlite: the declined blocks.
/// DELETE FROM pass_ledger WHERE kind = 'diarize-segment';
/// ```
///
/// Exact equality: `LIKE 'diarized-aligned (%'` also matches the older
/// diarized corpus.
fn spawn_diarized_writer(root: PathBuf) {
    const EVERY: std::time::Duration = std::time::Duration::from_mins(2);
    const BATCH: usize = 20;
    tokio::spawn(async move {
        loop {
            let pass_root = root.clone();
            let done = tokio::task::spawn_blocking(move || {
                let ingest = recalld::store::open(&pass_root)?;
                let mut meaning = recalld::work::open_write(&pass_root)?;
                let now = audiocore::instant::Stamp::now();
                recalld::diarized::write_pass(&mut meaning, &ingest, &now, BATCH)
            })
            .await;
            match done {
                // A pass that declines every block is the one most worth seeing.
                Ok(Ok(pass)) if pass.turns + pass.hidden + pass.kept > 0 => {
                    tracing::info!(
                        blocks = pass.blocks,
                        turns = pass.turns,
                        hidden = pass.hidden,
                        kept = pass.kept,
                        waiting = pass.waiting,
                        "diarized: written"
                    );
                }
                Ok(Ok(_)) => {}
                Ok(Err(err)) => {
                    tracing::warn!(%err, "diarized: pass failed");
                }
                Err(err) => {
                    tracing::error!(%err, "diarized: task failed");
                }
            }
            tokio::time::sleep(EVERY).await;
        }
    });
}

/// Run VAD over every delivered segment, so "active" means someone talking.
/// Small batches: a neural network per 32 ms window, on four cores shared with
/// Nextcloud.
fn spawn_speech_scanner(root: PathBuf) {
    // ort's prebuilt runtime needs AVX2, which Ivy Bridge (isis, amun) lacks:
    // calling it there is a SIGILL. Check once and stay off.
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

/// Turn stored transcriptions into turns, for clips that have none. Hides the
/// live guesses on its span (`live-reconciled`). Small batches, slow cadence.
///
/// To reverse it, both planes:
///
/// ```sql
/// DELETE FROM transcript_segments WHERE provenance = 'per-mic (runner)';
/// DELETE FROM pass_ledger WHERE kind = 'transcribe-segment';
/// ```
fn spawn_turn_writer(root: PathBuf) {
    const EVERY: std::time::Duration = std::time::Duration::from_mins(2);
    const BATCH: usize = 20;
    tokio::spawn(async move {
        loop {
            let pass_root = root.clone();
            let done = tokio::task::spawn_blocking(move || {
                let ingest = recalld::store::open(&pass_root)?;
                let mut meaning = recalld::work::open_write(&pass_root)?;
                let now = audiocore::instant::Stamp::now();
                recalld::turns::write_pass(&mut meaning, &ingest, &now, BATCH)
            })
            .await;
            match done {
                // An all-loops clip writes and refuses nothing, but says the
                // audio is bad.
                Ok(Ok(pass)) if pass.turns + pass.refused + pass.swept > 0 => {
                    tracing::info!(
                        blocks = pass.blocks,
                        turns = pass.turns,
                        refused = pass.refused,
                        swept = pass.swept,
                        barren = pass.barren,
                        "turns: written"
                    );
                }
                Ok(Ok(_)) => {}
                Ok(Err(err)) => {
                    tracing::warn!(%err, "turns: pass failed");
                }
                Err(err) => {
                    tracing::error!(%err, "turns: task failed");
                }
            }
            tokio::time::sleep(EVERY).await;
        }
    });
}

/// Derive transcription jobs for microphone clips with no turns. A timer, not
/// part of `queue::lease`: it spans both planes and scans one. The batch bound
/// keeps a runner from being handed days of backlog at once.
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

/// Register microphone clips in the meaning plane, so their turns have audio.
/// Bounded: `upload::probe` decodes the whole file.
///
/// To reverse it, both planes:
///
/// ```sql
/// DELETE FROM audio_segments WHERE path LIKE '%/ingest/%'
///   AND id NOT IN (SELECT audio_segment_id FROM transcript_segments
///                  WHERE audio_segment_id IS NOT NULL);
/// -- and in ingest.sqlite:
/// DELETE FROM pass_ledger WHERE kind = 'register-segment';
/// ```
///
/// Keep the `NOT IN`: a turn's audio must still resolve.
fn spawn_segment_registrar(root: PathBuf) {
    const EVERY: std::time::Duration = std::time::Duration::from_mins(5);
    const BATCH: usize = 40;
    tokio::spawn(async move {
        loop {
            let pass_root = root.clone();
            let done = tokio::task::spawn_blocking(move || {
                let ingest = recalld::store::open(&pass_root)?;
                let meaning = recalld::work::open_write(&pass_root)?;
                let now = audiocore::instant::Stamp::now();
                recalld::turns::register_segments(&meaning, &ingest, &pass_root, &now, BATCH)
            })
            .await;
            match done {
                // Not `waiting` (an unknown recorder keeps it high) or `retired`
                // (a hash lookup); `covered` cost a full decode, so a rising
                // count shows repeated decoding.
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
