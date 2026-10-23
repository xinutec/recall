//! The vocabulary is read for each transcription, so a term added while the
//! runner runs reaches the next clip without a restart. Drives the shipped
//! binary.

mod common;

use common::{job, queued_clip, serve};
use std::time::{Duration, Instant};

const TOKEN: &str = "recall-token";

/// An `asr` shim that echoes the prompt it was given, and adds a term to the
/// vocabulary after its first transcription: a person adding one mid-run.
fn shim_adding_a_term(meaning: &std::path::Path) -> Vec<String> {
    let script = format!(
        "import json, sqlite3, sys\n\
         added = False\n\
         for line in sys.stdin:\n\
         \x20   msg = json.loads(line)\n\
         \x20   if msg['op'] == 'hello':\n\
         \x20       result = {{'shim': 'asr'}}\n\
         \x20   else:\n\
         \x20       result = {{'language': 'en', 'segments': [{{'text': 'prompt=' + str(msg.get('initial_prompt'))}}]}}\n\
         \x20       if not added:\n\
         \x20           db = sqlite3.connect({path:?})\n\
         \x20           db.execute(\"INSERT INTO vocabulary (owner, term, created_utc) VALUES ('p', 'Zerobase', '2026-10-10T00:00:01+00:00')\")\n\
         \x20           db.commit()\n\
         \x20           added = True\n\
         \x20   print(json.dumps({{'id': msg['id'], 'ok': True, 'result': result}}))\n\
         \x20   sys.stdout.flush()\n",
        path = meaning.display().to_string()
    );
    vec!["python3".to_owned(), "-c".to_owned(), script]
}

#[test]
fn a_term_added_while_running_reaches_the_next_clip() {
    let root = tempfile::tempdir().expect("tempdir");
    let url = serve(root.path(), TOKEN);
    let meaning = root.path().join("recall.sqlite");
    rusqlite::Connection::open(&meaning)
        .expect("meaning")
        .execute_batch(
            "INSERT INTO sources (id, name, kind, owner) VALUES ('usb', 'USB', 'coreaudio', 'p');
             INSERT INTO vocabulary (owner, term, created_utc) VALUES ('p', 'Hexwick', '2026-10-10T00:00:00+00:00');",
        )
        .expect("term");
    let (first, second) = ("usb-20261010T100000.flac", "usb-20261010T100100.flac");
    queued_clip(root.path(), "usb", first);
    queued_clip(root.path(), "usb", second);

    let mut runner = std::process::Command::new(env!("CARGO_BIN_EXE_runner"))
        .args(["--url", &url, "--shim"])
        .args(shim_adding_a_term(&meaning))
        .env("RECALL_SYNC_TOKEN", TOKEN)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("the runner starts");
    let done = |name| job(root.path(), name).0 == "done";
    let deadline = Instant::now() + Duration::from_secs(15);
    while !(done(first) && done(second)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = runner.kill();
    let _ = runner.wait();

    // The queue picks the order; whichever went first, the other saw the term.
    let mut prompts: Vec<String> = [first, second]
        .map(|name| job(root.path(), name).1.unwrap_or_default())
        .to_vec();
    prompts.sort();
    assert!(
        prompts[0].contains("prompt=Hexwick\"") && prompts[1].contains("prompt=Hexwick, Zerobase"),
        "the term added after one clip reaches the next: {prompts:?}"
    );
}
