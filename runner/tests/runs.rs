//! A minute's runs merged into the one result the fleet stores.

use audiocore::language_runs::Run;
use runner::runs::merge;
use serde_json::json;

fn run(start: f64, end: f64, language: &str) -> Run {
    Run {
        start,
        end,
        language: Some(language.to_owned()),
    }
}

#[test]
fn runs_merge_into_minute_time_each_segment_keeping_its_language() {
    let decoded = [
        (
            run(0.0, 20.0, "en"),
            json!({"language": "en", "segments": [{"start": 1.0, "end": 3.0, "text": " hello",
                    "words": [{"start": 1.0, "end": 3.0, "text": " hello"}]}]}),
        ),
        (
            run(20.0, 60.0, "nl"),
            json!({"language": "nl", "segments": [{"start": 0.5, "end": 2.0, "text": " hallo"}]}),
        ),
    ];
    let merged = merge(&decoded);
    let segments = merged["segments"].as_array().unwrap();
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[0]["language"], "en");
    assert_eq!(segments[1]["start"], 20.5);
    assert_eq!(segments[1]["language"], "nl");
    assert_eq!(segments[0]["words"][0]["end"], 3.0);
    // The minute's language is its longest run's: 40 s of Dutch over 20 of English.
    assert_eq!(merged["language"], "nl");
}

#[test]
fn the_merged_result_reads_as_the_reply_the_fleet_parses() {
    let decoded = [(
        run(0.0, 60.0, "nl"),
        json!({"language": "nl", "segments": [{"start": 0.5, "end": 2.0, "text": " hallo"}]}),
    )];
    let reply: audiocore::shim::asr::Reply = serde_json::from_value(merge(&decoded)).unwrap();
    assert_eq!(reply.language.as_deref(), Some("nl"));
    assert_eq!(reply.segments[0].language.as_deref(), Some("nl"));
}
