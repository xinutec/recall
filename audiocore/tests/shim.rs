//! The runner-shim contract, from the Rust side (#1830): the committed examples
//! in `tests/fixtures/shim/` are what `audiocore::shim` writes and reads.
//! `tests/test_shim_contract.py` holds the Python shims to the same files.

use audiocore::shim::{Stored, asr, voices};
use serde::Serialize;
use serde::de::DeserializeOwned;

fn example(name: &str) -> serde_json::Value {
    let path = format!(
        "{}/../tests/fixtures/shim/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).expect(&path);
    serde_json::from_str(&text).expect("json")
}

/// The type reads the example, and writing it back loses nothing: a field one
/// side sends and the other does not know fails here.
fn round_trips<T: DeserializeOwned + Serialize>(name: &str) -> T {
    let want = example(name);
    let typed: T = serde_json::from_value(want.clone()).expect(name);
    assert_eq!(serde_json::to_value(&typed).expect("write"), want, "{name}");
    typed
}

#[test]
fn the_requests_are_what_the_shims_read() {
    let transcribe = asr::Request {
        audio: "usb-20260906T090000.flac".to_owned(),
        words: true,
        model: None,
        language: None,
        initial_prompt: Some("Alex, Sam".to_owned()),
    };
    assert_eq!(
        serde_json::to_value(&transcribe).expect("write"),
        example("transcribe-request.json")
    );
    let diarize = voices::Diarize {
        audio: "usb-20260906T090000.flac".to_owned(),
        embed: true,
    };
    assert_eq!(
        serde_json::to_value(&diarize).expect("write"),
        example("diarize-request.json")
    );
    let embed = voices::Embed {
        audio: "usb-20260906T090000.flac".to_owned(),
        start: 1.5,
        end: 4.0,
    };
    assert_eq!(
        serde_json::to_value(&embed).expect("write"),
        example("embed-request.json")
    );
}

#[test]
fn the_replies_read_whole() {
    let heard: asr::Reply = round_trips("transcribe-reply.json");
    let words = heard.segments[0].words.as_ref().expect("words");
    assert_eq!(words.len(), 2);
    assert_eq!(words[1].text, " there");

    let spoke: voices::Diarization = round_trips("diarize-reply.json");
    assert_eq!(spoke.turns.len(), 2);
    assert_eq!(spoke.speakers[0].seconds, Some(2.5));

    let print: voices::Embedding = round_trips("embed-reply.json");
    assert_eq!(print.vector, Some(vec![0.125, 0.25]));
}

#[test]
fn a_stored_result_is_the_reply_or_the_refusal() {
    let reply = example("transcribe-reply.json");
    let stored = serde_json::json!({ "ok": true, "result": reply }).to_string();
    let heard = Stored::<asr::Reply>::parse(&stored)
        .expect("stored")
        .answer();
    assert_eq!(heard.expect("an answer").language.as_deref(), Some("en"));

    let refused = r#"{"ok": false, "error": "FileNotFoundError: x"}"#;
    let parsed = Stored::<asr::Reply>::parse(refused).expect("stored");
    assert_eq!(parsed.error.as_deref(), Some("FileNotFoundError: x"));
    assert!(parsed.answer().is_none());
}

#[test]
fn an_older_word_spelling_is_read_and_the_current_one_written() {
    // mlx-whisper's raw `word`, no probability: results stored that way remain.
    let old = r#"{"start": 1.5, "end": 1.9, "word": "een"}"#;
    let word: asr::Word = serde_json::from_str(old).expect("old spelling");
    assert_eq!(word.text, "een");
    assert_eq!(word.probability, None);
    assert_eq!(
        serde_json::to_string(&word).expect("write"),
        r#"{"start":1.5,"end":1.9,"text":"een"}"#
    );
}
