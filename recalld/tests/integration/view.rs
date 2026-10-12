//! The rendered view holds render's lines for every clip, renders a clip again
//! only when what it is made from moved, keeps the ids of lines that did not
//! change, and shows a live turn until a transcribed clip covers it.

use audiocore::job::Kind;
use recalld::view::{self, Refreshed};
use rusqlite::Connection;
use transcript::{Act, Instant, Name, Span, Text};

const FILE: &str = "usb-20261004T120000.flac";

struct Fleet {
    dir: tempfile::TempDir,
    meaning: Connection,
    ingest: Connection,
}

impl Fleet {
    fn refresh(&self, all: bool) -> Refreshed {
        view::refresh(&self.meaning, &self.ingest, self.dir.path(), all).expect("refresh")
    }

    /// Visible lines: (id, text, tier, model).
    fn lines(&self) -> Vec<(i64, String, String, String)> {
        self.meaning
            .prepare(
                "SELECT id, text, tier, asr_model FROM rendered_lines
                 WHERE hidden_reason IS NULL ORDER BY start_utc, id",
            )
            .expect("prepare")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("rows")
    }

    fn hidden(&self) -> Vec<(String, String)> {
        self.meaning
            .prepare(
                "SELECT text, hidden_reason FROM rendered_lines
                 WHERE hidden_reason IS NOT NULL ORDER BY start_utc",
            )
            .expect("prepare")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("rows")
    }

    fn clip(&self) -> transcript::Clip {
        recalld::clips::by_filename(&self.ingest, FILE)
            .expect("clip")
            .expect("a clip")
    }

    fn at(&self, s: f64) -> Instant {
        self.clip().start.plus_seconds(s).expect("in range")
    }
}

fn segment(start: f64, end: f64, text: &str) -> String {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let step = (end - start) / tokens.len() as f64;
    // A pause between words, as Whisper's timings have.
    let words: Vec<String> = tokens
        .iter()
        .enumerate()
        .map(|(i, t)| {
            format!(
                r#"{{"start":{},"end":{},"word":" {t}","probability":0.9}}"#,
                start + step * i as f64,
                start + step * (i + 1) as f64 - 0.1
            )
        })
        .collect();
    format!(
        r#"{{"start":{start},"end":{end},"text":" {text}","words":[{}]}}"#,
        words.join(",")
    )
}

/// One usb clip, transcribed as two sentences, with its audio row.
fn fleet() -> Fleet {
    let dir = tempfile::tempdir().expect("tmp");
    let meaning = recalld::work::open_write(dir.path()).expect("meaning");
    recalld::meaning_schema::ensure(&meaning).expect("schema");
    let ingest = recalld::store::open(dir.path()).expect("ingest");
    recalld::store::insert(
        &ingest,
        &recalld::store::Row {
            source: "usb".into(),
            filename: FILE.into(),
            start_utc: "2026-10-04T12:00:00Z".into(),
            bytes: 1,
            sha256: "x".into(),
            received_utc: "2026-10-04T12:00:00Z".into(),
            sent_utc: None,
        },
    )
    .expect("segment");
    meaning
        .execute_batch(&format!(
            "INSERT INTO sources (id, name, kind) VALUES ('usb', 'usb', 'coreaudio');
             INSERT INTO audio_segments (source_id, path, start_utc, end_utc, sample_rate, channels)
             VALUES ('usb', '{}', '2026-10-04T12:00:00+00:00', '2026-10-04T12:01:00+00:00', 48000, 1);",
            dir.path().join("ingest/usb").join(FILE).display()
        ))
        .expect("audio row");
    let result = format!(
        r#"{{"ok":true,"result":{{"language":"nl","segments":[{},{}]}}}}"#,
        segment(1.0, 3.0, "goede morgen."),
        segment(10.0, 12.0, "hoe gaat het?")
    );
    ingest
        .execute(
            "INSERT INTO jobs (kind, filename, state, created_utc, done_utc, result)
             VALUES (?1, ?2, 'done', '2026-10-04T12:02:00Z', '2026-10-04T12:02:00Z', ?3)",
            (Kind::TranscribeSegment, FILE, result),
        )
        .expect("result");
    Fleet {
        dir,
        meaning,
        ingest,
    }
}

