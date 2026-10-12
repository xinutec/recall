//! Whether the server answers at all. Every other server check skips on no
//! answer, and with capture paused nothing else goes red: recall was down for
//! 26 minutes with every run healthy. So when it last answered is kept on the
//! boot disk, and a long silence fails.

use crate::check::{Check, Verdict, check};
use chrono::{DateTime, Duration, Utc};
use std::path::{Path, PathBuf};

/// A restart takes about two minutes; a quarter of an hour is not a restart.
const FAIL_AFTER_MIN: i64 = 15;

const LABEL: &str = "the server answers";
const EXPECTED: &str = "answers; a miss is a warning, 15 min of them a failure";

/// Where the last answer is kept: the boot disk, not the archive volume the
/// parent never reads.
#[must_use]
pub fn state_file(home: &Path) -> PathBuf {
    home.join(".local/state/recall-doctor/fleet-reached")
}

/// Grade this run's answer, `Ok` or why not, and remember an answer.
#[must_use]
pub fn reach_check(state: &Path, now: DateTime<Utc>, answer: &Result<(), String>) -> Check {
    let (verdict, observed) = match answer {
        Ok(()) => {
            let kept = state
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(state, now.to_rfc3339()));
            match kept {
                Ok(()) => (Verdict::Pass, "answered".to_owned()),
                // The next miss could not be told from a long one.
                Err(err) => (
                    Verdict::Warn,
                    format!("answered, but {} would not keep it: {err}", state.display()),
                ),
            }
        }
        Err(why) => match last_answer(state) {
            Some(last) => {
                let minutes = (now - last).num_minutes();
                let verdict = if now - last > Duration::minutes(FAIL_AFTER_MIN) {
                    Verdict::Fail
                } else {
                    Verdict::Warn
                };
                (verdict, format!("no answer for {minutes} min: {why}"))
            }
            None => (
                Verdict::Warn,
                format!("no answer, and none on record: {why}"),
            ),
        },
    };
    check("record", LABEL, verdict, observed, EXPECTED).build()
}

fn last_answer(state: &Path) -> Option<DateTime<Utc>> {
    let raw = std::fs::read_to_string(state).ok()?;
    DateTime::parse_from_rfc3339(raw.trim())
        .ok()
        .map(|t| t.with_timezone(&Utc))
}
