//! The verdicts: the roll-up, recording, the transcription pulse, the live
//! tier, the archive's reachability and the agents.

use chrono::{DateTime, Utc};
use doctor::archive::{self, archive_check};
use doctor::capture::{
    Beat, Recorder, WindowAudio, agent_checks, capture_checks, live_check, live_lag_check,
    live_lag_slow, live_lag_window, live_quiet, silent_after, worker_check, worker_slow,
    worker_stopped,
};
use doctor::check::{Verdict, worst};
use doctor::source::SourceKind;
use std::path::Path;

fn at(minute: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_788_894_682 + minute * 60, 0).expect("a real instant")
}

fn mic(id: &str, kind: SourceKind, last_audio: Option<DateTime<Utc>>) -> Recorder {
    Recorder {
        source_id: id.to_owned(),
        kind,
        last_audio,
    }
}

fn find<'a>(checks: &'a [doctor::check::Check], label: &str) -> &'a doctor::check::Check {
    checks
        .iter()
        .find(|c| c.label == label)
        .unwrap_or_else(|| panic!("no check labelled {label} in {checks:#?}"))
}

#[test]
fn skip_ranks_with_pass_and_ties_keep_the_first() {
    assert_eq!(worst([Verdict::Pass, Verdict::Skip]), Verdict::Pass);
    assert_eq!(worst([Verdict::Skip, Verdict::Pass]), Verdict::Pass);
    assert_eq!(worst([Verdict::Skip, Verdict::Warn]), Verdict::Warn);
    assert_eq!(worst([Verdict::Fail, Verdict::Warn]), Verdict::Fail);
    assert_eq!(worst([]), Verdict::Pass);
}

#[test]
fn a_verdict_serialises_as_the_word_the_report_contract_expects() {
    assert_eq!(serde_json::to_string(&Verdict::Fail).unwrap(), "\"fail\"");
    assert_eq!(serde_json::to_string(&Verdict::Skip).unwrap(), "\"skip\"");
}

#[test]
fn the_wired_mic_fails_where_a_phone_only_warns() {
    let now = at(60);
    let recorders = [
        mic("usb", SourceKind::CoreAudio, Some(at(0))),
        mic("pixel9", SourceKind::TcpPcm, Some(at(0))),
    ];
    let checks = capture_checks(&recorders, now, None, silent_after());
    assert_eq!(find(&checks, "usb").verdict, Verdict::Fail);
    assert_eq!(find(&checks, "pixel9").verdict, Verdict::Warn);
}

#[test]
fn every_microphone_silent_at_once_is_the_capture_process_not_a_coincidence() {
    let now = at(60);
    let recorders = [
        mic("usb", SourceKind::CoreAudio, Some(at(0))),
        mic("pixel9", SourceKind::TcpPcm, Some(at(0))),
    ];
    let checks = capture_checks(&recorders, now, None, silent_after());
    let roll_up = find(&checks, "recording");
    assert_eq!(roll_up.verdict, Verdict::Fail);
    assert_eq!(
        roll_up.observed,
        "every microphone is silent — capture is not running"
    );
}

#[test]
fn one_live_microphone_keeps_the_roll_up_green() {
    let now = at(60);
    let recorders = [
        mic("usb", SourceKind::CoreAudio, Some(at(59))),
        mic("pixel9", SourceKind::TcpPcm, Some(at(0))),
    ];
    let checks = capture_checks(&recorders, now, None, silent_after());
    let roll_up = find(&checks, "recording");
    assert_eq!(roll_up.verdict, Verdict::Pass);
    assert_eq!(roll_up.observed, "1/2 microphones live");
}

#[test]
fn no_recorders_at_all_is_a_fault_not_an_empty_pass() {
    let checks = capture_checks(&[], at(60), None, silent_after());
    let roll_up = find(&checks, "recording");
    assert_eq!(roll_up.verdict, Verdict::Fail);
    assert_eq!(roll_up.observed, "no recorders found");
}