#[test]
fn every_clip_is_rendered_once_and_again_only_when_its_inputs_move() {
    let f = fleet();

    let first = f.refresh(false);
    assert_eq!((first.rendered, first.unchanged), (1, 0));
    let lines = f.lines();
    let texts: Vec<&str> = lines.iter().map(|l| l.1.as_str()).collect();
    assert_eq!(texts, ["goede morgen.", "hoe gaat het?"]);
    assert!(lines.iter().all(|l| l.2 == "transcribed"));

    let again = f.refresh(false);
    assert_eq!((again.rendered, again.unchanged), (0, 1));
    assert_eq!(f.refresh(true).rendered, 1, "all renders every clip");
    assert_eq!(f.lines(), lines, "the same lines, the same ids");
}

#[test]
fn a_persons_words_show_and_the_untouched_line_keeps_its_id() {
    let f = fleet();
    f.refresh(false);
    let before = f.lines();
    let clip = f.clip();
    let act = Act::Words {
        clip: clip.id,
        span: Span::new(f.at(10.0), f.at(12.0)).expect("ordered"),
        over: Span::new(f.at(10.0), f.at(12.0)).expect("ordered"),
        text: Text::new("hoe gaat het met je?").expect("text"),
        checked: true,
    };
    recalld::edit_log::append(&f.meaning, f.at(100.0), &act, None).expect("append");

    assert_eq!(
        f.refresh(false).rendered,
        1,
        "the act moved the clip's inputs"
    );
    let after = f.lines();
    assert_eq!(after[0], before[0], "the untouched line keeps its id");
    assert_eq!(after[1].1, "hoe gaat het met je?");
    assert_eq!(
        (after[1].2.as_str(), after[1].3.as_str()),
        ("corrected", "human")
    );
    assert_eq!(
        f.hidden(),
        [(
            "hoe gaat het?".to_owned(),
            "replaced by a person's words".to_owned()
        )]
    );
}

#[test]
fn a_line_split_in_two_takes_new_ids_and_never_one_another_line_had() {
    let f = fleet();
    f.refresh(false);
    let before = f.lines();
    let clip = f.clip();
    // Naming "goede" alone breaks "goede morgen." in two.
    let act = Act::Speaker {
        clip: clip.id,
        span: Span::new(f.at(1.0), f.at(1.95)).expect("ordered"),
        name: Name::new("Anna").expect("name"),
        enrol: false,
    };
    recalld::edit_log::append(&f.meaning, f.at(100.0), &act, None).expect("append");

    assert_eq!(f.refresh(false).rendered, 1);
    let after = f.lines();
    let texts: Vec<&str> = after.iter().map(|l| l.1.as_str()).collect();
    assert_eq!(texts, ["goede", "morgen.", "hoe gaat het?"]);
    assert_eq!(after[2], before[1], "the untouched line keeps its id");
    let newest = before.iter().map(|l| l.0).max().expect("lines");
    assert!(
        after[..2].iter().all(|l| l.0 > newest),
        "a new line never takes an id that was handed out: {before:?} {after:?}"
    );
}

