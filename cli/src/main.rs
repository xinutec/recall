//! Argument parsing and dispatch for `recall-cli`.

use std::path::PathBuf;

use clap::{ArgGroup, Parser, Subcommand};
use cli::api::{Api, Error};
use cli::render;

/// The fleet: the system of record, and the only thing this talks to.
const DEFAULT_API: &str = "https://recall.xinutec.org";
const DEFAULT_LIMIT: i64 = 100;
/// A conversation breaks after a silence longer than this. Matches
/// `recalld::conversations::DEFAULT_GAP_SECONDS`, but sent explicitly.
const DEFAULT_GAP: f64 = 300.0;

/// Read and correct the recall archive, at the system of record.
///
/// There is no option to read a local database: a second answer nobody can
/// tell from the first is what this replaced. Reading transcripts needs a
/// browsing session; `capture` and `sources` do not.
#[derive(Parser)]
#[command(name = "recall-cli", after_help = cli::api::HOW_TO_SIGN_IN)]
struct Cli {
    /// The system of record [default: `RECALL_API`, else the fleet].
    #[arg(long, value_name = "URL")]
    api: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Full-text search the archive.
    Search {
        #[arg(required = true, allow_hyphen_values = true)]
        query: Vec<String>,
        #[command(flatten)]
        limit: Limit,
    },
    /// Diagnostic dump of specific turns.
    Show {
        #[arg(required = true)]
        ids: Vec<i64>,
    },
    /// The newest turns.
    Timeline {
        #[command(flatten)]
        limit: Limit,
    },
    /// Turns the model was least sure of.
    Review {
        #[command(flatten)]
        limit: Limit,
    },
    /// Every uploaded session.
    Sessions,
    /// One session, read through.
    Transcript { session: String },
    /// A day's conversations, or read one.
    Day {
        /// YYYY-MM-DD, `today` or `yesterday`.
        date: String,
        /// Read conversation N, or `last`.
        #[arg(long, value_parser = conversation)]
        conv: Option<Conv>,
        #[command(flatten)]
        limit: Limit,
    },
    /// Every recorder the fleet knows.
    Sources,
    /// Whether the recorders are running.
    Capture,
    /// Replace a turn's text: by id, or by the words within a session.
    #[command(group(ArgGroup::new("form").required(true).args(["id", "session"])))]
    Correct {
        #[arg(requires = "text", conflicts_with = "session")]
        id: Option<i64>,
        text: Option<String>,
        #[arg(long, requires = "fix")]
        session: Option<String>,
        /// OLD=>NEW; repeatable.
        #[arg(long, value_name = "OLD=>NEW", value_parser = fix, requires = "session")]
        fix: Vec<(String, String)>,
        #[command(flatten)]
        apply: Apply,
    },
    /// Nobody spoke: hide the turn.
    NoSpeech {
        id: i64,
        /// Show it again.
        #[arg(long)]
        undo: bool,
        #[command(flatten)]
        apply: Apply,
    },
    /// Transcribe clips again; a person's lines stay.
    #[command(group(ArgGroup::new("clips_from").args(["clips", "from", "candidates"]).required(true)))]
    Retranscribe {
        clips: Vec<String>,
        /// The clips, one per line (the first column, so `--candidates`
        /// output reads as it is).
        #[arg(long, value_name = "LIST")]
        from: Option<PathBuf>,
        /// Take a retranscription back.
        #[arg(long)]
        undo: bool,
        /// List the clips that lost speech to a loop, instead.
        #[arg(long, conflicts_with_all = ["clips", "from", "undo", "apply"])]
        candidates: bool,
        /// With `--candidates`, the least looped speech, in seconds.
        #[arg(long, default_value_t = 1.0, requires = "candidates")]
        min: f64,
        #[command(flatten)]
        apply: Apply,
    },
}

#[derive(clap::Args)]
struct Limit {
    #[arg(long = "limit", default_value_t = DEFAULT_LIMIT)]
    n: i64,
}

#[derive(clap::Args)]
struct Apply {
    /// Write; without it, a dry run.
    #[arg(long = "apply")]
    yes: bool,
}

#[derive(Clone)]
enum Conv {
    Last,
    Nth(usize),
}

fn conversation(raw: &str) -> Result<Conv, String> {
    match raw {
        "last" => Ok(Conv::Last),
        n => n
            .parse()
            .map(Conv::Nth)
            .map_err(|_| format!("a number or 'last', not {n:?}")),
    }
}

fn fix(raw: &str) -> Result<(String, String), String> {
    raw.split_once("=>")
        .map(|(old, new)| (old.to_owned(), new.to_owned()))
        .ok_or_else(|| format!("OLD=>NEW, not {raw:?}"))
}

/// `Ok(false)` means nothing matched, which exits 1 so scripts can branch on it.
fn run(api: &Api, command: Command) -> Result<bool, Error> {
    match command {
        Command::Search { query, limit } => search(api, &query.join(" "), limit.n),
        Command::Show { ids } => show(api, &ids),
        Command::Timeline { limit } => timeline(api, limit.n),
        Command::Review { limit } => review(api, limit.n),
        Command::Sessions => sessions(api),
        Command::Transcript { session } => transcript(api, &session),
        Command::Day { date, conv, limit } => day(api, &date, conv, limit.n),
        Command::Sources => sources(api),
        Command::Capture => capture(api),
        Command::Correct {
            id,
            text,
            session,
            fix,
            apply,
        } => correct(api, id.zip(text), session, &fix, apply.yes),
        Command::NoSpeech { id, undo, apply } => no_speech(api, id, undo, apply.yes),
        Command::Retranscribe {
            clips,
            from,
            undo,
            candidates,
            min,
            apply,
        } => {
            if candidates {
                retranscribe_candidates(api, min)
            } else {
                retranscribe(api, clips, from, undo, apply.yes)
            }
        }
    }
}

