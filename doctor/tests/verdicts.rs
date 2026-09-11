//! The rules that decide what fleetwatch is told: the roll-up, the recording
//! checks, the worker pulse, the live tier, and the archive's own reachability.

use chrono::{DateTime, Duration, Utc};
use doctor::archive::{self, archive_check, blanked_check, mirror_check};
use doctor::capture::{
    Beat, Recorder, WindowAudio, agent_checks, capture_checks, live_check, live_quiet,
    silent_after, worker_check, worker_slow, worker_stopped,
};
use doctor::check::{Verdict, worst};
use doctor::source::SourceKind;

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
    // A deliberate pause is not a fault and must never drag a summary upward.
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
    // The always-on mic is wired to the machine doing the recording and has no
    // excuse. A phone is carried out of the house, runs flat, gets closed.
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
    // The June shape: fourteen crash-loop restarts, ninety minutes recorded
    // nowhere, and from outside it looked exactly like a quiet house.
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
    // Deliberate, so it must never page anyone — but it IS shown, because a
    // pause nobody remembers is how a week of memory goes missing.
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
    // The pause file outlives the pause; only the bound decides.
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
    // On 2026-09-03 live's consumer thread died while its reader carried on:
    // the agent was up, KeepAlive satisfied, every other check green, and the
    // tier that answers "what did they just say" wrote nothing for 40 minutes.
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
    // A pause skips: nothing is recorded, so nothing should be transcribed.
    assert_eq!(
        live_check(Some(at(0)), now, Some(at(120)), live_quiet(), TALKING).verdict,
        Verdict::Skip
    );
}

/// Measured 2026-09-09 over 55.8 active hours: `live_check` was red for 36% of
/// them, and 4.7 of the 20.2 red hours were a quiet house rather than a fault.
/// Blaming live for the household's silence is what teaches a person to ignore
/// it, and #1383's real stalls are the 15.5 h that remain.
#[test]
fn a_quiet_house_is_not_a_live_fault_but_an_unscanned_one_is_not_quiet() {
    let now = at(60);
    let stale = Some(at(0));

    // Nobody spoke: there was nothing for live to write.
    assert_eq!(
        live_check(stale, now, None, live_quiet(), SILENT).verdict,
        Verdict::Skip
    );
    // People were audibly talking and live wrote nothing. This is the alarm the
    // check exists for.
    assert_eq!(
        live_check(stale, now, None, live_quiet(), TALKING).verdict,
        Verdict::Fail
    );

    // ⚠ The trap: `speech_s` is filled by a scanner on its own cadence, so a
    // window can read "no speech" simply because nothing in it has been
    // measured yet. Absence of measurement is NOT absence of speech, and
    // skipping on it would silence the check exactly when the archive fell
    // behind — the failure most likely to accompany a live stall.
    assert_eq!(
        live_check(stale, now, None, live_quiet(), UNSCANNED).verdict,
        Verdict::Fail
    );
    // Half-scanned is not enough to certify quiet either.
    assert_eq!(
        live_check(stale, now, None, live_quiet(), HALF_SCANNED_SILENT).verdict,
        Verdict::Fail
    );

    // Nothing was delivered at all: capture's own checks grade that, and live
    // failing too would be the same outage counted twice.
    assert_eq!(
        live_check(stale, now, None, live_quiet(), NO_AUDIO).verdict,
        Verdict::Skip
    );
    // A tier that has never produced a turn is still a fault while people talk,
    // and still not one in a silent house.
    assert_eq!(
        live_check(None, now, None, live_quiet(), TALKING).verdict,
        Verdict::Fail
    );
    assert_eq!(
        live_check(None, now, None, live_quiet(), SILENT).verdict,
        Verdict::Skip
    );
}

