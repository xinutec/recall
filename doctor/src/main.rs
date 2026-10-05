//! `doctor`: the Mac's health agent (launchd, every 300s).
//!
//! `--collect` is the child: it reads the archive volume, which can block
//! indefinitely, and prints [`archive::Collected`]. The default is the parent:
//! it runs the child with a time limit, adds launchd's and the server's checks,
//! prints them all and, with `--post`, sends them to fleetwatch.

use chrono::{DateTime, Utc};
use clap::Parser;
use doctor::check::{Check, Verdict};
use doctor::{agents, archive, bounded, capture, deaf, fleetwatch, live, record};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// The Mac's health agent: the archive's checks, launchd's and the server's.
#[derive(Parser)]
#[command(name = "doctor")]
struct Config {
    /// The archive root.
    #[arg(long)]
    out: PathBuf,
    /// Read the archive and print its checks as JSON (the child process).
    #[arg(long)]
    collect: bool,
    /// Send the verdicts to fleetwatch (token from `RECALL_FLEETWATCH_TOKEN`
    /// or `~/.config/fleetwatch/token`).
    #[arg(long)]
    post: bool,
    /// Fleetwatch.
    #[arg(long, default_value = fleetwatch::DEFAULT_URL)]
    url: String,
    /// The recall server, for the checks it measures (token from
    /// `RECALL_SYNC_TOKEN`). Absent, they skip.
    #[arg(long)]
    fleet: Option<String>,
}

/// The child's answer, or none if it failed or hung.
struct FromArchive {
    checks: Vec<Check>,
    /// Whether and how fast the archive answered; reported either way.
    answered: Check,
    paused_until: Option<DateTime<Utc>>,
}

impl FromArchive {
    fn unanswered(answered: Check) -> Self {
        Self {
            checks: Vec::new(),
            answered,
            paused_until: None,
        }
    }
}

/// Ask a child process for the archive's checks, and give up on it if it hangs.
fn read_archive_checks(out: &Path) -> FromArchive {
    let Ok(program) = std::env::current_exe() else {
        return FromArchive::unanswered(archive::archive_check(None, "cannot find my own binary"));
    };
    let args = vec![
        "--out".to_owned(),
        out.to_string_lossy().into_owned(),
        "--collect".to_owned(),
    ];
    let bound = archive::archive_bound().to_std().expect("a positive bound");
    let answer = match bounded::run(&program, &args, bound, &[]) {
        Ok(answer) => answer,
        Err(err) => {
            return FromArchive::unanswered(archive::archive_check(
                None,
                &format!("cannot start the archive read: {err}"),
            ));
        }
    };

    let Some(stdout) = answer.stdout else {
        // Timestamped: these lines are the only record of a stall, and the
        // first question is whether stalls cluster in time.
        let state = bounded::process_state(answer.pid);
        eprintln!(
            "{} doctor: the archive did not answer in {:.0}s — abandoned pid {} in state {} ({})",
            Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            bound.as_secs_f64(),
            answer.pid,
            state.label(),
            state.explain()
        );
        // The volume probe prints first, so this says which half was slow.
        for line in answer.stderr.lines().filter(|l| !l.trim().is_empty()) {
            eprintln!("  it had said: {line}");
        }
        // Who else was on the disk then (#1412).
        for line in bounded::disk_suspects_now(20) {
            eprintln!("  on the disk then: {line}");
        }
        return FromArchive::unanswered(archive::archive_check(None, ""));
    };
    if answer.status != Some(0) {
        let reason = answer.stderr.trim();
        let last = reason.lines().last().unwrap_or("the archive read failed");
        return FromArchive::unanswered(archive::archive_check(Some(answer.seconds), last));
    }
    match serde_json::from_str::<archive::Collected>(&stdout) {
        Ok(report) => FromArchive {
            checks: report.checks,
            answered: archive::archive_check(Some(report.seconds), ""),
            paused_until: report
                .paused_until
                .as_deref()
                .and_then(audiocore::instant::parse_utc),
        },
        Err(_) => FromArchive::unanswered(archive::archive_check(
            Some(answer.seconds),
            "unreadable archive report",
        )),
    }
}

/// The checks whose evidence is on the server: the live tier's two, the deaf
/// microphone and the record's faults. In the parent, since behind the child's
/// bound a slow server would read as a stalled disk.
fn live_checks(
    config: &Config,
    now: DateTime<Utc>,
    paused_until: Option<DateTime<Utc>>,
) -> Vec<Check> {
    let token = std::env::var("RECALL_SYNC_TOKEN").ok();
    let Some(fleet) = live::Fleet::new(config.fleet.as_deref(), token.as_deref()) else {
        let mut checks = live::unconfigured();
        checks.push(deaf::unconfigured());
        checks.extend(record::unconfigured());
        return checks;
    };
    let fetched = live::fetch(&fleet, now, capture::live_lag_window());
    let mut checks = live::live_checks(&fetched, now, paused_until);
    checks.push(deaf::deaf_check_from(&deaf::fetch(
        &fleet,
        now,
        deaf::window(),
    )));
    checks.extend(record::record_checks(&record::fetch(&fleet, now)));
    checks
}

/// An unreachable fleetwatch is logged, not failed: it shows the missing report
/// as stale.
fn report_to_fleetwatch(checks: &[Check], url: &str, home: &Path) {
    let from_env = std::env::var("RECALL_FLEETWATCH_TOKEN").ok();
    let Some(token) = fleetwatch::read_token(home, from_env.as_deref()) else {
        eprintln!(
            "doctor: no fleetwatch token — put the ingest token in \
             ~/.config/fleetwatch/token (see the fleetwatch README)"
        );
        return;
    };
    let body = fleetwatch::payload(checks, Utc::now(), None);
    match fleetwatch::post(&body, &token, url) {
        Ok(status) => println!("doctor: reported to fleetwatch ({status})"),
        Err(err) => eprintln!("doctor: could not reach fleetwatch: {err}"),
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

fn main() {
    let config = Config::parse();
    let now = Utc::now();

    if config.collect {
        let started = Instant::now();
        // Printed before anything slow: if the reads below hang, the parent
        // still sees stderr.
        let volume = archive::volume_check(&config.out);
        eprintln!("doctor: {} — {}", volume.label, volume.observed);
        let paused_until = agents::paused_until(&config.out);
        let checks = match archive::archive_checks(&config.out, now, volume, paused_until) {
            Ok(checks) => checks,
            Err(err) => {
                // The parent reports the last stderr line.
                eprintln!("{err}");
                std::process::exit(1);
            }
        };
        let collected = archive::Collected {
            seconds: started.elapsed().as_secs_f64(),
            checks,
            paused_until: paused_until.map(audiocore::instant::python_isoformat_utc),
        };
        println!(
            "{}",
            serde_json::to_string(&collected).expect("plain data serialises")
        );
        return;
    }

    let archive = read_archive_checks(&config.out);
    let mut checks = vec![archive.answered];
    checks.extend(archive.checks);
    checks.extend(capture::agent_checks(&agents::agent_health(&home())));
    checks.extend(live_checks(&config, now, archive.paused_until));

    for check in &checks {
        println!(
            "  [{:>4}] {}/{}: {}",
            check.verdict.mark(),
            check.section,
            check.label,
            check.observed
        );
    }

    if config.post {
        report_to_fleetwatch(&checks, &config.url, &home());
    }

    let failed = checks.iter().filter(|c| c.verdict == Verdict::Fail).count();
    if failed > 0 {
        println!("doctor: {failed} check(s) FAILED");
        std::process::exit(1);
    }
    println!("doctor: healthy");
}
