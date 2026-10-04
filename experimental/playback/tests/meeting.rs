//! A meeting scored against its word reference, with dropped words placed.

use playback::meeting::{Fate, Segment, align, ami_words, score};
use std::fmt::Write;

const XML: &str = r#"<nite:root>
   <w nite:id="a0" starttime="0.37" endtime="0.95">Hmm</w>
   <w nite:id="a1" starttime="1.0" endtime="1.4">Good</w>
   <w nite:id="a2" starttime="1.4" endtime="1.4" punc="true">.</w>
   <w nite:id="a3" starttime="1.5" endtime="1.9">morning</w>
   <w nite:id="a6" starttime="2.4" endtime="2.6">it&#39;s</w>
   <w nite:id="a4" starttime="2.0" endtime="2.1" trunc="true">Tu</w>
   <vocalsound nite:id="a5" starttime="2.2" endtime="2.3" type="cough"/>
</nite:root>"#;

#[test]
fn the_reference_keeps_spoken_words_only() {
    let words = ami_words("A", XML);
    let texts: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
    assert_eq!(texts, ["good", "morning", "it", "s"]);
    assert!((words[1].start - 1.5).abs() < 1e-9);
}

#[test]
fn the_alignment_names_each_reference_words_fate() {
    let r: Vec<String> = ["a", "b", "c", "d"].map(String::from).to_vec();
    let h: Vec<String> = ["a", "x", "d"].map(String::from).to_vec();
    let (errors, fates) = align(&r, &h);
    assert_eq!(
        (errors.substitutions, errors.deletions, errors.insertions),
        (1, 1, 0)
    );
    assert_eq!(fates.iter().filter(|f| **f == Fate::Right).count(), 2);
    // "x" stands in for b or c; either alignment costs the same.
    assert_eq!(
        fates.iter().filter(|f| **f == Fate::Substituted(1)).count(),
        1
    );
}

#[test]
fn a_dropped_stretch_is_told_apart_from_a_word_lost_inside_a_line() {
    let mut xml = String::from("<r>");
    // Ten words at 0-10 s, a line covers 0-4 s and drops one of them; 5-10 s
    // has no line at all.
    for i in 0..10 {
        write!(xml, r#"<w starttime="{i}.1" endtime="{i}.8">w{i}</w>"#).unwrap();
    }
    let reference = ami_words("A", &(xml + "</r>"));
    let line = Segment {
        start: 0.0,
        end: 4.0,
        text: "w0 w1 w3 w4".into(),
    };
    let report = score(&reference, &[line]);
    assert_eq!(report.errors.deletions, 6);
    assert_eq!(report.deleted_covered, 1);
    assert_eq!(report.deleted_uncovered, 5);
    assert_eq!(report.holes.len(), 1);
    assert_eq!(report.holes[0].words, 5);
}
