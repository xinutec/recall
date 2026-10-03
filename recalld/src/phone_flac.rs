//! A phone's own copy of a minute, stored as FLAC instead of WAV (#1842).
//!
//! The phones wrote their copy as WAV until they learned FLAC: the same samples
//! at about seven times the size. [`convert`] re-encodes each stored one as
//! `<stem>.phone.flac`, keeps it only if it decodes to exactly the WAV's
//! samples, renames every row that names the clip, and only then deletes the WAV.
//!
//! Run by hand (`recalld --root <root> phone-flac`), a dry run unless `--apply`.
//! Each step can be repeated, so an interrupted run is finished by the next.

use crate::sql;
use rusqlite::{Connection, OptionalExtension};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

crate::statements! {
    /// Stored WAVs no job is working on, received before `?1`.
    CANDIDATES: Ingest =
        "SELECT s.filename, s.source FROM segments s
         WHERE s.filename LIKE '%.wav' AND s.received_utc < ?1
           AND NOT EXISTS (SELECT 1 FROM jobs j WHERE j.filename = s.filename
                                                  AND j.state IN ('queued', 'leased'))
         ORDER BY s.filename";
    BUSY: Ingest =
        "SELECT 1 FROM jobs WHERE filename = ?1 AND state IN ('queued', 'leased')";
    IS_PHONE: Meaning =
        "SELECT 1 FROM sources WHERE id = ?1 AND kind = 'tcp_pcm'";
    /// The new name's row first: the tables below reference `segments`.
    COPY_SEGMENT: Ingest =
        "INSERT INTO segments (filename, source, start_utc, bytes, sha256, received_utc, sent_utc)
         SELECT ?2, source, start_utc, ?3, ?4, received_utc, sent_utc
         FROM segments WHERE filename = ?1";
    RENAME_LEVELS: Ingest = "UPDATE segment_levels SET filename = ?2 WHERE filename = ?1";
    RENAME_SPEECH: Ingest = "UPDATE segment_speech SET filename = ?2 WHERE filename = ?1";
    RENAME_JOBS: Ingest = "UPDATE jobs SET filename = ?2 WHERE filename = ?1";
    RENAME_LEDGER: Ingest = "UPDATE pass_ledger SET filename = ?2 WHERE filename = ?1";
    RENAME_REQUESTS: Ingest =
        "UPDATE retranscribe_requests SET filename = ?2 WHERE filename = ?1";
    DROP_SEGMENT: Ingest = "DELETE FROM segments WHERE filename = ?1";
    REPOINT: Meaning = "UPDATE audio_segments SET path = ?2 WHERE path = ?1";
}

/// What a run did, or in a dry run would do.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Converted {
    pub converted: usize,
    pub wav_bytes: u64,
    pub flac_bytes: u64,
    /// Clips left as they are, each with the reason.
    pub skipped: Vec<(String, String)>,
}

