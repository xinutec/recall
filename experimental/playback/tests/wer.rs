use playback::wer::{Errors, align, words};

fn w(s: &str) -> Vec<String> {
    words(s)
}

#[test]
fn normalises_as_the_python_does() {
    assert_eq!(w("Hello, World! It's"), ["hello", "world", "it", "s"]);
}

#[test]
fn counts_each_kind() {
    let e = align(&w("the cat sat on the mat"), &w("the cat sat on mat today"));
    assert_eq!(
        e,
        Errors {
            reference: 6,
            substitutions: 0,
            deletions: 1,
            insertions: 1
        }
    );
    assert_eq!(align(&w("a b c"), &w("a x c")).substitutions, 1);
}

#[test]
fn empty_sides() {
    assert_eq!(align(&w("a b"), &[]).deletions, 2);
    let e = align(&[], &w("x y"));
    assert_eq!(e.insertions, 2);
    assert_eq!(e.rate(), None);
}
