//! Per-device ingest tokens (docs/architecture.md): a token authorizes `PUT`
//! for one source only, so a stolen recorder can append audio and nothing else,
//! and is revoked by deleting its line.
//!
//! A `*` line grants every source, still write-only: the Mac's backfill
//! mirrors every device plus each uploaded meeting. No device gets `*`.
//!
//! One `<source> <token>` per line; `#` comments and blank lines ignored. A
//! mounted secret, read once at startup: rotation is a rollout. Unconfigured
//! means open, for dev and tests.

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;

/// Tokens are held as sha-256 digests, so comparison timing reveals nothing
/// about how much of a guess matched.
pub struct Tokens {
    by_source: HashMap<String, Vec<[u8; 32]>>,
    any_source: Vec<[u8; 32]>,
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// 401 (unknown) and 403 (another source's token) apart: a recorder holding a
/// neighbour's token is a configuration fault worth naming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allowed,
    UnknownToken,
    WrongSource,
}

impl Tokens {
    pub fn load(path: &Path) -> std::io::Result<Self> {
        Self::parse(&std::fs::read_to_string(path)?)
    }

    /// The fleet supplies it as `RECALLD_INGEST_TOKENS`, dev as a file.
    pub fn parse(text: &str) -> std::io::Result<Self> {
        let mut by_source: HashMap<String, Vec<[u8; 32]>> = HashMap::new();
        let mut any_source: Vec<[u8; 32]> = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((source, token)) = line.split_once(char::is_whitespace) else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "tokens file line is not `<source> <token>`",
                ));
            };
            if source == "*" {
                any_source.push(digest(token.trim()));
            } else {
                by_source
                    .entry(source.to_owned())
                    .or_default()
                    .push(digest(token.trim()));
            }
        }
        Ok(Self {
            by_source,
            any_source,
        })
    }

    pub fn check(&self, source: &str, bearer: &str) -> Verdict {
        let hashed = digest(bearer);
        if self
            .by_source
            .get(source)
            .is_some_and(|list| list.contains(&hashed))
            || self.any_source.contains(&hashed)
        {
            return Verdict::Allowed;
        }
        if self.by_source.values().any(|list| list.contains(&hashed)) {
            return Verdict::WrongSource;
        }
        Verdict::UnknownToken
    }
}

/// Equality for a single-token gate, compared as digests.
pub fn same_token(presented: &str, expected: &str) -> bool {
    digest(presented) == digest(expected)
}
