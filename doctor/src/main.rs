//! `doctor`: the Mac's health agent (launchd, every 300s).
//!
//! * `--collect` is the child: it reads the archive volume and prints one JSON
//!   object. Everything that can block indefinitely is here.
//! * the default is the parent: it asks a child for the archive's verdicts,
//!   gives up on it after a bound, adds launchd and the fleet's checks, prints
//!   the lot, and with `--post` sends it to fleetwatch.

use chrono::Utc;
use doctor::check::{Check, Verdict};
use doctor::{agents, archive, bounded, capture, deaf, fleetwatch, live};
use std::path::{Path, PathBuf};
use std::time::Instant;

struct Config {
    out: PathBuf,
    collect: bool,
    post: bool,
    url: String,
    /// Where the fleet is. Absent, the fleet checks skip rather than guess an
    /// address.
    fleet: Option<String>,
}

fn usage() -> ! {
    eprintln!(
        "usage: doctor --out <archive root> [--post] [--collect] [--url <fleetwatch>]\n\
         \n\
         --post   send the verdicts to fleetwatch (token from\n\
         \x20        RECALL_FLEETWATCH_TOKEN or ~/.config/fleetwatch/token)\n\
         --fleet  the fleet's base URL, for the live tier's own numbers\n\
         \x20        (bearer from RECALL_SYNC_TOKEN)\n\
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
    let mut fleet = None;
    let mut cli = std::env::args().skip(1);
    while let Some(arg) = cli.next() {
        match arg.as_str() {
            "--out" => out = Some(PathBuf::from(cli.next().unwrap_or_else(|| usage()))),
            "--url" => url = cli.next().unwrap_or_else(|| usage()),
            "--fleet" => fleet = Some(cli.next().unwrap_or_else(|| usage())),
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
        fleet,
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
/// Returns its checks plus the verdict on the asking itself, which is reported
/// whether or not the archive answered.
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
        // Timestamped: these lines are the only record that a stall happened,
        // and the first question about them is whether they cluster in time.
        let state = bounded::process_state(answer.pid);
        eprintln!(
            "{} doctor: the archive did not answer in {:.0}s — abandoned pid {} in state {} ({})",
            Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            bound.as_secs_f64(),
            answer.pid,
            state.label(),
            state.explain()
        );
        // What the child said before it hung: the volume probe prints first, so
        // this names which half was slow.
        for line in answer.stderr.lines().filter(|l| !l.trim().is_empty()) {
            eprintln!("  it had said: {line}");
        }
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

/// The checks whose evidence is on the fleet (the live tier's two and the deaf
/// microphone), asked of it and graded here.
///
/// In the parent, not the bounded child: the child survives an unresponsive
/// volume, and behind the same bound a slow fleet would read as a stalled disk.
fn live_checks(config: &Config, now: chrono::DateTime<Utc>, out: &Path) -> Vec<Check> {
    let token = std::env::var("RECALL_SYNC_TOKEN").ok();
    let Some(fleet) = live::Fleet::new(config.fleet.as_deref(), token.as_deref()) else {
        let mut checks = live::unconfigured();
        checks.push(deaf::unconfigured());
        return checks;
    };
    let fetched = live::fetch(&fleet, now, capture::live_lag_window());
    let mut checks = live::live_checks(&fetched, now, agents::paused_until(out));
    checks.push(deaf::deaf_check_from(&deaf::fetch(
        &fleet,
        now,
        deaf::window(),
    )));
    checks
}

/// Send the verdicts on. An unreachable monitor is not a broken recording: say
/// so and carry on, since the missing report shows as staleness at the other
/// end.
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
        // The child times itself, so the figure excludes process startup.
        let started = Instant::now();
        // ⚠ Probe the disk and print it before anything slow: if the reads
        // below hang, the parent never sees the checks but does see stderr.
        let volume = archive::volume_check(&config.out);
        eprintln!("doctor: {} — {}", volume.label, volume.observed);
        let checks = match archive::archive_checks(&config.out, now, volume) {
            Ok(checks) => checks,
            Err(err) => {
                // The parent turns stderr plus a non-zero exit into the archive
                // check's `detail`.
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
    checks.extend(live_checks(&config, now, &config.out));

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
