//! Known speech, played into the house through its loudspeakers, scored per
//! microphone against the text that was played (#1388).
//!
//! The household's own transcripts have no reference: a word error rate needs
//! someone to have checked every word. Played speech from a public test set
//! carries its reference with it, so every microphone, and any way of
//! combining them, can be scored with nobody at home.

pub mod corpus;
pub mod plan;
pub mod score;
pub mod wer;
