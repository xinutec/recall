//! Text rules recalld applies outside render: the live feed's refusals. The
//! rest are `transcript::quality` and `transcript::text`.

use audiocore::text::trim_wordless;
pub use audiocore::text::{is_repetition_loop, is_wordless};

/// True if `text` is nothing but one of `names`: "Anna.", " anna ", "Anna!".
///
/// The vocabulary prompt's cost: on audio it cannot place, Whisper reaches for
/// a listed name. Refused, not kept at zero confidence, since the line asserts
/// a person spoke. A real vocative is lost too, which only the live tier can
/// afford: the archive pass supersedes it with context.
#[must_use]
pub fn is_bare_name(text: &str, names: &[String]) -> bool {
    let bare = trim_wordless(text);
    !bare.is_empty()
        && names
            .iter()
            .any(|name| name.trim().eq_ignore_ascii_case(bare))
}
