//! The work plane's bytes, recorded from the two copies these types replaced:
//! each fixture must read and write back unchanged.

use audiocore::work::{Lease, LiveStored, LiveTurns, Prompt};
use serde::Serialize;
use serde::de::DeserializeOwned;

fn round_trips<T: Serialize + DeserializeOwned>(fixture: &str) -> T {
    let path = format!(
        "{}/../tests/fixtures/wire/{fixture}",
        env!("CARGO_MANIFEST_DIR")
    );
    let recorded = std::fs::read_to_string(&path).expect("fixture");
    let recorded = recorded.trim_end();
    let read: T = serde_json::from_str(recorded).expect("reads");
    assert_eq!(
        serde_json::to_string(&read).expect("writes"),
        recorded,
        "{fixture}"
    );
    read
}

#[test]
fn a_lease_writes_what_the_queue_sent() {
    for fixture in ["lease-enrol.json", "lease-pinned.json", "lease-plain.json"] {
        assert!(round_trips::<Lease>(fixture).job.is_some(), "{fixture}");
    }
    assert_eq!(round_trips::<Lease>("lease-empty.json").job, None);
}

#[test]
fn live_turns_and_their_answer_write_what_the_runner_and_recalld_sent() {
    let pushed = round_trips::<LiveTurns>("live-turns.json");
    assert_eq!(pushed.turns[1].language, None);
    assert_eq!(round_trips::<LiveStored>("live-stored.json").stored, 2);
}

#[test]
fn the_prompt_is_null_when_the_glossary_is_empty() {
    assert!(round_trips::<Prompt>("prompt.json").prompt.is_some());
    assert_eq!(round_trips::<Prompt>("prompt-empty.json").prompt, None);
}

#[test]
fn a_turn_without_a_language_reads_as_the_models_guess() {
    let turns: LiveTurns =
        serde_json::from_str(r#"{"turns":[{"start":"a","end":"b","text":"c","asr_model":"d"}]}"#)
            .expect("reads");
    assert_eq!(turns.turns[0].language, None);
}