#[test]
fn a_pause_collapses_capture_to_one_skip_that_names_the_resume() {
    let now = at(0);
    let recorders = [mic("usb", SourceKind::CoreAudio, None)];
    let checks = capture_checks(&recorders, now, Some(at(120)), silent_after());
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].verdict, Verdict::Skip);
    assert!(checks[0].observed.starts_with("paused until 2026-09-"));
    assert!(
        !checks[0].observed.contains(":22"),
        "the resume is shown to the minute, not the second: {}",
        checks[0].observed
    );
}

#[test]
fn an_elapsed_pause_is_no_pause_at_all() {
    // The file outlives the pause; its instant decides.
    let now = at(60);
    let recorders = [mic("usb", SourceKind::CoreAudio, Some(at(59)))];
    let checks = capture_checks(&recorders, now, Some(at(30)), silent_after());
    assert!(
        checks.len() > 1,
        "a stale pause must not collapse the checks"
    );
    assert_eq!(find(&checks, "usb").verdict, Verdict::Pass);
}

#[test]
fn a_recorder_that_never_wrote_says_so_and_carries_no_number() {
    let recorders = [mic("usb", SourceKind::CoreAudio, None)];
    let checks = capture_checks(&recorders, at(60), None, silent_after());
    let usb = find(&checks, "usb");
    assert_eq!(usb.observed, "no audio ever recorded");
    assert!(usb.value.is_none(), "there is no duration to trend");
}

#[test]
fn live_is_graded_on_what_it_produced_not_on_being_up() {
    // A dead transcriber thread leaves the agent up.
    let now = at(60);
    assert_eq!(
        live_check(Some(at(59)), now, None, live_quiet(), TALKING).verdict,
        Verdict::Pass
    );
    assert_eq!(
        live_check(Some(at(0)), now, None, live_quiet(), TALKING).verdict,
        Verdict::Fail
    );
    assert_eq!(
        live_check(None, now, None, live_quiet(), TALKING).verdict,
        Verdict::Fail
    );
    assert_eq!(
        live_check(Some(at(0)), now, Some(at(120)), live_quiet(), TALKING).verdict,
        Verdict::Skip
    );
}

#[test]
fn a_quiet_house_is_not_a_live_fault_but_an_unscanned_one_is_not_quiet() {
    let now = at(60);
    let stale = Some(at(0));

    assert_eq!(
        live_check(stale, now, None, live_quiet(), SILENT).verdict,
        Verdict::Skip
    );
    assert_eq!(
        live_check(stale, now, None, live_quiet(), TALKING).verdict,
        Verdict::Fail
    );
    assert_eq!(
        live_check(stale, now, None, live_quiet(), UNSCANNED).verdict,
        Verdict::Fail
    );
    assert_eq!(
        live_check(stale, now, None, live_quiet(), HALF_SCANNED_SILENT).verdict,
        Verdict::Fail
    );
    // The capture checks grade this.
    assert_eq!(
        live_check(stale, now, None, live_quiet(), NO_AUDIO).verdict,
        Verdict::Skip
    );
    // Never produced a turn.
    assert_eq!(
        live_check(None, now, None, live_quiet(), TALKING).verdict,
        Verdict::Fail
    );
    assert_eq!(
        live_check(None, now, None, live_quiet(), SILENT).verdict,
        Verdict::Skip
    );
}

const TALKING: WindowAudio = WindowAudio {
    delivered_s: 1200.0,
    scanned_s: 1200.0,
    speech_s: 300.0,
};
const SILENT: WindowAudio = WindowAudio {
    delivered_s: 1200.0,
    scanned_s: 1200.0,
    speech_s: 0.0,
};
const UNSCANNED: WindowAudio = WindowAudio {
    delivered_s: 1200.0,
    scanned_s: 0.0,
    speech_s: 0.0,
};
const HALF_SCANNED_SILENT: WindowAudio = WindowAudio {
    delivered_s: 1200.0,
    scanned_s: 500.0,
    speech_s: 0.0,
};
const NO_AUDIO: WindowAudio = WindowAudio {
    delivered_s: 0.0,
    scanned_s: 0.0,
    speech_s: 0.0,
};