fn search(api: &Api, query: &str, limit: i64) -> Result<bool, Error> {
    let hits = api.search(query, limit)?;
    if hits.is_empty() {
        println!("no matches for {query:?}");
        return Ok(false);
    }
    for hit in &hits {
        println!("{}", render::hit(hit));
    }
    Ok(true)
}

fn show(api: &Api, ids: &[i64]) -> Result<bool, Error> {
    let turns = api.transcripts(ids)?;
    if turns.is_empty() {
        println!("no turns found for {ids:?}");
        return Ok(false);
    }
    println!("{}", render::details(ids, &turns));
    Ok(true)
}

fn timeline(api: &Api, limit: i64) -> Result<bool, Error> {
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

fn review(api: &Api, limit: i64) -> Result<bool, Error> {
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

fn transcript(api: &Api, source: &str) -> Result<bool, Error> {
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
fn day(api: &Api, date: &str, which: Option<Conv>, limit: i64) -> Result<bool, Error> {
    let Some((after, before)) = cli::day::bounds(date) else {
        eprintln!("day must be YYYY-MM-DD, 'today' or 'yesterday', not {date:?}");
        std::process::exit(2)
    };
    let found = api.conversations(&after, &before, DEFAULT_GAP, limit)?;
    if found.items.is_empty() {
        println!("no conversations on {date}");
        return Ok(false);
    }
    let Some(which) = which else {
        println!("{}", render::conversations(date, &found.items));
        if found.has_more {
            println!("\n… the page filled; raise --limit to see the rest of the day");
        }
        return Ok(true);
    };
    let n = match which {
        Conv::Last => found.items.len(),
        Conv::Nth(n) => n,
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
/// By id and text, or by `OLD=>NEW` fixes within a session; clap refuses a
/// mix, which would mean guessing which the caller meant.
fn correct(
    api: &Api,
    by_id: Option<(i64, String)>,
    session: Option<String>,
    fixes: &[(String, String)],
    apply: bool,
) -> Result<bool, Error> {
    let outcome = match (by_id, session) {
        (Some((id, text)), _) => correct_by_id(api, id, &text, apply),
        (None, Some(session)) => correct_by_substring(api, &session, fixes, apply),
        (None, None) => unreachable!("clap requires one form"),
    }?;
    if !apply {
        println!("\nDRY-RUN only — nothing written. Re-run with --apply to commit.");
    }
    Ok(outcome)
}

fn correct_by_id(api: &Api, id: i64, text: &str, apply: bool) -> Result<bool, Error> {
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

/// Clips whose speech was lost under repetition loops, most lost first.
fn retranscribe_candidates(api: &Api, min: f64) -> Result<bool, Error> {
    let found = api.retranscribe_candidates(min)?;
    let total: f64 = found.iter().map(|c| c.looped_speech_s).sum();
    for c in &found {
        println!("{}\t{:.1}", c.filename, c.looped_speech_s);
    }
    eprintln!(
        "{} clip(s), {:.0} min of speech under loops",
        found.len(),
        total / 60.0
    );
    Ok(!found.is_empty())
}

/// Transcribe clips again: named, or one per line in `--from LIST`.
fn retranscribe(
    api: &Api,
    mut clips: Vec<String>,
    from: Option<PathBuf>,
    undo: bool,
    apply: bool,
) -> Result<bool, Error> {
    if let Some(list) = from {
        let Ok(text) = std::fs::read_to_string(&list) else {
            eprintln!("cannot read {}", list.display());
            std::process::exit(2)
        };
        // The first column: `--candidates` prints the seconds beside each name.
        clips.extend(
            text.lines()
                .filter_map(|l| l.split_whitespace().next())
                .map(str::to_owned),
        );
    }
    if clips.is_empty() {
        eprintln!("no clips named");
        std::process::exit(2)
    }
    if !apply {
        let verb = if undo {
            "taken back"
        } else {
            "transcribed again"
        };
        println!("{} clip(s) would be {verb}, from {}", clips.len(), clips[0]);
        println!("\nDRY-RUN only — nothing written. Re-run with --apply to commit.");
        return Ok(true);
    }
    if undo {
        for clip in &clips {
            println!("{clip}: {}", api.undo_retranscribe(clip)?);
        }
        return Ok(true);
    }
    let (mut queued, mut skipped) = (0, Vec::new());
    for batch in clips.chunks(cli::api::RETRANSCRIBE_BATCH) {
        let done = api.retranscribe(batch)?;
        queued += done.queued.len();
        skipped.extend(done.skipped);
    }
    println!("{queued} clip(s) back in Whisper's queue");
    for clip in &skipped {
        println!("skipped (no finished transcription): {clip}");
    }
    Ok(skipped.is_empty())
}

/// ⚠ The other write, gated like `correct`: the turn is hidden and its words
/// filed as the model's invention.
fn no_speech(api: &Api, id: i64, undo: bool, apply: bool) -> Result<bool, Error> {
    if undo {
        // A hidden turn is not in the reads, so there is nothing to show first.
        if apply {
            api.undo_no_speech(id)?;
            println!("#{id} shown again");
        } else {
            println!(
                "#{id} would be shown again\n\nDRY-RUN only — nothing written. Re-run with --apply to commit."
            );
        }
        return Ok(true);
    }
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
    let cli = Cli::parse();
    let base = cli
        .api
        .or_else(|| std::env::var("RECALL_API").ok())
        .unwrap_or_else(|| DEFAULT_API.to_owned());
    let api = Api::new(&base, Api::saved_session());
    match run(&api, cli.command) {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}
