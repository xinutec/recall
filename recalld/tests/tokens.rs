//! The fourth credential plane: a token opens exactly one source's write path.

use recalld::tokens::{Tokens, Verdict, same_token};

fn load(text: &str) -> Tokens {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("tokens");
    std::fs::write(&path, text).expect("write");
    Tokens::load(&path).expect("load")
}

#[test]
fn a_token_opens_its_own_source_only() {
    let tokens = load("usb secret-a\npixel5 secret-b\n");
    assert_eq!(tokens.check("usb", "secret-a"), Verdict::Allowed);
    assert_eq!(tokens.check("pixel5", "secret-a"), Verdict::WrongSource);
    assert_eq!(tokens.check("usb", "nonsense"), Verdict::UnknownToken);
}

#[test]
fn comments_blanks_and_rotation_pairs_parse() {
    // Two live tokens for one source is what a rotation window looks like.
    let tokens = load("# comment\n\nusb old-token\nusb new-token\n");
    assert_eq!(tokens.check("usb", "old-token"), Verdict::Allowed);
    assert_eq!(tokens.check("usb", "new-token"), Verdict::Allowed);
}

#[test]
fn a_malformed_line_fails_the_load() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("tokens");
    std::fs::write(&path, "just-one-word\n").expect("write");
    assert!(Tokens::load(&path).is_err());
}

#[test]
fn read_token_equality_is_exact() {
    assert!(same_token("abc", "abc"));
    assert!(!same_token("abc", "abd"));
    assert!(!same_token("abc", ""));
}
