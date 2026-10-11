//! A fleet in a temporary root: clips with stored results in the ingest plane,
//! their audio rows in the meaning plane, and the rendered view over them.

use audiocore::job::Kind;
use rusqlite::Connection;
use transcript::Clip;

pub struct Fleet {
    pub dir: tempfile::TempDir,
    pub meaning: Connection,
    pub ingest: Connection,
}

/// One shown line: id, text, tier, model, speaker label, cluster.
#[derive(Debug, Clone, PartialEq)]
pub struct Shown {
    pub id: i64,
    pub text: String,
    pub tier: String,
    pub model: String,
    pub label: Option<String>,
    pub cluster: Option<String>,
}

/// A model segment `start..end` seconds into the clip, its words spread evenly
/// with a pause between them.
pub fn segment(start: f64, end: f64, text: &str) -> String {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let step = (end - start) / tokens.len() as f64;
    let words: Vec<String> = tokens
        .iter()
        .enumerate()
        .map(|(i, t)| {
            format!(
                r#"{{"start":{},"end":{},"word":" {t}","probability":0.9}}"#,
                start + step * i as f64,
                start + step * (i + 1) as f64 - 0.1
            )
        })
        .collect();
    format!(
        r#"{{"start":{start},"end":{end},"text":" {text}","words":[{}]}}"#,
        words.join(",")
    )
}

impl Fleet {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("tmp");
        let meaning = recalld::work::open_write(dir.path()).expect("meaning");
        recalld::meaning_schema::ensure(&meaning).expect("schema");
        let ingest = recalld::store::open(dir.path()).expect("ingest");
        Self {
            dir,
            meaning,
            ingest,
        }
    }

    /// A one-minute clip of `source` (registered with `kind` once) starting at
    /// `start` (`YYYYMMDDTHHMMSS`), transcribed as `segments` and, if given,
    /// diarized as `turns` (speaker, start, end).
    pub fn clip(
        &self,
        source: &str,
        kind: &str,
        start: &str,
        segments: &[(f64, f64, &str)],
        turns: Option<&[(&str, f64, f64)]>,
    ) -> Clip {
        let filename = format!("{source}-{start}.flac");
        let at = chrono::NaiveDateTime::parse_from_str(start, "%Y%m%dT%H%M%S")
            .expect("stamp")
            .and_utc();
        let iso = |t: chrono::DateTime<chrono::Utc>| audiocore::instant::python_isoformat_utc(t);
        recalld::store::insert(
            &self.ingest,
            &recalld::store::Row {
                source: source.into(),
                filename: filename.clone(),
                start_utc: at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                bytes: 1,
                sha256: "x".into(),
                received_utc: at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                sent_utc: None,
            },
        )
        .expect("segment");
        self.meaning
            .execute(
                "INSERT OR IGNORE INTO sources (id, name, kind) VALUES (?1, ?1, ?2)",
                (source, kind),
            )
            .expect("source");
        self.meaning
            .execute(
                "INSERT INTO audio_segments (source_id, path, start_utc, end_utc, sample_rate, channels)
                 VALUES (?1, ?2, ?3, ?4, 48000, 1)",
                (
                    source,
                    self.dir
                        .path()
                        .join("ingest")
                        .join(source)
                        .join(&filename)
                        .to_string_lossy()
                        .into_owned(),
                    iso(at),
                    iso(at + chrono::Duration::seconds(60)),
                ),
            )
            .expect("audio row");
        let body: Vec<String> = segments
            .iter()
            .map(|(a, b, t)| segment(*a, *b, t))
            .collect();
        self.finish(
            Kind::TranscribeSegment,
            &filename,
            &format!(
                r#"{{"ok":true,"result":{{"language":"nl","segments":[{}]}}}}"#,
                body.join(",")
            ),
        );
        if let Some(turns) = turns {
            let turns: Vec<String> = turns
                .iter()
                .map(|(s, a, b)| format!(r#"{{"speaker":"{s}","start":{a},"end":{b}}}"#))
                .collect();
            self.finish(
                Kind::DiarizeSegment,
                &filename,
                &format!(
                    r#"{{"ok":true,"result":{{"turns":[{}],"speakers":[]}}}}"#,
                    turns.join(",")
                ),
            );
        }
        recalld::clips::by_filename(&self.ingest, &filename)
            .expect("clip")
            .expect("a clip")
    }

    fn finish(&self, kind: Kind, filename: &str, result: &str) {
        self.ingest
            .execute(
                "INSERT INTO jobs (kind, filename, state, created_utc, done_utc, result)
                 VALUES (?1, ?2, 'done', '2026-10-04T12:02:00Z', '2026-10-04T12:02:00Z', ?3)",
                (kind, filename, result),
            )
            .expect("result");
    }

    pub fn refresh(&self) {
        recalld::view::refresh(&self.meaning, &self.ingest, self.dir.path(), false)
            .expect("refresh");
    }

    /// The shown lines, in reading order.
    pub fn shown(&self) -> Vec<Shown> {
        self.meaning
            .prepare(
                "SELECT id, text, tier, asr_model, speaker_label, speaker_cluster FROM rendered_lines
                 WHERE hidden_reason IS NULL ORDER BY start_utc, id",
            )
            .expect("prepare")
            .query_map([], |r| {
                Ok(Shown {
                    id: r.get(0)?,
                    text: r.get(1)?,
                    tier: r.get(2)?,
                    model: r.get(3)?,
                    label: r.get(4)?,
                    cluster: r.get(5)?,
                })
            })
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("rows")
    }

    pub fn texts(&self) -> Vec<String> {
        self.shown().into_iter().map(|l| l.text).collect()
    }

    /// Hidden rows: (id, text, reason).
    pub fn hidden(&self) -> Vec<(i64, String, String)> {
        self.meaning
            .prepare(
                "SELECT id, text, hidden_reason FROM rendered_lines
                 WHERE hidden_reason IS NOT NULL ORDER BY start_utc, id",
            )
            .expect("prepare")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("rows")
    }

    pub fn root(&self) -> &std::path::Path {
        self.dir.path()
    }

    /// The acts in the log, by kind name.
    pub fn act_kinds(&self) -> Vec<String> {
        recalld::edit_log::all(&self.meaning)
            .expect("log")
            .into_iter()
            .map(|e| {
                serde_json::to_value(&e.act)
                    .ok()
                    .and_then(|v| v.as_object().and_then(|o| o.keys().next().cloned()))
                    .unwrap_or_default()
            })
            .collect()
    }
}