#[test]
fn a_worker_that_never_ran_is_a_fault() {
    assert_eq!(
        worker_check(None, at(0), worker_slow(), worker_stopped()).verdict,
        Verdict::Fail
    );
}

#[test]
fn a_completed_pass_is_graded_from_when_it_finished() {
    let beat = Beat {
        started: at(0),
        finished: Some(at(10)),
        seconds: Some(600.0),
        rows: 0,
    };
    assert_eq!(
        worker_check(Some(&beat), at(20), worker_slow(), worker_stopped()).verdict,
        Verdict::Pass
    );
    assert_eq!(
        worker_check(Some(&beat), at(50), worker_slow(), worker_stopped()).verdict,
        Verdict::Warn
    );
    assert_eq!(
        worker_check(Some(&beat), at(90), worker_slow(), worker_stopped()).verdict,
        Verdict::Fail
    );
    let observed = worker_check(Some(&beat), at(20), worker_slow(), worker_stopped()).observed;
    assert!(observed.contains("nothing to do"), "{observed}");
}

#[test]
fn a_running_pass_is_graded_from_when_it_started_and_named_differently() {
    // A pass that never returns points at the archive, not launchd.
    let beat = Beat {
        started: at(0),
        finished: None,
        seconds: None,
        rows: 0,
    };
    let check = worker_check(Some(&beat), at(90), worker_slow(), worker_stopped());
    assert_eq!(check.verdict, Verdict::Fail);
    assert!(check.observed.starts_with("a pass has been running"));
}

#[test]
fn the_worker_heartbeat_fixture_is_the_shape_the_runner_writes() {
    // `runner/tests/pulse.rs` checks the writer against the same fixture.
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tests/fixtures/worker-heartbeat.json"
    );
    let text = std::fs::read_to_string(fixture).expect("the fixture is committed");
    let beat: Beat = serde_json::from_str(&text).expect("the shape the runner writes");
    assert_eq!(beat.rows, 0);
    assert_eq!(beat.seconds, Some(17.567_348));
    let finished = beat.finished.expect("a completed pass");
    assert!(finished > beat.started);
    assert_eq!(
        worker_check(Some(&beat), finished, worker_slow(), worker_stopped()).verdict,
        Verdict::Pass
    );
}

#[test]
fn an_unanswered_archive_fails_rather_than_skips() {
    // A skip reads as not applicable.
    let unanswered = archive_check(None, "");
    assert_eq!(unanswered.verdict, Verdict::Fail);
    assert_eq!(unanswered.observed, "no answer in 60s");
    assert!(unanswered.value.is_none());
}

#[test]
fn a_slow_archive_warns_while_it_is_still_only_slow() {
    assert_eq!(archive_check(Some(1.5), "").verdict, Verdict::Pass);
    assert_eq!(archive_check(Some(10.0), "").verdict, Verdict::Warn);
    assert_eq!(archive_check(Some(59.0), "").verdict, Verdict::Warn);
    assert_eq!(archive_check(Some(1.5), "").value, Some(1.5));
}

#[test]
fn an_unreachable_volume_fails_rather_than_reading_as_a_fast_probe() {
    // An error must not read as a 0.00 s probe.
    let missing = archive::volume_check(Path::new("/nonexistent-volume-for-a-test"));
    assert_eq!(missing.verdict, Verdict::Fail);
    assert!(missing.value.is_none(), "an unread page has no latency");
    assert!(
        missing.observed.contains("cannot read the archive"),
        "{}",
        missing.observed
    );
}

