//! Voiceprint enrolment's work-list and its writes: which labelled fragments
//! become reference vectors, which clip each is cut from, and which prints a
//! person's later act makes stale.

use crate::fixture::Fleet;
use audiocore::job::Kind;
use chrono::{DateTime, Utc};
use recalld::enrol::{Print, Span, derive_jobs, forget_unwanted, pending, write_pass};
use recalld::store;
use transcript::{Act, Clip, Name, Span as TimeSpan};

fn at(iso: &str) -> DateTime<Utc> {
    iso.parse().expect("t")
}

/// One usb clip with a transcription.
fn fleet() -> (Fleet, Clip) {
    let f = Fleet::new();
    let clip = f.clip(
        "usb",
        "coreaudio",
        "20260910T100000",
        &[(1.0, 8.0, "een twee drie")],
        None,
    );
    (f, clip)
}

/// A person names `from..to` seconds of `clip`; returns the act's id.
fn name(f: &Fleet, clip: &Clip, from: f64, to: f64, who: &str, enrol: bool) -> i64 {
    name_from(f, clip, from, to, who, enrol, None)
}

fn name_from(
    f: &Fleet,
    clip: &Clip,
    from: f64,
    to: f64,
    who: &str,
    enrol: bool,
    origin: Option<&str>,
) -> i64 {
    let s = |x: f64| clip.start.plus_seconds(x).expect("in range");
    recalld::edit_log::append(
        &f.meaning,
        s(100.0),
        &Act::Speaker {
            clip: clip.id,
            span: TimeSpan::new(s(from), s(to)).expect("ordered"),
            name: Name::new(who).expect("name"),
            enrol,
        },
        origin,
    )
    .expect("append")
    .stored()
}

fn spans(f: &Fleet) -> Vec<Span> {
    pending(&f.meaning, &f.ingest)
        .expect("pending")
        .into_iter()
        .map(|(_, s)| s)
        .collect()
}

fn print(f: &Fleet, act: Option<i64>, line: Option<i64>) {
    f.meaning
        .execute_batch("INSERT OR IGNORE INTO speakers (id, name) VALUES (1, 'Someone');")
        .expect("speaker");
    f.meaning
        .execute(
            "INSERT INTO speaker_embeddings (speaker_id, vector, created_utc, source_edit_id, source_segment_id)
             VALUES (1, '[1.0]', '2026-09-10T12:00:00+00:00', ?1, ?2)",
            (act, line),
        )
        .expect("print");
}

fn prints(f: &Fleet) -> i64 {
    f.meaning
        .query_row("SELECT count(*) FROM speaker_embeddings", [], |r| r.get(0))
        .expect("count")
}

#[test]
fn a_labelled_fragment_is_offered_as_seconds_into_its_clip() {
    let (f, clip) = fleet();
    let act = name(&f, &clip, 2.0, 6.0, "Alice", true);

    let work = pending(&f.meaning, &f.ingest).expect("pending");

    assert_eq!(
        work,
        [(
            "usb-20260910T100000.flac".to_owned(),
            Span {
                segment_id: act,
                start_s: 2.0,
                end_s: 6.0
            }
        )]
    );
}

#[test]
fn a_short_fragment_or_one_kept_from_enrolment_is_not_offered() {
    let (f, clip) = fleet();
    name(&f, &clip, 2.0, 2.5, "Alice", true);
    name(&f, &clip, 10.0, 14.0, "Bob", false);

    assert!(spans(&f).is_empty());
}

#[test]
fn a_fragment_starting_a_hair_before_its_clip_seeks_to_zero_not_backwards() {
    // ffmpeg answers a negative seek with the whole clip.
    let (f, clip) = fleet();
    name(&f, &clip, -0.2, 3.0, "Alice", true);

    assert!(spans(&f)[0].start_s.abs() < 1e-9, "seeks to zero");
}

#[test]
fn a_fragment_with_a_print_is_not_offered_again_nor_one_whose_old_line_has_one() {
    let (f, clip) = fleet();
    let printed = name(&f, &clip, 2.0, 6.0, "Alice", true);
    print(&f, Some(printed), None);
    // Recovered from a line that was enrolled before the edit log existed.
    name_from(&f, &clip, 10.0, 14.0, "Bob", true, Some("Line(55)"));
    print(&f, None, Some(55));

    assert!(spans(&f).is_empty());
}

