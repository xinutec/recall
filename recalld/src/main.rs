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

#[derive(Clone, clap::Subcommand)]
enum Task {
    /// Give every stored file a clip and say whether all have one
    /// (`recalld::clips`).
    ClipCensus,
    /// Render every clip into the rendered view and say what it did
    /// (`recalld::view`).
    RenderView,
    /// Whether every stored file matches the size and hash its row recorded,
    /// and every audio row names a file (`recalld::verify`). Reads every byte.
    Verify,
    /// Delete the household capture recorded in `[from, to)` from both planes
    /// and the disk, tombstoned so it cannot be delivered again
    /// (`recalld::forget`). Uploads are left to their own delete. Without
    /// `--apply` it only says what the span holds.
    ForgetSpan {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        apply: bool,
    },
    /// Remove a derived source (one built from the mics, not recorded) from
    /// both planes and the disk. Refused for anything recorded.
    RetireSource {
        #[arg(long)]
        source: String,
        #[arg(long)]
        apply: bool,
    },
}

/// Open both planes as the server does, run `report` over them, and print what
/// it returns as JSON.
fn report<T: serde::Serialize, E: std::fmt::Display>(
    root: &std::path::Path,
    report: impl FnOnce(&mut rusqlite::Connection, &mut rusqlite::Connection) -> Result<T, E>,
) -> ExitCode {
    if let Err(complaint) = recalld::app::prepare(root) {
        eprintln!("recalld: {complaint}");
        return ExitCode::FAILURE;
    }
    let done = recalld::work::open_write(root)
        .map_err(|err| err.to_string())
        .and_then(|mut meaning| {
            let mut ingest = recalld::store::open(root).map_err(|err| err.to_string())?;
            report(&mut meaning, &mut ingest).map_err(|err| err.to_string())
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
    match task {
        Task::ClipCensus => report(root, |meaning, ingest| {
            recalld::clips::census(meaning, ingest, root)
        }),
        Task::RenderView => report(root, |meaning, ingest| {
            recalld::view::refresh(meaning, ingest, root, true)
        }),
        Task::Verify => report(root, |meaning, ingest| {
            recalld::verify::run(meaning, ingest, root)
        }),
        Task::ForgetSpan { from, to, apply } => {
            let Some(span) = recalld::forget::Span::parse(&from, &to) else {
                eprintln!("recalld: --from and --to must be instants, --from first");
                return ExitCode::FAILURE;
            };
            let now = audiocore::instant::Stamp::now();
            report(root, |meaning, ingest| {
                recalld::forget::span(meaning, ingest, root, &span, &now, apply)
            })
        }
        Task::RetireSource { source, apply } => {
            let Some(source) = transcript::SourceId::parse(&source) else {
                eprintln!("recalld: --source must be a source id");
                return ExitCode::FAILURE;
            };
            report(root, |meaning, ingest| {
                recalld::forget::retire_source(meaning, ingest, root, &source, apply)
            })
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
    if let Err(complaint) = recalld::app::prepare(&root) {
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
    // Keeps shown lines current; a person's write renders its own clips.
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
            let pass_root = root.clone();
            let job = move || -> Result<_, recalld::view::ViewError> {
                let meaning = recalld::work::open_write(&pass_root)?;
                let ingest = recalld::store::open(&pass_root)?;
                ingest.busy_timeout(std::time::Duration::from_mins(2))?;
                recalld::view::refresh(&meaning, &ingest, &pass_root, all)
            };
            if let Some(done) = PASSES.run("view", job, view_problem).await {
                if all || done.written > 0 || !done.unreadable.is_empty() || !done.failed.is_empty()
                {
                    tracing::info!(
                        all,
                        rendered = done.rendered,
                        written = done.written,
                        unreadable = done.unreadable.len(),
                        failed = done.failed.len(),
                        live = done.live_shown,
                        "view: refreshed"
                    );
                }
                all = false;
            }
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

/// Turn people's namings into voiceprints. Slow on purpose: it only has to
/// keep up with a handful of labels a week, and must not outbid diarization
/// for the GPU.
fn spawn_enroller(root: PathBuf) {
    const EVERY: std::time::Duration = std::time::Duration::from_mins(10);
    const DERIVE: usize = 5;
    const WRITE: usize = 20;
    PASSES.register("enrol", EVERY, chrono::Utc::now());
    tokio::spawn(async move {
        loop {
            let pass_root = root.clone();
            let job = move || {
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
            };
            // A pass that only finds stale spans must still log.
            if let Some((queued, pass)) = PASSES.run("enrol", job, |_| None).await
                && queued + pass.prints + pass.stale > 0
            {
                tracing::info!(
                    queued,
                    clips = pass.clips,
                    prints = pass.prints,
                    stale = pass.stale,
                    "enrol: pass"
                );
            }
            tokio::time::sleep(EVERY).await;
        }
    });
}

/// Run VAD over every delivered segment, so "active" means someone talking.
/// Small batches: a neural network per 32 ms window, on four cores shared with
/// Nextcloud.
fn spawn_speech_scanner(root: PathBuf) {
    // One trial inference first: a runtime that cannot run here leaves the
    // pass off, reported as such, rather than failing every batch.
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
            let batch_root = root.clone();
            let job = move || recalld::speech::scan_once(&batch_root, BATCH);
            let pause = match PASSES.run("speech", job, |_| None).await {
                Some(0) => Some(IDLE),
                Some(n) => {
                    tracing::info!(measured = n, "speech: batch complete");
                    None
                }
                None => Some(BACKOFF),
            };
            if let Some(pause) = pause {
                tokio::time::sleep(pause).await;
            }
        }
    });
}

/// Derive transcription jobs for microphone clips not yet transcribed. A timer, not
/// part of `queue::lease`: it spans both planes and scans one. The batch bound
/// keeps a runner from being handed days of backlog at once.
fn spawn_segment_deriver(root: PathBuf) {
    const EVERY: std::time::Duration = std::time::Duration::from_mins(10);
    const BATCH: usize = 50;
    PASSES.register("segment-derive", EVERY, chrono::Utc::now());
    tokio::spawn(async move {
        loop {
            let pass_root = root.clone();
            let job = move || {
                let ingest = recalld::store::open(&pass_root)?;
                let meaning = recalld::work::open_write(&pass_root)?;
                let now = chrono::Utc::now();
                let followed = recalld::queue::derive_follow_ons(&ingest, now)?;
                if followed > 0 {
                    tracing::info!(followed, "segments: diarize jobs caught up");
                }
                recalld::queue::derive_segment_jobs(&ingest, &meaning, now, BATCH)
            };
            if let Some(queued) = PASSES.run("segment-derive", job, |_| None).await
                && queued > 0
            {
                tracing::info!(queued, "segments: transcribe jobs derived");
            }
            tokio::time::sleep(EVERY).await;
        }
    });
}

/// Register microphone clips in the meaning plane, so their lines have audio.
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
            let pass_root = root.clone();
            let job = move || {
                let ingest = recalld::store::open(&pass_root)?;
                let meaning = recalld::work::open_write(&pass_root)?;
                let now = audiocore::instant::Stamp::now();
                recalld::registrar::register_segments(&meaning, &ingest, &pass_root, &now, BATCH)
            };
            // Logged when the pass decided a clip; `waiting` (an unknown
            // recorder keeps it high) and `known` alone are not news.
            if let Some(pass) = PASSES.run("segment-register", job, |_| None).await
                && pass.added + pass.unreadable + pass.covered > 0
            {
                tracing::info!(
                    added = pass.added,
                    covered = pass.covered,
                    known = pass.known,
                    probed = pass.probed,
                    unreadable = pass.unreadable,
                    waiting = pass.waiting,
                    "segments: registered for playback"
                );
            }
            tokio::time::sleep(EVERY).await;
        }
    });
}
