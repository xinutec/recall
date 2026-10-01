//! Public test sets with a transcript per utterance.

use std::io::BufRead;
use std::path::{Path, PathBuf};

/// One recorded utterance and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Utterance {
    pub audio: PathBuf,
    /// The reference text, as the corpus spells it.
    pub text: String,
    /// Who is speaking, as precisely as the corpus says: a `LibriSpeech` reader,
    /// or only the corpus for FLEURS, which names no speakers.
    pub speaker: String,
    pub lang: String,
}

/// One `LibriSpeech` reader's utterances: `<root>/<speaker>/<chapter>/` holds
/// the `.flac` files and a `<speaker>-<chapter>.trans.txt` of `<id> <TEXT>` lines.
pub fn librispeech(root: &Path, speaker: &str) -> std::io::Result<Vec<Utterance>> {
    let mut out = Vec::new();
    let mut chapters: Vec<PathBuf> = std::fs::read_dir(root.join(speaker))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    chapters.sort();
    for chapter in chapters {
        let Some(name) = chapter.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let trans = chapter.join(format!("{speaker}-{name}.trans.txt"));
        for line in std::io::BufReader::new(std::fs::File::open(trans)?).lines() {
            let line = line?;
            let Some((id, text)) = line.split_once(' ') else {
                continue;
            };
            out.push(Utterance {
                audio: chapter.join(format!("{id}.flac")),
                text: text.to_lowercase(),
                speaker: format!("librispeech-{speaker}"),
                lang: "en".into(),
            });
        }
    }
    Ok(out)
}

/// A FLEURS split: tab-separated `<id> <file> <raw> <normalised> ...` rows,
/// the audio in `audio_dir`. The normalised transcript is the reference.
pub fn fleurs(tsv: &Path, audio_dir: &Path, lang: &str) -> std::io::Result<Vec<Utterance>> {
    let mut out = Vec::new();
    for line in std::io::BufReader::new(std::fs::File::open(tsv)?).lines() {
        let line = line?;
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 4 {
            continue;
        }
        out.push(Utterance {
            audio: audio_dir.join(fields[1]),
            text: fields[3].to_string(),
            speaker: format!("fleurs-{lang}"),
            lang: lang.to_string(),
        });
    }
    Ok(out)
}
