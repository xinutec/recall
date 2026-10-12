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
use recalld::passes::PASSES;
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
    /// Render every clip into the rendered view and say what it did
    /// (`recalld::view`).
    RenderView,
    /// Whether every stored file matches the size and hash its row recorded,
    /// and every audio row names a file (`recalld::verify`). Reads every byte.
    Verify,
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

/// Open both planes as the server does, run `report` over them, and print what
/// it returns as JSON.
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
        Task::RenderView => {
            return report(root, |meaning, ingest| {
                recalld::view::refresh(meaning, ingest, root, true)
            });
        }
        Task::Verify => {
            return report(root, |meaning, ingest| {
                recalld::verify::run(meaning, ingest, root)
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
    Ok(())
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

/// Every background pass the daemon runs, in one place.
fn spawn_background_passes(root: &std::path::Path) {
    let root = root.to_path_buf();
    let root = &root;
    spawn_speech_scanner(root.clone());
    // The one loop that writes shown lines: render's, from results and acts.
    spawn_view(root.clone());
    spawn_segment_registrar(root.clone());
    spawn_segment_deriver(root.clone());
    spawn_enroller(root.clone());
}

/// Keep the rendered view current: every clip once at start (render's rules
/// may have changed with this binary), then each clip whose inputs moved.
fn spawn_view(root: PathBuf) {
    const EVERY: std::time::Duration = std::time::Duration::from_secs(30);
    PASSES.register("view", EVERY, chrono::Utc::now());
    tokio::spawn(async move {
        let mut all = true;
        loop {
            PASSES.begin("view", chrono::Utc::now());
            let pass_root = root.clone();
            let done = tokio::task::spawn_blocking(move || {
                let meaning = recalld::work::open_write(&pass_root)?;
                let ingest = recalld::store::open(&pass_root)?;
                ingest.busy_timeout(std::time::Duration::from_mins(2))?;
                recalld::view::refresh(&meaning, &ingest, &pass_root, all)
                    .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))
            })
            .await;
            let problem = match done {
                Ok(Ok(done)) => {
                    if done.rendered > 0 || !done.unreadable.is_empty() || !done.failed.is_empty() {
                        tracing::info!(
                            all,
                            rendered = done.rendered,
                            unreadable = done.unreadable.len(),
                            failed = done.failed.len(),
                            live = done.live_shown,
                            "view: refreshed"
                        );
                    }
                    all = false;
                    view_problem(&done)
                }
                Ok(Err(err)) => {
                    tracing::warn!(%err, "view: refresh failed");
                    Some(err.to_string())
                }
                Err(err) => {
                    tracing::error!(%err, "view: task failed");
                    Some(err.to_string())
                }
            };
            PASSES.ended("view", chrono::Utc::now(), problem);
            tokio::time::sleep(EVERY).await;
        }
    });
}

/// What the doctor should hear about a refresh that ran: clips it could not
/// render or whose result does not read. Their old lines stay meanwhile.
fn view_problem(done: &recalld::view::Refreshed) -> Option<String> {
    let first = done.failed.first().or_else(|| done.unreadable.first())?;
    Some(format!(
        "{} clip(s) would not render and {} do not read; clip {}: {}",
        done.failed.len(),
        done.unreadable.len(),
        first.0,
        first.1
    ))
}

/// Turn human-named turns into voiceprints. Slow on purpose: it only has to
/// keep up with a handful of labels a week, and must not outbid diarization
/// for the GPU.
fn spawn_enroller(root: PathBuf) {
    const EVERY: std::time::Duration = std::time::Duration::from_mins(10);
    const DERIVE: usize = 5;
    const WRITE: usize = 20;
    PASSES.register("enrol", EVERY, chrono::Utc::now());
    tokio::spawn(async move {
        loop {
            PASSES.begin("enrol", chrono::Utc::now());
            let pass_root = root.clone();
            let done = tokio::task::spawn_blocking(move || {
                let ingest = recalld::store::open(&pass_root)?;
                let meaning = recalld::work::open_write(&pass_root)?;
                let now = chrono::Utc::now();
                let forgotten = recalld::enrol::forget_unwanted(&meaning)?;
                if forgotten > 0 {
                    tracing::info!(forgotten, "enrol: prints forgotten");
                }
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
            let problem = match done {
                // A pass that only finds stale spans must still log.
                Ok(Ok((queued, pass))) if queued + pass.prints + pass.stale > 0 => {
                    tracing::info!(
                        queued,
                        clips = pass.clips,
                        prints = pass.prints,
                        stale = pass.stale,
                        "enrol: pass"
                    );
                    None
                }
                Ok(Ok(_)) => None,
                Ok(Err(err)) => {
                    tracing::warn!(%err, "enrol: pass failed");
                    Some(err.to_string())
                }
                Err(err) => {
                    tracing::error!(%err, "enrol: task failed");
                    Some(err.to_string())
                }
            };
            PASSES.ended("enrol", chrono::Utc::now(), problem);
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
    // ort panics, rather than erring, when its library will not load: caught,
    // or one missing file would keep the whole server from starting.
    let tried = std::panic::catch_unwind(audiocore::vad::self_test).unwrap_or_else(|_| {
        Err(audiocore::vad::Error::Model(
            "the ONNX runtime would not load".to_owned(),
        ))
    });
    if let Err(err) = tried {
        tracing::warn!(
            %err,
            "speech: DISABLED — the ONNX runtime could not run a trial inference. \
             Segments stay unmeasured until it can; the ingest plane is unaffected."
        );
        PASSES.off(
            "speech",
            IDLE,
            chrono::Utc::now(),
            format!("the ONNX runtime could not run a trial inference: {err}"),
        );
        return;
    }
    PASSES.register("speech", IDLE, chrono::Utc::now());
    tokio::spawn(async move {
        loop {
            PASSES.begin("speech", chrono::Utc::now());
            let batch_root = root.clone();
            let wrote =
                tokio::task::spawn_blocking(move || recalld::speech::scan_once(&batch_root, BATCH))
                    .await;
            let (problem, pause) = match wrote {
                Ok(Ok(0)) => (None, Some(IDLE)),
                Ok(Ok(n)) => {
                    tracing::info!(measured = n, "speech: batch complete");
                    (None, None)
                }
                Ok(Err(err)) => {
                    tracing::warn!(%err, "speech: scan failed; backing off");
                    (Some(err.to_string()), Some(BACKOFF))
                }
                Err(err) => {
                    tracing::error!(%err, "speech: task failed; backing off");
                    (Some(err.to_string()), Some(BACKOFF))
                }
            };
            PASSES.ended("speech", chrono::Utc::now(), problem);
            if let Some(pause) = pause {
                tokio::time::sleep(pause).await;
            }
        }
    });
}

/// Derive transcription jobs for microphone clips with no turns. A timer, not
/// part of `queue::lease`: it spans both planes and scans one. The batch bound
/// keeps a runner from being handed days of backlog at once.
fn spawn_segment_deriver(root: PathBuf) {
    const EVERY: std::time::Duration = std::time::Duration::from_mins(10);
    const BATCH: usize = 50;
    PASSES.register("segment-derive", EVERY, chrono::Utc::now());
    tokio::spawn(async move {
        loop {
            PASSES.begin("segment-derive", chrono::Utc::now());
            let pass_root = root.clone();
            let done = tokio::task::spawn_blocking(move || {
                let ingest = recalld::store::open(&pass_root)?;
                let meaning = recalld::work::open_write(&pass_root)?;
                recalld::queue::derive_segment_jobs(&ingest, &meaning, chrono::Utc::now(), BATCH)
            })
            .await;
            let problem = match done {
                Ok(Ok(0)) => None,
                Ok(Ok(queued)) => {
                    tracing::info!(queued, "segments: transcribe jobs derived");
                    None
                }
                Ok(Err(err)) => {
                    tracing::warn!(%err, "segment derive: pass failed");
                    Some(err.to_string())
                }
                Err(err) => {
                    tracing::error!(%err, "segment derive: task failed");
                    Some(err.to_string())
                }
            };
            PASSES.ended("segment-derive", chrono::Utc::now(), problem);
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
///   AND id NOT IN (SELECT audio_segment_id FROM rendered_lines
///                  WHERE audio_segment_id IS NOT NULL);
/// -- and in ingest.sqlite:
/// DELETE FROM pass_ledger WHERE kind = 'register-segment';
/// ```
///
/// Keep the `NOT IN`: a line's audio must still resolve.
fn spawn_segment_registrar(root: PathBuf) {
    const EVERY: std::time::Duration = std::time::Duration::from_mins(5);
    const BATCH: usize = 40;
    PASSES.register("segment-register", EVERY, chrono::Utc::now());
    tokio::spawn(async move {
        loop {
            PASSES.begin("segment-register", chrono::Utc::now());
            let pass_root = root.clone();
            let done = tokio::task::spawn_blocking(move || {
                let ingest = recalld::store::open(&pass_root)?;
                let meaning = recalld::work::open_write(&pass_root)?;
                let now = audiocore::instant::Stamp::now();
                recalld::registrar::register_segments(&meaning, &ingest, &pass_root, &now, BATCH)
            })
            .await;
            let mut problem = None;
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
                Ok(Err(err)) => {
                    tracing::warn!(%err, "segment register: pass failed");
                    problem = Some(err.to_string());
                }
                Err(err) => {
                    tracing::error!(%err, "segment register: task failed");
                    problem = Some(err.to_string());
                }
            }
            PASSES.ended("segment-register", chrono::Utc::now(), problem);
            tokio::time::sleep(EVERY).await;
        }
    });
}
