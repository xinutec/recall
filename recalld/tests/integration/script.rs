//! Which script a turn is written in — the one safe signal #1410 found.

use recalld::quality::{foreign_script_ratio, is_foreign_script};

/// The household's own languages read as fully Latin, diacritics and all.
#[test]
fn dutch_and_english_are_latin() {
    for text in [
        "En dat ga ik thuis ook doen.",
        "We should leave at eight.",
        "coördinatie, ijsje, café, Ångström",
    ] {
        assert!(
            foreign_script_ratio(text) < f64::EPSILON,
            "{text:?} scored {}",
            foreign_script_ratio(text)
        );
        assert!(!is_foreign_script(text));
    }
}

/// ⚠ The hallucinations this exists to catch: 931 visible turns are under half
/// Latin, and 273 of them are LABELLED nl or en while emitting Cyrillic or
/// Japanese — the model contradicting itself (#1410).
#[test]
fn cyrillic_and_japanese_are_foreign() {
    for text in [
        "И ти не правиш нищо",
        "これはテストです",
        "이것은 시험입니다",
    ] {
        assert!(
            foreign_script_ratio(text) > 0.99,
            "{text:?} scored {}",
            foreign_script_ratio(text)
        );
        assert!(is_foreign_script(text));
    }
}

/// ⚠ **Punctuation, digits and spaces are NOT letters and must not vote.** A
/// turn of "..." would otherwise read as 100% foreign, and `is_wordless`
/// already owns that case.
#[test]
fn only_letters_vote() {
    for text in ["...", "!!! ???", "12:45", "   "] {
        assert!(
            foreign_script_ratio(text) < f64::EPSILON,
            "{text:?} must not count as foreign"
        );
    }
}

/// A mixed turn is judged by its majority, so one borrowed word does not
/// condemn a Dutch sentence and one Dutch word does not rescue a Russian one.
#[test]
fn a_mixed_turn_is_judged_by_its_majority() {
    assert!(!is_foreign_script("We call it совет sometimes"));
    assert!(is_foreign_script("Мы называем это soviet иногда всегда"));
}

/// ⚠ The edge the range table exists for: Latin Extended, where a guess at
/// block boundaries would disagree with the Python this replaces.
#[test]
fn latin_extended_letters_are_latin() {
    for text in ["ŉ ǆ ȿ ɏ", "ᴀ ᴠ", "ﬀ ﬁ", "Ａ Ｚ"] {
        assert!(
            foreign_script_ratio(text) < f64::EPSILON,
            "{text:?} scored {}",
            foreign_script_ratio(text)
        );
    }
}