#[test]
fn the_volume_probe_reads_a_fixed_page_however_big_the_archive_gets() {
    let dir = tempfile::tempdir().expect("a scratch dir");
    let db = dir.path().join(archive::PROBE_FILE);
    std::fs::write(&db, vec![0_u8; 1024 * 1024]).expect("write");
    let small = archive::volume_check(dir.path());
    std::fs::write(&db, vec![0_u8; 64 * 1024 * 1024]).expect("write");
    let large = archive::volume_check(dir.path());
    assert_eq!(small.verdict, Verdict::Pass);
    assert_eq!(large.verdict, Verdict::Pass);
    assert!(small.value.is_some() && large.value.is_some());
    assert!(
        large.value.expect("a reading") < volume_slow_seconds(),
        "a 64x bigger archive cost {:?}s to probe",
        large.value
    );
}

#[test]
fn an_archive_smaller_than_a_page_is_not_a_stalled_volume() {
    let dir = tempfile::tempdir().expect("a scratch dir");
    for bytes in [0_usize, 1, 100] {
        std::fs::write(dir.path().join(archive::PROBE_FILE), vec![0_u8; bytes]).expect("write");
        let check = archive::volume_check(dir.path());
        assert_eq!(
            check.verdict,
            Verdict::Pass,
            "{bytes} bytes read as a fault"
        );
    }
}

fn volume_slow_seconds() -> f64 {
    archive::volume_slow().num_seconds() as f64
}

#[test]
fn an_archive_that_answered_with_an_error_still_fails() {
    let failed = archive_check(Some(2.0), "database is locked");
    assert_eq!(failed.verdict, Verdict::Fail);
    assert!(failed.observed.contains("database is locked"));
}

#[test]
fn an_installed_but_unloaded_agent_is_always_a_fault() {
    // Agents stay loaded while paused.
    let checks = agent_checks(&[
        ("org.xinutec.recall-worker".to_owned(), true),
        ("org.xinutec.recall-capture".to_owned(), false),
    ]);
    assert_eq!(
        find(&checks, "org.xinutec.recall-worker").verdict,
        Verdict::Pass
    );
    let down = find(&checks, "org.xinutec.recall-capture");
    assert_eq!(down.verdict, Verdict::Fail);
    assert_eq!(down.observed, "NOT LOADED");
}

#[test]
fn no_agents_installed_at_all_is_reported_once() {
    let checks = agent_checks(&[]);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].verdict, Verdict::Fail);
}

#[test]
fn too_few_live_turns_skips_rather_than_passing() {
    let unmeasured = live_lag_check(None, live_lag_slow(), "3 live turn(s) in the window");
    assert_eq!(unmeasured.verdict, Verdict::Skip);
    assert!(unmeasured.value.is_none(), "an unmeasured lag has no trend");
    assert!(
        unmeasured.observed.contains("3 live turn(s)"),
        "a skip carries the caller's reason: {}",
        unmeasured.observed
    );
}

#[test]
fn a_feed_falling_behind_the_speaker_warns_while_it_is_still_only_slow() {
    // Only used without a median.
    const UNUSED: &str = "";
    let slow = live_lag_slow();
    let bound = slow.num_seconds() as f64;
    assert_eq!(
        live_lag_check(Some(3.0), slow, UNUSED).verdict,
        Verdict::Pass
    );
    assert_eq!(
        live_lag_check(Some(bound + 1.0), slow, UNUSED).verdict,
        Verdict::Warn
    );
    assert_eq!(live_lag_check(Some(3.0), slow, UNUSED).value, Some(3.0));
    assert!(
        live_lag_check(Some(bound + 1.0), slow, UNUSED)
            .observed
            .contains("falling further behind"),
        "a warn must say what is happening, not just that it is slow"
    );
}

#[test]
fn the_lag_window_is_shorter_than_a_day_so_it_cannot_blend_a_fault_with_its_fix() {
    assert!(live_lag_window() < chrono::Duration::days(1));
    // Long enough to clear the sample floor: half an hour of conversation
    // yields about 18 live turns.
    assert!(live_lag_window() > chrono::Duration::hours(1));
}