/// Convert every phone WAV received before `before` (ISO-8601, the stored
/// spelling). Without `apply` nothing is written; sizes are the WAVs' only.
///
/// # Errors
/// If a plane refuses. A clip that cannot be converted is skipped, not an error.
pub fn convert(
    root: &Path,
    meaning: &Connection,
    ingest: &mut Connection,
    before: &str,
    apply: bool,
) -> rusqlite::Result<Converted> {
    let candidates: Vec<(String, String)> = CANDIDATES
        .prepare(ingest)?
        .query_map([before], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut done = Converted::default();
    for (filename, source) in candidates {
        if IS_PHONE
            .query_row(meaning, [&source], |_| Ok(()))
            .optional()?
            .is_none()
        {
            continue;
        }
        let dir = crate::store::source_dir(root, &source);
        match one(&dir, &filename, meaning, ingest, apply) {
            Ok(Some((wav, flac))) => {
                done.converted += 1;
                done.wav_bytes += wav;
                done.flac_bytes += flac;
            }
            Ok(None) => done
                .skipped
                .push((filename, "a job took it during the run".to_owned())),
            Err(Skip::Plane(err)) => return Err(err),
            Err(Skip::Clip(why)) => done.skipped.push((filename, why)),
        }
    }
    Ok(done)
}

enum Skip {
    Plane(rusqlite::Error),
    Clip(String),
}

impl From<rusqlite::Error> for Skip {
    fn from(err: rusqlite::Error) -> Self {
        Self::Plane(err)
    }
}

/// One clip: `Some((wav bytes, flac bytes))` when converted, `None` when a job
/// claimed it before the rename.
fn one(
    dir: &Path,
    filename: &str,
    meaning: &Connection,
    ingest: &mut Connection,
    apply: bool,
) -> Result<Option<(u64, u64)>, Skip> {
    let clip = |why: String| Skip::Clip(why);
    let stem = filename.strip_suffix(".wav").unwrap_or(filename);
    let flac_name = format!("{stem}.phone.flac");
    let wav_path = dir.join(filename);
    let flac_path = dir.join(&flac_name);
    let wav = std::fs::read(&wav_path).map_err(|err| clip(format!("cannot read: {err}")))?;
    let (rate, pcm) = plain_pcm(&wav).ok_or_else(|| {
        clip("not the phones' plain 16-bit mono WAV; left for a person".to_owned())
    })?;
    if !apply {
        return Ok(Some((len(&wav), 0)));
    }
    // The samples come from the bytes, not from a decoder reading the WAV: a
    // copy a crash cut short says zero samples in its header, so the check
    // must not rest on how a decoder treats that.
    let flac = match std::fs::read(&flac_path) {
        Ok(existing) => existing,
        Err(_) => encode(pcm, rate).map_err(clip)?,
    };
    if decode(&flac).map_err(clip)? != pcm {
        return Err(clip(
            "the FLAC does not decode to the WAV's samples".to_owned(),
        ));
    }
    if !flac_path.exists() {
        land(dir, &flac_path, &flac).map_err(|err| clip(format!("cannot write: {err}")))?;
    }
    let old_path = wav_path.to_string_lossy();
    let new_path = flac_path.to_string_lossy();
    REPOINT.execute(meaning, (&*old_path, &*new_path))?;
    let tx = sql::write(ingest)?;
    if BUSY
        .query_row(&tx, [filename], |_| Ok(()))
        .optional()?
        .is_some()
    {
        drop(tx);
        REPOINT.execute(meaning, (&*new_path, &*old_path))?;
        return Ok(None);
    }
    let sha = crate::ingest::sha256_hex(&flac);
    let bytes = i64::try_from(flac.len()).unwrap_or(i64::MAX);
    COPY_SEGMENT.execute(&tx, (filename, &flac_name, bytes, &sha))?;
    for rename in [
        RENAME_LEVELS,
        RENAME_SPEECH,
        RENAME_JOBS,
        RENAME_LEDGER,
        RENAME_REQUESTS,
    ] {
        rename.execute(&tx, (filename, &flac_name))?;
    }
    DROP_SEGMENT.execute(&tx, [filename])?;
    tx.commit()?;
    std::fs::remove_file(&wav_path)
        .map_err(|err| clip(format!("converted, but the WAV stays: {err}")))?;
    Ok(Some((len(&wav), len(&flac))))
}

fn len(bytes: &[u8]) -> u64 {
    u64::try_from(bytes.len()).unwrap_or(u64::MAX)
}

/// The sample rate and PCM of the 44-byte-header WAV the phones write, or
/// `None` for anything else.
fn plain_pcm(wav: &[u8]) -> Option<(u32, &[u8])> {
    let u16_at = |at: usize| Some(u16::from_le_bytes(wav.get(at..at + 2)?.try_into().ok()?));
    let u32_at = |at: usize| Some(u32::from_le_bytes(wav.get(at..at + 4)?.try_into().ok()?));
    let plain = wav.get(0..4)? == b"RIFF"
        && wav.get(8..16)? == b"WAVEfmt "
        && u32_at(16)? == 16
        && u16_at(20)? == 1
        && u16_at(22)? == 1
        && u16_at(34)? == 16
        && wav.get(36..40)? == b"data";
    let body = wav.get(44..)?;
    // An odd last byte is half a sample; no decoder returns it.
    (plain && body.len() >= 2).then(|| (u32_at(24).unwrap_or(0), &body[..body.len() & !1]))
}

fn encode(pcm: &[u8], rate: u32) -> Result<Vec<u8>, String> {
    let rate = rate.to_string();
    ffmpeg(
        &[
            "-f",
            "s16le",
            "-ar",
            &rate,
            "-ac",
            "1",
            "-i",
            "pipe:0",
            "-c:a",
            "flac",
            "-compression_level",
            "5",
            "-f",
            "flac",
            "pipe:1",
        ],
        pcm,
    )
}

fn decode(flac: &[u8]) -> Result<Vec<u8>, String> {
    ffmpeg(
        &["-i", "pipe:0", "-f", "s16le", "-c:a", "pcm_s16le", "pipe:1"],
        flac,
    )
}

/// Run ffmpeg over `input`, returning what it writes. The input is fed from a
/// thread: ffmpeg writes while it reads, and a full pipe either way would
/// otherwise stall both.
fn ffmpeg(args: &[&str], input: &[u8]) -> Result<Vec<u8>, String> {
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("ffmpeg: {err}"))?;
    let mut stdin = child.stdin.take().ok_or("ffmpeg: no stdin")?;
    let input = input.to_vec();
    let feeder = std::thread::spawn(move || stdin.write_all(&input));
    let out = child
        .wait_with_output()
        .map_err(|err| format!("ffmpeg: {err}"))?;
    let fed = feeder.join().map_err(|_| "ffmpeg: the feeder panicked")?;
    if !out.status.success() {
        return Err(format!(
            "ffmpeg: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    fed.map_err(|err| format!("ffmpeg: {err}"))?;
    Ok(out.stdout)
}

/// Write `bytes` to `path` durably: fsynced in the ingest door's temp
/// directory, then renamed in.
fn land(dir: &Path, path: &PathBuf, bytes: &[u8]) -> std::io::Result<()> {
    let tmpdir = dir.parent().unwrap_or(dir).join(".tmp");
    std::fs::create_dir_all(&tmpdir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(&tmpdir)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist_noclobber(path).map_err(|err| err.error)?;
    std::fs::File::open(dir)?.sync_all()
}
