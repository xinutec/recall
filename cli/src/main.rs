//! Argument parsing and dispatch for `recall-cli`.
//!
//! Hand-rolled, like `runner` and `audiod`: the workspace carries no argument
//! parser.

use cli::api::{Api, Error};
use cli::render;

/// The fleet: the system of record, and the only thing this talks to.
const DEFAULT_API: &str = "http://10.100.0.2:8000";
const DEFAULT_LIMIT: i64 = 100;
/// A conversation breaks after a silence longer than this. Matches
/// `recalld::conversations::DEFAULT_GAP_SECONDS`, but sent explicitly.
const DEFAULT_GAP: f64 = 300.0;

fn usage() -> ! {
    eprintln!(
        "usage: recall-cli [--api <url>] <command> [args]\n\
         \n\
         commands:\n\
         \x20 search <query> [--limit N]   full-text search the archive\n\
         \x20 show <id> [<id>...]          diagnostic dump of specific turns\n\
         \x20 timeline [--limit N]         the newest turns\n\
         \x20 review [--limit N]           turns the model was least sure of\n\
         \x20 sessions                     every uploaded session\n\
         \x20 transcript <session>         one session, read through\n\
         \x20 day <YYYY-MM-DD> [--conv N]  a day's conversations, or read one\n\
         \x20 sources                      every recorder the fleet knows\n\
         \x20 capture                      whether the recorders are running\n\
         \x20 correct <id> <text> --apply             replace a turn's text\n\
         \x20 correct --session <id> --fix OLD=>NEW    ...by the words instead\n\
         \x20 no-speech <id> --apply                  nobody spoke: hide the turn\n\
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

/// Pull `--flag VALUE` out of the remaining arguments, leaving the rest.
fn take_value(args: &mut Vec<String>, flag: &str) -> Option<String> {
    let at = args.iter().position(|a| a == flag)?;
    let value = args.get(at + 1).cloned();
    if value.is_none() {
        usage()
    }
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

/// `Ok(false)` means nothing matched, which exits 1 so scripts can branch on it.
fn run(api: &Api, command: &str, mut args: Vec<String>) -> Result<bool, Error> {
    match command {
        "search" => search(api, &mut args),
        "show" => show(api, &args),
        "timeline" => timeline(api, &mut args),
        "review" => review(api, &mut args),
        "sessions" => sessions(api),
        "transcript" => transcript(api, &args),
        "day" => day(api, &mut args),
        "sources" => sources(api),
        "capture" => capture(api),
        "correct" => correct(api, &mut args),
        "no-speech" => no_speech(api, &mut args),
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

fn sessions(api: &Api) -> Result<bool, Error> {
    let items = api.sessions()?;
    println!("{}", render::sessions(&items));
    Ok(!items.is_empty())
}

fn transcript(api: &Api, args: &[String]) -> Result<bool, Error> {
    let Some(source) = args.first() else { usage() };
    let export = api.session_transcript(source)?;
    if export.turns.is_empty() {
        println!("no transcript for session {source:?}");
        return Ok(false);
    }
    println!("{}", render::export(&export));
    Ok(true)
}

/// A day of the always-on stream: list its conversations, or read one.
///
/// The window is the local day, not the UTC one (see [`cli::day`]).
fn day(api: &Api, args: &mut Vec<String>) -> Result<bool, Error> {
    let which = take_value(args, "--conv");
    let limit = take_limit(args);
    let Some(date) = args.first().cloned() else {
        usage()
    };
    let Some((after, before)) = cli::day::bounds(&date) else {
        eprintln!("day must be YYYY-MM-DD, 'today' or 'yesterday', not {date:?}");
        std::process::exit(2)
    };
    let found = api.conversations(&after, &before, DEFAULT_GAP, limit)?;
    if found.items.is_empty() {
        println!("no conversations on {date}");
        return Ok(false);
    }
    let Some(which) = which else {
        println!("{}", render::conversations(&date, &found.items));
        if found.has_more {
            println!("\n… the page filled; raise --limit to see the rest of the day");
        }
        return Ok(true);
    };
    let n = if which == "last" {
        found.items.len()
    } else if let Ok(n) = which.parse::<usize>() {
        n
    } else {
        eprintln!("--conv must be a number or 'last', not {which:?}");
        std::process::exit(2)
    };
    let Some(conv) = n.checked_sub(1).and_then(|i| found.items.get(i)) else {
        println!("no conversation {n} on {date} (have {})", found.items.len());
        return Ok(false);
    };
    println!(
        "{}",
        render::conversation(&format!("{date} · conversation {n}"), conv)
    );
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

/// Read-only: what the recorders are doing, to check before anything that
/// could pause them.
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

/// ⚠ A write. It reaches the corrections corpus, the only part of the
/// archive not re-derivable from audio, so it is a dry run unless `--apply` is
/// given, and the change is printed either way.
///
/// Two forms:
///
///     correct <id> "<the whole corrected line>"
///     correct --session <id> --fix "OLD=>NEW" [--fix ...]
///
/// The second corrects by the visible words, without looking up an id.
fn correct(api: &Api, args: &mut Vec<String>) -> Result<bool, Error> {
    let apply = take_flag(args, "--apply");
    let session = take_value(args, "--session");
    let mut fixes = Vec::new();
    while let Some(raw) = take_value(args, "--fix") {
        let Some((old, new)) = raw.split_once("=>") else {
            eprintln!("--fix must be OLD=>NEW, not {raw:?}");
            std::process::exit(2)
        };
        fixes.push((old.to_owned(), new.to_owned()));
    }
    let outcome = match session {
        Some(session) => correct_by_substring(api, &session, &fixes, apply),
        None if fixes.is_empty() => correct_by_id(api, args, apply),
        // Mixing the forms would mean guessing which the caller meant.
        None => usage(),
    }?;
    if !apply {
        println!("\nDRY-RUN only — nothing written. Re-run with --apply to commit.");
    }
    Ok(outcome)
}

fn correct_by_id(api: &Api, args: &[String], apply: bool) -> Result<bool, Error> {
    let (Some(id), Some(text)) = (
        args.first().and_then(|a| a.parse::<i64>().ok()),
        args.get(1),
    ) else {
        usage()
    };
    let Some(current) = api.transcripts(&[id])?.into_iter().next() else {
        println!("no turn {id}");
        return Ok(false);
    };
    show_change(&current, text);
    if apply {
        let new_id = api.correct(current.id, text)?;
        println!("   -> applied as new turn #{new_id}");
    }
    Ok(true)
}

/// ⚠ The other write, gated like `correct`: the turn is hidden and its words
/// filed as the model's invention.
fn no_speech(api: &Api, args: &mut Vec<String>) -> Result<bool, Error> {
    let apply = take_flag(args, "--apply");
    let Some(id) = args.first().and_then(|a| a.parse::<i64>().ok()) else {
        usage()
    };
    let Some(current) = api.transcripts(&[id])?.into_iter().next() else {
        println!("no turn {id}");
        return Ok(false);
    };
    show_change(&current, "(nobody spoke)");
    if apply {
        api.no_speech(current.id)?;
        println!("   -> hidden");
    } else {
        println!("\nDRY-RUN only — nothing written. Re-run with --apply to commit.");
    }
    Ok(true)
}

/// A substring matching more than one turn is skipped, not guessed at: a
/// correction on the wrong turn cannot be detected later.
fn correct_by_substring(
    api: &Api,
    session: &str,
    fixes: &[(String, String)],
    apply: bool,
) -> Result<bool, Error> {
    if fixes.is_empty() {
        usage()
    }
    let turns = api.source_turns(session, 1000)?;
    if turns.is_empty() {
        println!("no current turns for session {session:?}");
        return Ok(false);
    }
    println!(
        "session {session}: {} turns  ::  mode = {}\n",
        turns.len(),
        if apply { "APPLY" } else { "DRY-RUN" }
    );
    let mut ok = true;
    for (old, new) in fixes {
        let matches: Vec<&cli::api::Turn> = turns
            .iter()
            .filter(|t| t.text.contains(old.as_str()))
            .collect();
        let [only] = matches.as_slice() else {
            println!(
                "!! {old:?} matched {} turn(s) (need exactly 1) -- SKIP\n",
                matches.len()
            );
            ok = false;
            continue;
        };
        let corrected = only.text.replace(old.as_str(), new);
        show_change(only, &corrected);
        if apply {
            let new_id = api.correct(only.id, &corrected)?;
            println!("   -> applied as new turn #{new_id}");
        }
        println!();
    }
    Ok(ok)
}

fn show_change(turn: &cli::api::Turn, corrected: &str) {
    println!("#{}  [{}]", turn.id, render::who(turn));
    println!("   OLD: {}", turn.text);
    println!("   NEW: {corrected}");
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