#[test]
fn naming_a_fragment_again_forgets_its_print_even_one_learnt_from_an_old_line() {
    let (f, clip) = fleet();
    let first = name(&f, &clip, 2.0, 6.0, "Alice", true);
    print(&f, Some(first), None);
    name_from(&f, &clip, 10.0, 14.0, "Bob", true, Some("Line(55)"));
    print(&f, None, Some(55));
    assert_eq!(
        forget_unwanted(&f.meaning).expect("forget"),
        0,
        "both wanted"
    );

    let again = name(&f, &clip, 2.0, 6.0, "Carol", true);
    name(&f, &clip, 10.0, 14.0, "Bob", false);

    assert_eq!(forget_unwanted(&f.meaning).expect("forget"), 2);
    assert_eq!(prints(&f), 0);
    let offered: Vec<i64> = spans(&f).iter().map(|s| s.segment_id).collect();
    assert_eq!(offered, [again], "the new naming waits for its own print");
}

#[test]
fn one_job_per_clip_however_many_fragments_it_holds_and_none_without_any() {
    let (f, clip) = fleet();
    f.clip(
        "usb",
        "coreaudio",
        "20260910T100100",
        &[(1.0, 8.0, "vier vijf")],
        None,
    );
    name(&f, &clip, 2.0, 6.0, "Alice", true);
    name(&f, &clip, 10.0, 14.0, "Bob", true);

    let queued =
        derive_jobs(&f.ingest, &f.meaning, at("2026-09-10T12:00:00Z"), 10).expect("derive");

    assert_eq!(queued, 1);
    let jobs: Vec<String> = f
        .ingest
        .prepare("SELECT filename FROM jobs WHERE kind = ?1")
        .expect("prepare")
        .query_map([Kind::EnrollSpeaker], |r| r.get(0))
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows");
    assert_eq!(jobs, ["usb-20260910T100000.flac"]);
}

fn finished(ingest: &rusqlite::Connection, filename: &str, result: &str) {
    ingest
        .execute(
            "INSERT INTO jobs (kind, filename, state, created_utc, done_utc, result)
             VALUES (?1, ?2, 'done', '2026-09-10T12:00:00Z', '2026-09-10T12:05:00Z', ?3)",
            rusqlite::params![Kind::EnrollSpeaker, filename, result],
        )
        .expect("job");
}

fn reply(prints: &[Print]) -> String {
    serde_json::json!({ "ok": true, "result": { "prints": prints } }).to_string()
}

fn ingest_at(root: &std::path::Path) -> rusqlite::Connection {
    let conn = store::open(root).expect("ingest");
    recalld::ingest_schema::ensure(&conn).expect("schema");
    conn
}

fn enrolled(f: &Fleet) -> Vec<(String, i64)> {
    f.meaning
        .prepare(
            "SELECT s.name, e.source_edit_id FROM speaker_embeddings e
             JOIN speakers s ON s.id = e.speaker_id ORDER BY e.id",
        )
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("rows")
        .collect::<Result<_, _>>()
        .expect("collect")
}

fn pass(f: &Fleet) -> recalld::enrol::Enrolled {
    write_pass(
        &f.meaning,
        &f.ingest,
        &crate::stamp("2026-09-10T12:10:00+00:00"),
        10,
    )
    .expect("pass")
}

const CLIP: &str = "usb-20260910T100000.flac";

#[test]
fn an_embedded_span_becomes_a_reference_voiceprint_under_its_naming() {
    let (f, clip) = fleet();
    let act = name(&f, &clip, 2.0, 6.0, "Alice", true);
    finished(
        &f.ingest,
        CLIP,
        &reply(&[Print {
            segment_id: act,
            vector: vec![0.5, -0.25],
        }]),
    );

    assert_eq!(pass(&f).prints, 1);
    assert_eq!(enrolled(&f), [("Alice".to_owned(), act)]);
    assert!(spans(&f).is_empty(), "printed, so it leaves the work-list");
}

#[test]
fn a_fragment_named_again_while_the_runner_worked_is_not_filed_under_the_old_name() {
    let (f, clip) = fleet();
    let act = name(&f, &clip, 2.0, 6.0, "Alice", true);
    name(&f, &clip, 2.0, 6.0, "Bob", true);
    finished(
        &f.ingest,
        CLIP,
        &reply(&[Print {
            segment_id: act,
            vector: vec![0.5],
        }]),
    );

    let done = pass(&f);

    assert_eq!((done.prints, done.stale), (0, 1));
    assert!(enrolled(&f).is_empty());
}

#[test]
fn an_empty_vector_is_refused_and_the_clip_is_ledgered_all_the_same() {
    // An empty vector would become somebody's best match on quiet audio; an
    // unledgered clip is decided again every pass.
    let (f, clip) = fleet();
    let act = name(&f, &clip, 2.0, 6.0, "Alice", true);
    finished(
        &f.ingest,
        CLIP,
        &reply(&[Print {
            segment_id: act,
            vector: vec![],
        }]),
    );

    assert_eq!(pass(&f).prints, 0);
    assert!(enrolled(&f).is_empty());
    assert_eq!(pass(&f).clips, 0, "ledgered: not looked at again");
}

