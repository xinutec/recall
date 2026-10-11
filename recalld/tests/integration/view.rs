//! The rendered view holds render's lines for every clip, renders a clip again
//! only when what it is made from moved, keeps the ids of lines that did not
//! change, and shows a live turn until a transcribed clip covers it.

use audiocore::job::Kind;
use recalld::view::{self, Refreshed};
use rusqlite::Connection;
use transcript::{Act, Instant, Span, Text};

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
fn render_clips_shows_an_act_at_once() {
    let f = fleet();
    f.refresh(false);
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
    assert_eq!(f.refresh(false).rendered, 0, "already current");
}

#[test]
fn a_live_turn_shows_until_a_transcribed_clip_covers_it() {
    let f = fleet();
    f.meaning
        .execute_batch(
            "INSERT INTO transcript_segments (start_utc, end_utc, text, asr_model)
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
    let asked =
        recalld::retranscribe::request(&f.ingest, std::slice::from_ref(&clip.filename), &now)
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

    recalld::retranscribe::request(&f.ingest, std::slice::from_ref(&clip.filename), &now)
        .expect("again");
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
