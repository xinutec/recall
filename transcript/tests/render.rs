//! render: every rule in its module header, and that no model word vanishes
//! without a stated reason.

use transcript::render::{
    Author, Heard, HeardSegment, HeardWord, Input, Speaker, Speech, VoiceTurn, Voices, Why, render,
};
use transcript::voice::Voiceprint;
use transcript::{Act, Clip, ClipId, Edit, EditId, Instant, Name, SourceId, Span, Text};

fn clip() -> Clip {
    Clip {
        id: ClipId::from_stored(1),
        source: SourceId::parse("usb").unwrap(),
        start: Instant::from_micros(1_759_000_000_000_000).unwrap(),
        filename: "usb-x.flac".into(),
        path: "ingest/usb/usb-x.flac".into(),
    }
}

/// `s` seconds into the clip.
fn at(s: f64) -> Instant {
    clip().start.plus_seconds(s).unwrap()
}

fn span(a: f64, b: f64) -> Span {
    Span::new(at(a), at(b)).unwrap()
}

/// A segment whose words are one per whitespace token, evenly spaced.
fn segment(start: f64, end: f64, text: &str) -> HeardSegment {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let step = (end - start) / tokens.len() as f64;
    HeardSegment {
        start,
        end,
        text: text.into(),
        words: Some(
            tokens
                .iter()
                .enumerate()
                .map(|(i, t)| HeardWord {
                    start: start + step * i as f64,
                    end: start + step * (i + 1) as f64,
                    text: format!(" {t}"),
                    probability: Some(0.9),
                })
                .collect(),
        ),
    }
}

fn heard(segments: Vec<HeardSegment>) -> Heard {
    Heard {
        language: Some("nl".into()),
        segments,
    }
}

fn edit(id: i64, act: Act) -> Edit {
    Edit {
        id: EditId::from_stored(id),
        at: at(100.0 + id as f64),
        act,
    }
}

fn lines(
    heard: &Heard,
    voices: Option<&Voices>,
    speech: &Speech,
    edits: &[Edit],
) -> Vec<(String, Option<Speaker>)> {
    let clip = clip();
    render(&Input {
        clip: &clip,
        heard: Some(heard),
        voices,
        speech,
        edits,
        enrolled: &[],
    })
    .lines
    .iter()
    .map(|l| (l.text().to_owned(), l.speaker().cloned()))
    .collect()
}

fn texts(heard: &Heard, voices: Option<&Voices>, speech: &Speech, edits: &[Edit]) -> Vec<String> {
    lines(heard, voices, speech, edits)
        .into_iter()
        .map(|(t, _)| t)
        .collect()
}

#[test]
fn without_diarization_a_line_is_a_segment() {
    let h = heard(vec![
        segment(0.0, 2.0, "goede morgen"),
        segment(2.0, 4.0, "hoe gaat het"),
    ]);
    assert_eq!(
        texts(&h, None, &Speech::default(), &[]),
        ["goede morgen", "hoe gaat het"]
    );
}

#[test]
fn a_line_breaks_where_the_speaker_changes_between_sentences_and_where_the_segment_ends() {
    let h = heard(vec![
        segment(0.0, 4.0, "een twee. drie vier"),
        segment(4.0, 6.0, "vijf zes"),
    ]);
    let v = Voices {
        turns: vec![
            VoiceTurn {
                speaker: "A".into(),
                start: 0.0,
                end: 2.0,
            },
            VoiceTurn {
                speaker: "B".into(),
                start: 2.0,
                end: 6.0,
            },
        ],
        prints: vec![],
    };
    // B speaks across the segment boundary at 4 s: still two lines, never one.
    assert_eq!(
        texts(&h, Some(&v), &Speech::default(), &[]),
        ["een twee.", "drie vier", "vijf zes"]
    );
}