#[test]
fn a_replayed_print_does_not_enrol_the_same_naming_twice() {
    let (f, clip) = fleet();
    let act = name(&f, &clip, 2.0, 6.0, "Alice", true);
    let twice = reply(&[
        Print {
            segment_id: act,
            vector: vec![0.5],
        },
        Print {
            segment_id: act,
            vector: vec![0.5],
        },
    ]);
    finished(&f.ingest, CLIP, &twice);

    assert_eq!(pass(&f).prints, 1);
    assert_eq!(enrolled(&f).len(), 1);
}

#[test]
fn a_leased_enrolment_job_carries_its_spans_and_other_kinds_carry_none() {
    // A job without spans would silently enrol nobody. Other kinds omit
    // `spans` on the wire.
    let (f, clip) = fleet();
    let act = name(&f, &clip, 2.0, 6.0, "Alice", true);

    let mut enrol_job = recalld::queue::Job {
        id: 1,
        kind: Kind::EnrollSpeaker,
        filename: CLIP.to_owned(),
        source: "usb".to_owned(),
        spans: Vec::new(),
        language: None,
    };
    recalld::enrol::attach_spans(f.root(), &mut enrol_job).expect("attach");
    assert_eq!(
        enrol_job.spans,
        vec![Span {
            segment_id: act,
            start_s: 2.0,
            end_s: 6.0
        }]
    );
    assert!(
        serde_json::to_string(&enrol_job)
            .expect("json")
            .contains("spans")
    );

    let mut diarize_job = recalld::queue::Job {
        kind: Kind::DiarizeSegment,
        spans: Vec::new(),
        ..enrol_job.clone()
    };
    recalld::enrol::attach_spans(f.root(), &mut diarize_job).expect("attach");
    assert!(diarize_job.spans.is_empty());
    assert!(
        !serde_json::to_string(&diarize_job)
            .expect("json")
            .contains("spans"),
        "an unrelated kind's lease body must not grow a field"
    );
}

/// A delivered blob with a capture time of its own.
fn delivered_at(root: &std::path::Path, filename: &str, start_utc: &str) {
    let conn = store::open(root).expect("ingest");
    store::insert(
        &conn,
        &store::Row {
            source: "usb".into(),
            filename: filename.into(),
            start_utc: start_utc.into(),
            bytes: 1,
            sha256: "x".into(),
            received_utc: "2026-09-10T10:01:00Z".into(),
            sent_utc: None,
        },
    )
    .expect("row");
}

#[test]
fn a_lease_of_another_kind_does_not_need_the_meaning_plane_at_all() {
    // A lease of another kind does not open `recall.sqlite`.
    let empty = tempfile::tempdir().expect("tempdir");
    let mut job = recalld::queue::Job {
        id: 1,
        kind: Kind::TranscribeSegment,
        filename: "usb-20260910T100000.wav".to_owned(),
        source: "usb".to_owned(),
        spans: Vec::new(),
        language: None,
    };
    recalld::enrol::attach_spans(empty.path(), &mut job).expect("no meaning plane, no problem");
}

#[test]
fn enrolment_outranks_capture_time_or_it_would_never_be_leased() {
    // Clips awaiting a print are usually old; by capture time they would wait
    // behind days of diarize jobs.
    let dir = tempfile::tempdir().expect("tempdir");
    let ingest = ingest_at(dir.path());
    // A newer clip with diarization to do, and an older one awaiting a print.
    delivered_at(
        dir.path(),
        "usb-20260912T100000.wav",
        "2026-09-12T10:00:00Z",
    );
    delivered_at(
        dir.path(),
        "usb-20260910T100000.wav",
        "2026-09-10T10:00:00Z",
    );
    for (kind, filename) in [
        (Kind::DiarizeSegment, "usb-20260912T100000.wav"),
        (Kind::EnrollSpeaker, "usb-20260910T100000.wav"),
    ] {
        ingest
            .execute(
                "INSERT INTO jobs (kind, filename, created_utc)
                 VALUES (?1, ?2, '2026-09-12T11:00:00Z')",
                rusqlite::params![kind, filename],
            )
            .expect("job");
    }

    let job = recalld::queue::lease(
        dir.path(),
        at("2026-09-12T12:00:00Z"),
        &[Kind::DiarizeSegment, Kind::EnrollSpeaker],
    )
    .expect("lease")
    .expect("a job");
    assert_eq!(
        job.kind,
        Kind::EnrollSpeaker,
        "the older clip's print comes first"
    );
}
