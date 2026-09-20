//! The doctor's loop rule must agree with the one that WROTE the turn.
//!
//! ⚠ `doctor::blanked` copies `recalld::quality` because the doctor does not
//! depend on recalld. The copies drifted once: the doctor searched on after a
//! unit with enough repeats but too short a span, where the original returns
//! there. They then disagreed about real text — and they judge the same text
//! for opposite purposes, so a turn recalld wrote that the doctor calls junk is
//! household memory reported as unrecoverable.

/// The exact string the drift was found on. Enough repeats of a SHORT unit to
/// qualify, spanning under the minimum, followed by a genuine long loop.
const DIVERGED_ON: &str = "ababababxyzxyzxyzxyzxyz";

#[test]
fn the_case_the_two_copies_once_disagreed_about() {
    assert!(
        !doctor::blanked::is_repetition_loop(DIVERGED_ON),
        "the doctor searched on where the original returns — it now calls a \
         turn junk that recalld wrote"
    );
}

#[test]
fn a_real_char_loop_is_still_caught() {
    // Guarding the above must not have blinded it to what it is for.
    assert!(doctor::blanked::is_repetition_loop("ASTASTASTASTASTAST"));
    assert!(doctor::blanked::is_repetition_loop("obaobaobaobaoba"));
    assert!(!doctor::blanked::is_repetition_loop(
        "the kettle has boiled"
    ));
}
