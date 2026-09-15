//! Argument parsing and dispatch for `recall-cli`.
//!
//! Hand-rolled, like `runner` and `audiod`: the workspace carries no argument
//! parser, and one subcommand table is not the place to start.

use cli::api::{Api, Error};
use cli::render;

/// The fleet: the system of record, and the only thing this talks to.
const DEFAULT_API: &str = "http://10.100.0.2:8000";
const DEFAULT_LIMIT: i64 = 100;

fn usage() -> ! {
    eprintln!(
        "usage: recall-cli [--api <url>] <command> [args]\n\
         \n\
         commands:\n\
         \x20 search <query> [--limit N]   full-text search the archive\n\
         \x20 show <id> [<id>...]          diagnostic dump of specific turns\n\
         \x20 timeline [--limit N]         the newest turns\n\
         \x20 review [--limit N]           turns the model was least sure of\n\
         \x20 sources                      every recorder the fleet knows\n\
         \x20 capture                      whether the recorders are running\n\
         \x20 correct <id> <text> --apply  replace a turn's text\n\
         \n\
         --api defaults to {DEFAULT_API}, the system of record. There is no\n\
         option to read a local database: a second answer nobody can tell from\n\
         the first is what this replaced.\n\
         \n\
         Reading transcripts needs a browsing session; `capture` and `sources`\n\
         do not.\n\
         \n\
         {}",
        cli::api::HOW_TO_SIGN_IN
    );
    std::process::exit(2)
}

/// Pull `--limit N` out of the remaining arguments, leaving the rest.
fn take_limit(args: &mut Vec<String>) -> i64 {
    let Some(at) = args.iter().position(|a| a == "--limit") else {
        return DEFAULT_LIMIT;
    };
    let Some(value) = args.get(at + 1).and_then(|v| v.parse().ok()) else {
        usage()
    };
    args.drain(at..=at + 1);
    value
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    let Some(at) = args.iter().position(|a| a == flag) else {
        return false;
    };
    args.remove(at);
    true
}

/// `Ok(false)` means "nothing matched", which is exit 1 — the shape the Python
/// had, so a script that branched on it keeps working.
fn run(api: &Api, command: &str, mut args: Vec<String>) -> Result<bool, Error> {
    match command {
        "search" => search(api, &mut args),
        "show" => show(api, &args),
        "timeline" => timeline(api, &mut args),
        "review" => review(api, &mut args),
        "sources" => sources(api),
        "capture" => capture(api),
        "correct" => correct(api, &mut args),
        _ => usage(),
    }
}

fn search(api: &Api, args: &mut Vec<String>) -> Result<bool, Error> {
    let limit = take_limit(args);
    let query = args.join(" ");
    if query.is_empty() {
        usage()
    }
    let hits = api.search(&query, limit)?;
    if hits.is_empty() {
        println!("no matches for {query:?}");
        return Ok(false);
    }
    for hit in &hits {
        println!("{}", render::hit(hit));
    }
    Ok(true)
}

fn show(api: &Api, args: &[String]) -> Result<bool, Error> {
    let mut ids = Vec::new();
    for arg in args {
        match arg.parse::<i64>() {
            Ok(id) => ids.push(id),
            Err(_) => usage(),
        }
    }
    if ids.is_empty() {
        usage()
    }
    let turns = api.transcripts(&ids)?;
    if turns.is_empty() {
        println!("no turns found for {ids:?}");
        return Ok(false);
    }
    println!("{}", render::details(&ids, &turns));
    Ok(true)
}

fn timeline(api: &Api, args: &mut Vec<String>) -> Result<bool, Error> {
    let limit = take_limit(args);
    let page = api.timeline(limit, None)?;
    if page.items.is_empty() {
        println!("no turns");
        return Ok(false);
    }
    println!("{}", render::transcript("timeline", &page.items));
    if page.has_more {
        println!("\n… more, older than this page");
    }
    Ok(true)
}

fn review(api: &Api, args: &mut Vec<String>) -> Result<bool, Error> {
    let limit = take_limit(args);
    let turns = api.review(limit)?;
    if turns.is_empty() {
        println!("nothing waiting for review");
        return Ok(false);
    }
    for turn in &turns {
        println!("{}", render::hit(turn));
    }
    Ok(true)
}

fn sources(api: &Api) -> Result<bool, Error> {
    let sources = api.sources()?;
    for source in &sources {
        let state = if source.recording {
            "recording"
        } else if source.active {
            "active"
        } else {
            "idle"
        };
        println!(
            "{:12} {:10} {:10} last={}",
            source.id,
            source.kind,
            state,
            source.last_active.as_deref().unwrap_or("never")
        );
    }
    Ok(!sources.is_empty())
}

/// ⚠ Read-only, and the reason it is a command at all: the pause is Pippijn's
/// control, and anything that could silence a recorder is preceded by asking
/// what the recorders are currently doing.
fn capture(api: &Api) -> Result<bool, Error> {
    let capture = api.capture()?;
    println!(
        "running={}  desired={}  settled={}  mic={}",
        capture.running,
        capture.desired_running,
        capture.settled,
        if capture.mic_reachable {
            "reachable"
        } else {
            "UNREACHABLE"
        }
    );
    if let Some(until) = &capture.paused_until {
        println!("paused until {until}");
    }
    Ok(true)
}

/// ⚠ The one write, and it reaches the corrections corpus — the only thing in
/// the archive that is not re-derivable from audio. `--apply` is required and
/// the change is printed either way: the Python this replaces was dry-run by
/// default for the same reason, and a correction typed against the wrong id is
/// not recoverable from the CLI.
fn correct(api: &Api, args: &mut Vec<String>) -> Result<bool, Error> {
    let apply = take_flag(args, "--apply");
    let (Some(id), Some(text)) = (
        args.first().and_then(|a| a.parse::<i64>().ok()),
        args.get(1),
    ) else {
        usage()
    };
    let before = api.transcripts(&[id])?;
    let Some(current) = before.first() else {
        println!("no turn {id}");
        return Ok(false);
    };
    println!("#{}  [{}]", current.id, render::who(current));
    println!("   OLD: {}", current.text);
    println!("   NEW: {text}");
    if apply {
        let new_id = api.correct(current.id, text)?;
        println!("   -> applied as new turn #{new_id}");
    } else {
        println!("\nDRY-RUN only — nothing written. Re-run with --apply to commit.");
    }
    Ok(true)
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut base = std::env::var("RECALL_API").unwrap_or_else(|_| DEFAULT_API.to_owned());
    if args.first().is_some_and(|a| a == "--api") {
        if args.len() < 2 {
            usage()
        }
        base = args.remove(1);
        args.remove(0);
    }
    if args.is_empty() {
        usage()
    }
    let command = args.remove(0);
    let api = Api::new(&base, Api::saved_session());
    match run(&api, &command, args) {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}
