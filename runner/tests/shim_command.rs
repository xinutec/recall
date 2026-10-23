//! `--shim`'s words, as both binaries read them.

use runner::shim::command;

#[test]
fn no_words_run_the_transcribing_shim() {
    assert_eq!(
        command(&[]),
        (
            "python".to_owned(),
            vec!["-m".to_owned(), "recall.shim_asr".to_owned()]
        )
    );
}

#[test]
fn the_first_word_is_the_program_and_the_rest_its_arguments() {
    let words = ["uv", "run", "-m", "recall.shim_voices"].map(str::to_owned);
    assert_eq!(
        command(&words),
        (
            "uv".to_owned(),
            ["run", "-m", "recall.shim_voices"]
                .map(str::to_owned)
                .to_vec()
        )
    );
}
