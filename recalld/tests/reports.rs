//! The client-report surface (stage F1) — and mostly, its one security boundary.
//!
//! `/api/log` and `/api/telemetry` take verbatim UI text from a browser and write
//! it into a log line. Everything here is about the case where that text is
//! hostile, because the failure is silent: the request succeeds, the log looks
//! normal, and it is no longer evidence of what happened.

use recalld::reports::{ClientLog, log_line, one_line};

const MAX: usize = 160;

#[test]
fn a_newline_in_a_label_cannot_forge_a_log_line() {
    // ⚠ THE attack this function exists for. `label=` is written verbatim into a
    // line, so a newline lets a client append lines of its own — including
    // further `client-event` lines attributed to somebody else. One forged line
    // and the log stops being evidence.
    let hostile = "ok\nclient-event kind=deleted path=/ label=everything";

    let safe = one_line(hostile, MAX);

    assert!(!safe.contains('\n'));
    assert_eq!(safe, "ok client-event kind=deleted path=/ label=everything");
}

#[test]
fn carriage_returns_and_tabs_are_flattened_too() {
    // \r alone rewrites a line on a terminal; a tab misaligns a field-separated
    // log. Both are control characters and both go.
    assert_eq!(one_line("a\r\nb\tc", MAX), "a b c");
}

#[test]
fn unicode_line_and_paragraph_separators_do_not_survive() {
    // ⚠ U+2028 and U+2029 are NOT control characters, so a guard written against
    // `is_control` alone lets them through — and plenty of log viewers and
    // JavaScript treat them as line breaks. They are whitespace, which is what
    // catches them here.
    let hostile = "before\u{2028}after\u{2029}more";

    assert_eq!(one_line(hostile, MAX), "before after more");
}

#[test]
fn bidi_overrides_that_reorder_a_rendered_line_are_stripped() {
    // These cannot forge a newline, so they are not line injection — but they can
    // make a line RENDER as something other than what it says, which attacks the
    // same property by a different route.
    let hostile = "safe\u{202E}desrever\u{202C} tail";

    let safe = one_line(hostile, MAX);

    assert!(!safe.contains('\u{202E}'));
    assert!(!safe.contains('\u{202C}'));
    assert_eq!(safe, "safe desrever tail");
}

#[test]
fn zero_width_characters_do_not_survive_either() {
    let safe = one_line("re\u{200B}call\u{FEFF}", MAX);
    assert_eq!(safe, "re call");
}

#[test]
fn runs_of_whitespace_collapse_to_one_space() {
    assert_eq!(one_line("  a     b  ", MAX), "a b");
    assert_eq!(one_line("   ", MAX), "");
}

#[test]
fn truncation_counts_characters_not_bytes() {
    // ⚠ Byte truncation would cut a multi-byte glyph in half and write invalid
    // UTF-8 into the log. Each of these is 3 bytes and one character.
    let long = "\u{3042}".repeat(200); // HIRAGANA A

    let safe = one_line(&long, 10);

    assert_eq!(safe.chars().count(), 10);
    assert_eq!(safe, "\u{3042}".repeat(10));
}

#[test]
fn a_log_line_keeps_only_the_first_line_of_a_stack() {
    // A browser stack is dozens of frames. Writing all of them lets one client
    // error become a hundred log lines, which is the flood the cap exists for.
    let entry = ClientLog {
        level: "error".into(),
        url: Some("/sessions/meeting-x".into()),
        message: "TypeError: undefined is not a function".into(),
        stack: Some("at foo (main.js:1)\nat bar (main.js:2)\nat baz (main.js:3)".into()),
    };

    let line = log_line("2026-09-07T09:00:00+00:00", &entry);

    assert_eq!(line.lines().count(), 2);
    assert!(line.contains("at foo (main.js:1)"));
    assert!(!line.contains("at bar"));
}

#[test]
fn a_hostile_message_cannot_break_out_of_its_log_line() {
    // The message field gets the same treatment as the label — it is equally
    // client-supplied, and a guard applied to only one field is not a guard.
    let entry = ClientLog {
        level: "error\nforged".into(),
        url: Some("/x\nforged".into()),
        message: "boom\nclient-event kind=forged".into(),
        stack: None,
    };

    let line = log_line("2026-09-07T09:00:00+00:00", &entry);

    assert_eq!(line.lines().count(), 1, "got: {line}");
}

#[test]
fn a_missing_url_reads_as_absent_rather_than_empty() {
    let entry = ClientLog {
        level: "warn".into(),
        url: None,
        message: "something".into(),
        stack: None,
    };

    assert!(log_line("2026-09-07T09:00:00+00:00", &entry).contains(" - "));
}

#[test]
fn a_real_telemetry_batch_from_the_app_deserialises() {
    // ⚠ The test the first draft of this port needed and did not have. The app
    // sends `{ kind, path, label, at: Date.now() }` — `at` is a NUMBER. Typing it
    // as a string made every batch fail to deserialise, which would have been a
    // 422 in production and silent loss of the activity trace. dev-lint's mirror
    // check caught it against the generated models.ts; this pins it here too, so
    // a future edit fails at the unit level rather than at the contract gate.
    let body = r#"[
        {"kind":"tap","path":"/sessions","label":"Upload","at":1788000000000},
        {"kind":"nav","path":"/","label":null,"at":1788000000001}
    ]"#;

    let events: Vec<recalld::reports::TelemetryEvent> =
        serde_json::from_str(body).expect("a real client batch must parse");

    assert_eq!(events.len(), 2);
    assert_eq!(events[0].at, 1_788_000_000_000);
    assert_eq!(events[0].path, "/sessions");
    assert_eq!(events[1].label, None);
}