/// 20 minutes of delivered audio, all scanned, full of speech.
const TALKING: WindowAudio = WindowAudio {
    delivered_s: 1200.0,
    scanned_s: 1200.0,
    speech_s: 300.0,
};
/// The same window, scanned, with nothing said in it.
const SILENT: WindowAudio = WindowAudio {
    delivered_s: 1200.0,
    scanned_s: 1200.0,
    speech_s: 0.0,
};
/// Audio arrived and none of it has been measured yet.
const UNSCANNED: WindowAudio = WindowAudio {
    delivered_s: 1200.0,
    scanned_s: 0.0,
    speech_s: 0.0,
};
/// Half measured and quiet so far — not enough to call the window quiet.
const HALF_SCANNED_SILENT: WindowAudio = WindowAudio {
    delivered_s: 1200.0,
    scanned_s: 500.0,
    speech_s: 0.0,
};
/// No recorder delivered anything.
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
    // 30 min is the warn line, an hour the fail line — the 2026-08-10 shape.
    assert_eq!(
        worker_check(Some(&beat), at(50), worker_slow(), worker_stopped()).verdict,
        Verdict::Warn
    );
    assert_eq!(
        worker_check(Some(&beat), at(90), worker_slow(), worker_stopped()).verdict,
        Verdict::Fail
    );
    // An empty pass says so rather than reporting zero rows as a number.
    let observed = worker_check(Some(&beat), at(20), worker_slow(), worker_stopped()).observed;
    assert!(observed.contains("nothing to do"), "{observed}");
}

#[test]
fn a_running_pass_is_graded_from_when_it_started_and_named_differently() {
    // Both are the same clock; they point at different things. A pass that will
    // not return is the archive, a loop that will not start one is launchd.
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
fn the_worker_heartbeat_fixture_is_the_shape_the_python_worker_writes() {
    // The other half of this contract is tests/test_cli_worker.py, which asserts
    // the real worker writes these exact keys. The fixture is the one copy — a
    // field renamed on either side fails on both, which is the only way a
    // cross-language seam gets checked at all.
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tests/fixtures/worker-heartbeat.json"
    );
    let text = std::fs::read_to_string(fixture).expect("the fixture is committed");
    let beat: Beat = serde_json::from_str(&text).expect("the shape the worker writes");
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
    // A skip reads as "not applicable", and nothing is more applicable than the
    // archive being unreachable. The June lesson was that a silence which looks
    // deliberate is how a fault survives for weeks.
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
    // Trended either way: the only warning anyone gets before it wedges.
    assert_eq!(archive_check(Some(1.5), "").value, Some(1.5));
}

#[test]
fn an_archive_that_answered_with_an_error_still_fails() {
    let failed = archive_check(Some(2.0), "database is locked");
    assert_eq!(failed.verdict, Verdict::Fail);
    assert!(failed.observed.contains("database is locked"));
}

#[test]
fn an_incomplete_fleet_mirror_fails_because_the_backup_claim_depends_on_it() {
    // "If the Mac dies the archive lives on Isis" is only true while this is 0.
    assert_eq!(mirror_check(0, Duration::hours(1)).verdict, Verdict::Pass);
    assert_eq!(mirror_check(1, Duration::hours(1)).verdict, Verdict::Fail);
}

#[test]
fn a_blanked_segment_names_the_command_that_fixes_it() {
    assert_eq!(blanked_check(0).verdict, Verdict::Pass);
    let bad = blanked_check(3);
    assert_eq!(bad.verdict, Verdict::Fail);
    assert!(bad.observed.contains("recall repair"));
}

#[test]
fn an_installed_but_unloaded_agent_is_always_a_fault() {
    // The agents self-gate — they park while capture is paused, they do not
    // unload — so "not loaded" never means "deliberately off".
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
fn a_python_instant_drops_a_zero_fraction_and_keeps_a_real_one() {
    // These strings are COMPARED as text by every archive query, so the
    // spelling has to be the one Python wrote.
    let whole = DateTime::from_timestamp(1_788_894_682, 0).unwrap();
    assert_eq!(archive::python_iso(whole), "2026-09-08T19:11:22+00:00");
    let fractional = DateTime::from_timestamp(1_788_894_682, 164_504_000).unwrap();
    assert_eq!(
        archive::python_iso(fractional),
        "2026-09-08T19:11:22.164504+00:00"
    );
}