#[test]
fn a_speaker_change_inside_a_sentence_does_not_break_it() {
    // Seen in the shadow diff: "They don't know that. Did | they say ..." when
    // a diarized boundary landed one word early.
    let h = heard(vec![segment(
        0.0,
        6.0,
        "they do not know that. did they say two tests",
    )]);
    let v = Voices {
        turns: vec![
            VoiceTurn {
                speaker: "A".into(),
                start: 0.0,
                end: 3.4,
            },
            VoiceTurn {
                speaker: "B".into(),
                start: 3.4,
                end: 6.0,
            },
        ],
        prints: vec![],
    };
    let got = lines(&h, Some(&v), &Speech::default(), &[]);
    let shown: Vec<(&str, String)> = got
        .iter()
        .map(|(t, s)| {
            (
                t.as_str(),
                match s {
                    Some(Speaker::Cluster(c)) => c.clone(),
                    _ => String::new(),
                },
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            ("they do not know that.", "A".to_owned()),
            ("did they say two tests", "B".to_owned())
        ]
    );
}

#[test]
fn a_run_shorter_than_half_a_second_goes_to_its_neighbour() {
    let h = heard(vec![segment(0.0, 3.0, "a b c d e f")]);
    let v = Voices {
        turns: vec![
            VoiceTurn {
                speaker: "A".into(),
                start: 0.0,
                end: 1.4,
            },
            VoiceTurn {
                speaker: "B".into(),
                start: 1.4,
                end: 1.6,
            },
            VoiceTurn {
                speaker: "A".into(),
                start: 1.6,
                end: 3.0,
            },
        ],
        prints: vec![],
    };
    assert_eq!(
        texts(&h, Some(&v), &Speech::default(), &[]),
        ["a b c d e f"]
    );
}

#[test]
fn model_artifacts_are_dropped_with_their_reason() {
    let h = heard(vec![
        segment(0.0, 2.0, "Thank you."),
        segment(
            2.0,
            8.0,
            "everything everything everything everything everything everything",
        ),
        segment(8.0, 9.0, "..."),
        segment(9.0, 11.0, "echte woorden"),
    ]);
    let quiet = Speech {
        seconds: Some(5.0),
        regions: Some(vec![(9.0, 11.0)]),
    };
    let clip = clip();
    let out = render(&Input {
        clip: &clip,
        heard: Some(&h),
        voices: None,
        speech: &quiet,
        edits: &[],
        enrolled: &[],
    });
    let why: Vec<Why> = out.dropped.iter().map(|d| d.why).collect();
    assert_eq!(
        why,
        [Why::SilencePhrase, Why::RepetitionLoop, Why::Wordless]
    );
    assert_eq!(out.lines.len(), 1);
}

#[test]
fn thank_you_where_speech_was_heard_is_kept() {
    let h = heard(vec![segment(0.0, 2.0, "Thank you.")]);
    let heard_it = Speech {
        seconds: Some(20.0),
        regions: Some(vec![(0.0, 2.0)]),
    };
    assert_eq!(texts(&h, None, &heard_it, &[]), ["Thank you."]);
}

#[test]
fn a_persons_words_own_their_span_and_say_so() {
    let h = heard(vec![segment(0.0, 4.0, "een twee drie vier")]);
    let fix = edit(
        1,
        Act::Words {
            clip: ClipId::from_stored(1),
            span: span(2.0, 4.0),
            over: span(2.0, 4.0),
            text: Text::new("DRIE VIER").unwrap(),
            checked: true,
        },
    );
    let clip = clip();
    let out = render(&Input {
        clip: &clip,
        heard: Some(&h),
        voices: None,
        speech: &Speech::default(),
        edits: &[fix],
        enrolled: &[],
    });
    let shown: Vec<(&str, Author, bool)> = out
        .lines
        .iter()
        .map(|l| (l.text(), l.by(), l.checked()))
        .collect();
    assert_eq!(
        shown,
        [
            ("een twee", Author::Model, false),
            ("DRIE VIER", Author::Person(EditId::from_stored(1)), true)
        ]
    );
    assert!(
        out.dropped
            .iter()
            .all(|d| d.why == Why::Replaced(EditId::from_stored(1)))
    );
    assert_eq!(out.dropped.len(), 2);
}

#[test]
fn nobody_spoke_removes_the_words_and_writes_no_line() {
    let h = heard(vec![segment(0.0, 2.0, "uitgevonden tekst")]);
    let mark = edit(
        1,
        Act::NoSpeech {
            clip: ClipId::from_stored(1),
            span: span(0.0, 2.0),
        },
    );
    assert!(texts(&h, None, &Speech::default(), &[mark]).is_empty());
}

#[test]
fn a_later_act_wins_and_a_retraction_takes_one_back() {
    let h = heard(vec![segment(0.0, 2.0, "model")]);
    let words = |id, t: &str| {
        edit(
            id,
            Act::Words {
                clip: ClipId::from_stored(1),
                span: span(0.0, 2.0),
                over: span(0.0, 2.0),
                text: Text::new(t).unwrap(),
                checked: true,
            },
        )
    };
    let retract = |id, of| {
        edit(
            id,
            Act::Retract {
                edit: EditId::from_stored(of),
            },
        )
    };
    let s = Speech::default();
    assert_eq!(
        texts(&h, None, &s, &[words(1, "eerste"), words(2, "tweede")]),
        ["tweede"]
    );
    assert_eq!(
        texts(
            &h,
            None,
            &s,
            &[words(1, "eerste"), words(2, "tweede"), retract(3, 2)]
        ),
        ["eerste"]
    );
    assert_eq!(
        texts(&h, None, &s, &[words(1, "eerste"), retract(2, 1)]),
        ["model"]
    );
    assert_eq!(
        texts(
            &h,
            None,
            &s,
            &[words(1, "eerste"), retract(2, 1), retract(3, 2)]
        ),
        ["eerste"],
        "retracting a retraction restores"
    );
}

#[test]
fn the_speaker_is_a_persons_naming_then_the_voice_then_the_print_then_the_label() {
    let h = heard(vec![segment(0.0, 2.0, "hallo daar")]);
    let v = Voices {
        turns: vec![VoiceTurn {
            speaker: "SPEAKER_00".into(),
            start: 0.0,
            end: 2.0,
        }],
        prints: vec![("SPEAKER_00".into(), vec![1.0, 0.0])],
    };
    let s = Speech::default();
    let clip = clip();
    let speaker = |edits: &[Edit], enrolled: &[Voiceprint]| {
        render(&Input {
            clip: &clip,
            heard: Some(&h),
            voices: Some(&v),
            speech: &s,
            edits,
            enrolled,
        })
        .lines[0]
            .speaker()
            .cloned()
    };
    assert_eq!(
        speaker(&[], &[]),
        Some(Speaker::Cluster("SPEAKER_00".into()))
    );
    let prints = [Voiceprint {
        person: "Alex".into(),
        vector: vec![1.0, 0.0],
    }];
    assert!(
        matches!(speaker(&[], &prints), Some(Speaker::Guess { ref name, .. }) if name == "Alex")
    );
    let voice = edit(
        1,
        Act::Voice {
            source: SourceId::parse("usb").unwrap(),
            cluster: "SPEAKER_00".into(),
            name: Name::new("Sam").unwrap(),
        },
    );
    assert!(
        matches!(speaker(std::slice::from_ref(&voice), &prints), Some(Speaker::Voice { ref name, .. }) if name.as_str() == "Sam")
    );
    let named = edit(
        2,
        Act::Speaker {
            clip: ClipId::from_stored(1),
            span: span(0.0, 2.0),
            name: Name::new("Robin").unwrap(),
            enrol: true,
        },
    );
    assert!(
        matches!(speaker(&[voice, named], &prints), Some(Speaker::Named { ref name, .. }) if name.as_str() == "Robin")
    );
}

#[test]
fn acts_on_another_clip_change_nothing() {
    let h = heard(vec![segment(0.0, 2.0, "eigen woorden")]);
    let elsewhere = edit(
        1,
        Act::NoSpeech {
            clip: ClipId::from_stored(2),
            span: span(0.0, 2.0),
        },
    );
    assert_eq!(
        texts(&h, None, &Speech::default(), &[elsewhere]),
        ["eigen woorden"]
    );
}

#[test]
fn render_is_deterministic_and_no_model_word_vanishes_without_a_reason() {
    let h = heard(vec![
        segment(0.0, 3.0, "een twee drie"),
        segment(3.0, 5.0, "Thank you."),
        segment(5.0, 9.0, "vier vijf zes zeven"),
    ]);
    let v = Voices {
        turns: vec![
            VoiceTurn {
                speaker: "A".into(),
                start: 0.0,
                end: 6.0,
            },
            VoiceTurn {
                speaker: "B".into(),
                start: 6.0,
                end: 9.0,
            },
        ],
        prints: vec![],
    };
    let speech = Speech {
        seconds: Some(8.0),
        regions: Some(vec![(0.0, 3.0), (5.0, 9.0)]),
    };
    let fix = edit(
        1,
        Act::Words {
            clip: ClipId::from_stored(1),
            span: span(5.0, 6.0),
            over: span(5.0, 6.0),
            text: Text::new("VIER").unwrap(),
            checked: true,
        },
    );
    let clip = clip();
    let input = Input {
        clip: &clip,
        heard: Some(&h),
        voices: Some(&v),
        speech: &speech,
        edits: std::slice::from_ref(&fix),
        enrolled: &[],
    };
    let first = render(&input);
    assert_eq!(first, render(&input));

    let model_words = 3 + 4; // the dropped segment is accounted for as one drop
    let shown_model_words: usize = first
        .lines
        .iter()
        .filter(|l| l.by() == Author::Model)
        .map(|l| l.text().split_whitespace().count())
        .sum();
    let replaced = first
        .dropped
        .iter()
        .filter(|d| matches!(d.why, Why::Replaced(_)))
        .count();
    assert_eq!(shown_model_words + replaced, model_words);
    assert_eq!(
        first
            .dropped
            .iter()
            .filter(|d| d.why == Why::SilencePhrase)
            .count(),
        1
    );
}

#[test]
fn two_neighbouring_corrections_whose_edges_overlap_both_stand() {
    // Seen in the shadow diff: two corrected lines starting in the same second
    // cancelled each other, and the model's words came back.
    let h = heard(vec![segment(0.0, 4.0, "yeah yeah line up")]);
    let words = |id, a, b, t: &str| {
        edit(
            id,
            Act::Words {
                clip: ClipId::from_stored(1),
                span: span(a, b),
                over: span(a, b),
                text: Text::new(t).unwrap(),
                checked: true,
            },
        )
    };
    let shown = texts(
        &h,
        None,
        &Speech::default(),
        &[
            words(1, 0.0, 2.2, "Yeah, yeah, yeah."),
            words(2, 1.9, 4.0, "Line up, please."),
        ],
    );
    assert_eq!(shown, ["Yeah, yeah, yeah.", "Line up, please."]);
}

#[test]
fn a_correction_replaces_the_whole_line_it_corrected_even_where_it_narrowed_the_words() {
    // Seen in the shadow diff: a correction that moved a line's start later
    // let the model's words before the new start come back.
    let h = heard(vec![segment(0.0, 4.0, "hi here's hello hello hi there")]);
    let narrowed = edit(
        1,
        Act::Words {
            clip: ClipId::from_stored(1),
            span: span(3.0, 4.0),
            over: span(0.0, 4.0),
            text: Text::new("Hi there.").unwrap(),
            checked: true,
        },
    );
    assert_eq!(
        texts(&h, None, &Speech::default(), &[narrowed]),
        ["Hi there."]
    );
}
