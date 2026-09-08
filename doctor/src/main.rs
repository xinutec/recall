//! `doctor` — the Mac's health agent (launchd, every 300s).
//!
//! Two modes, and the split between them is the point (see the crate docs):
//!
//! * `--collect` is the CHILD. It reads the archive volume and prints one JSON
//!   object. Everything that can block indefinitely is here.
//! * the default is the PARENT. It asks a child for the archive's verdicts,
//!   gives up on it after a bound, adds what it can read from the boot disk
//!   (launchd), prints the lot, and with `--post` sends it to fleetwatch.

use chrono::Utc;
use doctor::check::{Check, Verdict};
use doctor::{agents, archive, bounded, capture, fleetwatch};
use std::path::{Path, PathBuf};
use std::time::Instant;

struct Config {
    out: PathBuf,
    collect: bool,
    post: bool,
    url: String,
}

fn usage() -> ! {
    eprintln!(
        "usage: doctor --out <archive root> [--post] [--collect] [--url <fleetwatch>]\n\
         \n\
         --post   send the verdicts to fleetwatch (token from\n\
         \x20        RECALL_FLEETWATCH_TOKEN or ~/.config/fleetwatch/token)\n\
         --collect  read the archive and print its checks as JSON — the child\n\
         \x20          half; not meant to be run by hand"
    );
    std::process::exit(2)
}

fn parse_args() -> Config {
    let mut out = None;
    let mut collect = false;
    let mut post = false;
    let mut url = fleetwatch::DEFAULT_URL.to_owned();
    let mut cli = std::env::args().skip(1);
    while let Some(arg) = cli.next() {
        match arg.as_str() {
            "--out" => out = Some(PathBuf::from(cli.next().unwrap_or_else(|| usage()))),
            "--url" => url = cli.next().unwrap_or_else(|| usage()),
            "--collect" => collect = true,
            "--post" => post = true,
            _ => usage(),
        }
    }
    Config {
        out: out.unwrap_or_else(|| usage()),
        collect,
        post,
        url,
    }
}

/// What `--collect` prints: the child's own timing plus its verdicts.
#[derive(serde::Deserialize)]
struct Collected {
    seconds: f64,
    checks: Vec<Check>,
}

/// Ask a child process for the archive's checks, and give up on it if it hangs.
///
/// Returns what it managed to say plus the verdict on the asking itself, which
/// is reported whether or not the archive answered — that check IS the finding
/// when it did not.
fn read_archive_checks(out: &Path) -> (Vec<Check>, Check) {
    let Ok(program) = std::env::current_exe() else {
        return (
            Vec::new(),
            archive::archive_check(None, "cannot find my own binary"),
        );
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
            return (
                Vec::new(),
                archive::archive_check(None, &format!("cannot start the archive read: {err}")),
            );
        }
    };

    let Some(stdout) = answer.stdout else {
        eprintln!(
            "doctor: the archive did not answer in {:.0}s — abandoned pid {} \
             (it is in uninterruptible disk wait; it exits when the volume does)",
            bound.as_secs_f64(),
            answer.pid
        );
        return (Vec::new(), archive::archive_check(None, ""));
    };
    if answer.status != Some(0) {
        let reason = answer.stderr.trim();
        let last = reason.lines().last().unwrap_or("the archive read failed");
        return (
            Vec::new(),
            archive::archive_check(Some(answer.seconds), last),
        );
    }
    match serde_json::from_str::<Collected>(&stdout) {
        Ok(report) => (
            report.checks,
            archive::archive_check(Some(report.seconds), ""),
        ),
        Err(_) => (
            Vec::new(),
            archive::archive_check(Some(answer.seconds), "unreadable archive report"),
        ),
    }
}

/// Send the verdicts on. An unreachable monitor is not a broken recording: say
/// so and carry on, because the missing report is already visible at the other
/// end as staleness. Failing the health check because the *health reporting*
/// failed would be the tail wagging the dog.
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
    let config = parse_args();
    let now = Utc::now();

    if config.collect {
        // The child times ITSELF, so the figure that reaches fleetwatch is the
        // archive read rather than a second process's startup.
        let started = Instant::now();
        let checks = match archive::archive_checks(
            &config.out,
            now,
            std::env::var_os("RECALL_SYNC_TOKEN").is_some(),
        ) {
            Ok(checks) => checks,
            Err(err) => {
                // stderr, and a non-zero exit: the parent turns this into the
                // archive check's `detail`, which is where a reader will look.
                eprintln!("{err}");
                std::process::exit(1);
            }
        };
        println!(
            "{}",
            serde_json::json!({
                "seconds": started.elapsed().as_secs_f64(),
                "checks": checks,
            })
        );
        return;
    }

    let (archive_checks, reachable) = read_archive_checks(&config.out);
    let mut checks = vec![reachable];
    checks.extend(archive_checks);
    checks.extend(capture::agent_checks(&agents::agent_health(&home())));

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
