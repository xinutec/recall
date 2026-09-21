//! The live tier is graded here and measured on the fleet, so the interesting
//! cases are the ones where the fleet does not answer.

use chrono::{TimeZone, Utc};
use doctor::check::Verdict;
use doctor::live::{Fleet, LiveHealth, live_checks, unconfigured};

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 21, 12, 0, 0).unwrap()
}

/// A window the household clearly spoke in, so nothing skips for quiet.
fn talking(lag_median_s: Option<f64>, lag_samples: usize) -> LiveHealth {
    LiveHealth {
        lag_median_s,
        lag_samples,
        newest_turn_utc: Some("2026-09-21T11:59:00+00:00".to_owned()),
        delivered_s: 1200.0,
        scanned_s: 1200.0,
        speech_s: 300.0,
    }
}

#[test]
fn an_unreachable_fleet_skips_both_checks_and_fails_neither() {
    // ⚠⚠ The failure this whole module exists for. A live tier that cannot be
    // ASKED about is not a broken one, and a FAIL here would cry wolf every
    // time the VPN blinked — which teaches a person to stop reading the check
    // that is supposed to catch a dead tier.
    let checks = live_checks(
        &Err("cannot reach the fleet (timed out)".to_owned()),
        now(),
        None,
    );
    assert_eq!(
        checks.len(),
        2,
        "both, or a reader infers the silent one passed"
    );
    for check in &checks {
        assert_eq!(check.verdict, Verdict::Skip, "{}", check.label);
        assert!(
            check.observed.contains("reach the fleet"),
            "a skip must name the network, not read as a quiet house: {}",
            check.observed
        );
    }
}

#[test]
fn a_mac_with_no_fleet_says_so_rather_than_guessing_an_address() {
    let checks = unconfigured();
    assert_eq!(checks.len(), 2);
    for check in &checks {
        assert_eq!(check.verdict, Verdict::Skip);
        assert!(
            check.observed.contains("no fleet configured"),
            "{}",
            check.observed
        );
    }
}

#[test]
fn a_fleet_that_is_configured_needs_both_halves_of_the_credential() {
    // Either half missing is "not half of the Isis pair", never a request to
    // an address with no token on it.
    assert!(Fleet::new(Some("http://fleet:8000"), Some("t")).is_some());
    assert!(Fleet::new(None, Some("t")).is_none());
    assert!(Fleet::new(Some("http://fleet:8000"), None).is_none());
    assert!(Fleet::new(Some("http://fleet:8000"), Some("")).is_none());
}

#[test]
fn a_handful_of_turns_skips_with_the_count_rather_than_grading_noise() {
    // ⚠ The sample floor is the GRADER's rule, which is why the fleet sends the
    // count and not a pre-filtered median: a measurement that hid its own
    // sample size could not be graded by any other one.
    let checks = live_checks(&Ok(talking(Some(4.0), 3)), now(), None);
    let lag = &checks[0];
    assert_eq!(lag.verdict, Verdict::Skip);
    assert!(lag.observed.contains('3'), "say how few: {}", lag.observed);
    assert!(lag.value.is_none(), "an unmeasured lag has no trend");
}

#[test]
fn enough_turns_are_actually_graded() {
    let healthy = live_checks(&Ok(talking(Some(4.0), 40)), now(), None);
    assert_eq!(healthy[0].verdict, Verdict::Pass);
    assert_eq!(healthy[0].value, Some(4.0));
    assert_eq!(healthy[1].verdict, Verdict::Pass, "a turn a minute ago");

    // The failure #1383 fixed: turns keep arriving, each later than the last.
    let behind = live_checks(&Ok(talking(Some(120.0), 40)), now(), None);
    assert_eq!(behind[0].verdict, Verdict::Warn);
}

#[test]
fn the_pause_still_skips_the_liveness_check() {
    // Pause is the Mac's own state and stays the Mac's to read: nothing is
    // being recorded, so nothing should be transcribed.
    let until = now() + chrono::Duration::hours(1);
    let checks = live_checks(&Ok(talking(Some(4.0), 40)), now(), Some(until));
    assert_eq!(checks[1].verdict, Verdict::Skip);
    assert!(
        checks[1].observed.contains("paused"),
        "{}",
        checks[1].observed
    );
}

#[test]
fn a_window_nobody_spoke_in_skips_instead_of_blaming_the_tier() {
    // Measured 2026-09-09: the check was red for 36% of 55.8 active hours, and
    // a quarter of that was simply a quiet house.
    let quiet = LiveHealth {
        speech_s: 0.0,
        ..talking(Some(4.0), 40)
    };
    assert_eq!(
        live_checks(&Ok(quiet), now(), None)[1].verdict,
        Verdict::Skip
    );
}

#[test]
fn an_unscanned_window_is_not_a_quiet_one() {
    // ⚠ `scanned_s` is separate from `delivered_s` on purpose: a window reads
    // "no speech" both when the house was silent and when nothing in it has
    // been measured yet. Collapsing those would silence the check exactly when
    // the archive fell behind — the condition most likely to accompany a stall.
    let unscanned = LiveHealth {
        scanned_s: 0.0,
        speech_s: 0.0,
        ..talking(Some(4.0), 40)
    };
    assert_ne!(
        live_checks(&Ok(unscanned), now(), None)[1].verdict,
        Verdict::Skip,
        "an unmeasured window must not certify quiet"
    );
}