#[test]
fn render_clips_shows_an_act_at_once() {
    let f = fleet();
    f.refresh(false);
    let before = f.lines();
    let clip = f.clip();
    let act = Act::NoSpeech {
        clip: clip.id,
        span: Span::new(f.at(0.5), f.at(3.5)).expect("ordered"),
    };
    recalld::edit_log::append(&f.meaning, f.at(100.0), &act, None).expect("append");

    view::render_clips(&f.meaning, &f.ingest, f.dir.path(), &[clip.id]).expect("render");

    let texts: Vec<String> = f.lines().into_iter().map(|l| l.1).collect();
    assert_eq!(texts, ["hoe gaat het?"]);
    assert_eq!(
        f.hidden(),
        [("goede morgen.".to_owned(), "nobody spoke".to_owned())]
    );
    let hidden_id: i64 = f
        .meaning
        .query_row(
            "SELECT id FROM rendered_lines WHERE hidden_reason IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .expect("hidden row");
    assert_eq!(hidden_id, before[0].0, "hidden, the line keeps its id");
    assert_eq!(f.refresh(false).rendered, 0, "already current");
}

#[test]
fn a_live_turn_shows_until_a_transcribed_clip_covers_it() {
    let f = fleet();
    f.meaning
        .execute_batch(
            "INSERT INTO live_lines (start_utc, end_utc, text, asr_model)
             VALUES ('2026-10-04T12:00:30+00:00', '2026-10-04T12:00:32+00:00', 'covered', 'live'),
                    ('2026-10-04T12:05:00+00:00', '2026-10-04T12:05:02+00:00', 'still live', 'live');",
        )
        .expect("live turns");

    assert_eq!(f.refresh(false).live_shown, 1);
    let live: Vec<(String, String)> = f
        .lines()
        .into_iter()
        .filter(|l| l.2 == "live")
        .map(|l| (l.1, l.3))
        .collect();
    assert_eq!(live, [("still live".to_owned(), "live".to_owned())]);
}

#[test]
fn a_clip_run_again_shows_its_words_until_the_new_ones_land_and_a_cancel_keeps_them() {
    let f = crate::fixture::Fleet::new();
    let clip = f.clip(
        "usb",
        "coreaudio",
        "20261004T120000",
        &[(1.0, 3.0, "oude woorden")],
        None,
    );
    f.refresh();
    let now = audiocore::instant::Stamp::now();
    let asked = recalld::retranscribe::request(&f.ingest, std::slice::from_ref(&clip.filename))
        .expect("request");
    assert_eq!(asked.queued.len(), 1);

    f.refresh();
    assert_eq!(f.texts(), ["oude woorden"], "still shown while it waits");

    assert!(recalld::retranscribe::cancel(&f.ingest, &clip.filename, &now).expect("cancel"));
    let done: Option<String> = f
        .ingest
        .query_row(
            "SELECT done_utc FROM jobs WHERE filename = ?1",
            [&clip.filename],
            |r| r.get(0),
        )
        .expect("job");
    assert!(done.is_some(), "done again, with the result it kept");

    recalld::retranscribe::request(&f.ingest, std::slice::from_ref(&clip.filename)).expect("again");
    f.ingest
        .execute(
            "UPDATE jobs SET state = 'done', done_utc = '2026-10-04T13:00:00Z', result = ?2
             WHERE filename = ?1",
            (
                &clip.filename,
                format!(
                    r#"{{"ok":true,"result":{{"language":"nl","segments":[{}]}}}}"#,
                    crate::fixture::segment(1.0, 3.0, "nieuwe woorden")
                ),
            ),
        )
        .expect("lands");
    f.refresh();
    assert_eq!(f.texts(), ["nieuwe woorden"]);
}

#[test]
fn a_clip_whose_rows_will_not_write_is_reported_and_the_rest_still_render() {
    // A second recording, a minute later.
    const SECOND: &str = "usb-20261004T120100.flac";
    let f = fleet();
    recalld::store::insert(
        &f.ingest,
        &recalld::store::Row {
            source: "usb".into(),
            filename: SECOND.into(),
            start_utc: "2026-10-04T12:01:00Z".into(),
            bytes: 1,
            sha256: "y".into(),
            received_utc: "2026-10-04T12:01:00Z".into(),
            sent_utc: None,
        },
    )
    .expect("segment");
    f.meaning
        .execute(
            "INSERT INTO audio_segments (source_id, path, start_utc, end_utc, sample_rate, channels)
             VALUES ('usb', ?1, '2026-10-04T12:01:00+00:00', '2026-10-04T12:02:00+00:00', 48000, 1)",
            [f.dir.path().join("ingest/usb").join(SECOND).display().to_string()],
        )
        .expect("audio row");
    f.ingest
        .execute(
            "INSERT INTO jobs (kind, filename, state, created_utc, done_utc, result)
             VALUES (?1, ?2, 'done', '2026-10-04T12:03:00Z', '2026-10-04T12:03:00Z', ?3)",
            (
                Kind::TranscribeSegment,
                SECOND,
                format!(
                    r#"{{"ok":true,"result":{{"language":"nl","segments":[{}]}}}}"#,
                    segment(1.0, 3.0, "tot straks.")
                ),
            ),
        )
        .expect("result");
    // The first recording's rows refuse to write, as this morning's did.
    let broken = f.clip().id.stored();
    f.meaning
        .execute_batch(&format!(
            "CREATE TRIGGER refuse BEFORE INSERT ON rendered_lines WHEN NEW.clip_id = {broken}
             BEGIN SELECT RAISE(ABORT, 'refused'); END;"
        ))
        .expect("trigger");

    let done = f.refresh(false);

    assert_eq!(done.rendered, 1, "the other recording rendered");
    assert_eq!(done.failed.len(), 1);
    assert_eq!(done.failed[0].0, broken);
    assert!(done.failed[0].1.contains("refused"), "{:?}", done.failed);
    let texts: Vec<String> = f.lines().into_iter().map(|l| l.1).collect();
    assert_eq!(texts, ["tot straks."]);

    f.meaning
        .execute_batch("DROP TRIGGER refuse")
        .expect("drop");
    let again = f.refresh(false);
    assert_eq!(
        (again.rendered, again.failed.len()),
        (1, 0),
        "retried, now current"
    );
}

/// Commits on the meaning plane while `f` runs.
fn commits(f: &Fleet, run: impl FnOnce()) -> usize {
    let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = count.clone();
    f.meaning
        .commit_hook(Some(move || {
            counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            false
        }))
        .expect("hook");
    run();
    f.meaning.commit_hook(None::<fn() -> bool>).expect("unhook");
    count.load(std::sync::atomic::Ordering::Relaxed)
}

#[test]
fn a_full_render_with_nothing_changed_writes_nothing() {
    // Every start renders every clip, in case render's rules changed; almost
    // always they did not, and rewriting 100k lines took minutes.
    let f = fleet();
    f.refresh(false);
    let before = f.lines();
    let fts: i64 = f
        .meaning
        .query_row("SELECT count(*) FROM rendered_fts", [], |r| r.get(0))
        .expect("fts");

    let mut done = Refreshed::default();
    let wrote = commits(&f, || done = f.refresh(true));

    assert_eq!(done.rendered, 1, "rendered, to compare");
    assert_eq!(done.written, 0, "nothing changed, nothing written");
    assert_eq!(wrote, 0, "not a single commit");
    assert_eq!(f.lines(), before);
    let after: i64 = f
        .meaning
        .query_row("SELECT count(*) FROM rendered_fts", [], |r| r.get(0))
        .expect("fts");
    assert_eq!(after, fts);
}

#[test]
fn a_refresh_writes_its_clips_in_one_commit_not_one_each() {
    // A commit is an fsync: one per clip was 16 minutes on Isis's disk.
    let f = fleet();
    for minute in 1..=3 {
        let name = format!("usb-20261004T12{minute:02}00.flac");
        recalld::store::insert(
            &f.ingest,
            &recalld::store::Row {
                source: "usb".into(),
                filename: name.clone(),
                start_utc: format!("2026-10-04T12:{minute:02}:00Z"),
                bytes: 1,
                sha256: "y".into(),
                received_utc: "2026-10-04T12:10:00Z".into(),
                sent_utc: None,
            },
        )
        .expect("segment");
        f.meaning
            .execute(
                "INSERT INTO audio_segments (source_id, path, start_utc, end_utc, sample_rate, channels)
                 VALUES ('usb', ?1, ?2, ?2, 48000, 1)",
                (
                    f.dir.path().join("ingest/usb").join(&name).display().to_string(),
                    format!("2026-10-04T12:{minute:02}:00+00:00"),
                ),
            )
            .expect("audio row");
        f.ingest
            .execute(
                "INSERT INTO jobs (kind, filename, state, created_utc, done_utc, result)
                 VALUES (?1, ?2, 'done', '2026-10-04T12:10:00Z', '2026-10-04T12:10:00Z', ?3)",
                (
                    Kind::TranscribeSegment,
                    &name,
                    format!(
                        r#"{{"ok":true,"result":{{"language":"nl","segments":[{}]}}}}"#,
                        segment(1.0, 3.0, "nog een.")
                    ),
                ),
            )
            .expect("result");
    }

    let mut done = Refreshed::default();
    let wrote = commits(&f, || done = f.refresh(false));

    assert_eq!((done.rendered, done.written), (4, 4));
    assert_eq!(wrote, 1);
}

#[test]
fn two_hidden_lines_with_one_span_keep_their_ids_and_are_not_rewritten() {
    // As a real clip has it: a segment ending in zero-length words, then an
    // empty segment at the same instant, all inside "nobody spoke". Hidden
    // twice over one instant, once for each reason; matched by span alone, one
    // of the two took a new id at every render.
    let f = fleet();
    let word = |start: f64, end: f64, w: &str| {
        format!(r#"{{"start":{start},"end":{end},"word":" {w}","probability":0.9}}"#)
    };
    let result = format!(
        r#"{{"ok":true,"result":{{"language":"nl","segments":[{{"start":1.0,"end":3.0,"text":" goede morgen dan","words":[{},{},{}]}},{{"start":3.0,"end":3.0,"text":"","words":[]}},{}]}}}}"#,
        word(1.0, 3.0, "goede"),
        word(3.0, 3.0, "morgen"),
        word(3.0, 3.0, "dan"),
        segment(10.0, 12.0, "hoe gaat het?")
    );
    f.ingest
        .execute("UPDATE jobs SET result = ?1", [result])
        .expect("result");
    let clip = f.clip();
    let act = Act::NoSpeech {
        clip: clip.id,
        span: Span::new(f.at(0.5), f.at(3.5)).expect("ordered"),
    };
    recalld::edit_log::append(&f.meaning, f.at(100.0), &act, None).expect("append");
    f.refresh(false);
    let ids = || -> Vec<i64> {
        f.meaning
            .prepare("SELECT id FROM rendered_lines ORDER BY id")
            .expect("prepare")
            .query_map([], |r| r.get(0))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("rows")
    };
    let shared: i64 = f
        .meaning
        .query_row(
            "SELECT count(*) FROM (SELECT 1 FROM rendered_lines GROUP BY start_utc, end_utc
                 HAVING count(*) > 1)",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(shared, 1, "the case: two lines over one span");
    let before = ids();

    let again = f.refresh(true);

    assert_eq!(again.written, 0, "nothing changed");
    assert_eq!(ids(), before, "every line keeps its id");
}

#[test]
fn an_exclusion_made_in_the_app_reads_excluded_and_one_recovered_from_the_old_tables_left_out() {
    // Only the first gets a row with an undo in the app; the recovered ones
    // (second copies, played test speech) were nobody's choice here.
    let f = fleet();
    let clip = f.clip();
    let exclude = |from: f64, to: f64| Act::Exclude {
        clip: clip.id,
        span: Span::new(f.at(from), f.at(to)).expect("ordered"),
    };
    recalld::edit_log::append(&f.meaning, f.at(100.0), &exclude(0.5, 3.5), Some("Hide(7)"))
        .expect("recovered");
    recalld::edit_log::append(&f.meaning, f.at(200.0), &exclude(9.5, 12.5), None)
        .expect("in the app");

    f.refresh(false);

    assert_eq!(
        f.hidden(),
        [
            ("goede morgen.".to_owned(), "left out".to_owned()),
            ("hoe gaat het?".to_owned(), "excluded".to_owned()),
        ]
    );
}
