//! The fourth credential plane (docs/architecture.md): per-device, write-only
//! ingest tokens. A token authorizes `PUT` for exactly one source — not read,
//! not list, not another device's directory — so a stolen recorder can append
//! audio and do nothing else, and is revoked by deleting its line.
//!
//! The file lives outside the repo and outside the image (`--tokens` points at
//! a mounted secret); one `<source> <token>` per line, `#` comments and blank
//! lines ignored. Read once at startup — rotation is a pod rollout, which is
//! how every other secret here already rotates. Unconfigured means open: the
//! repo's standing inert-unless-configured pattern, so dev and tests carry no
//! ceremony and the fleet raises the gate by mounting the file.

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;

/// The parsed token table. Tokens are held as sha-256 digests so an equality
/// check compares fixed-length hashes — timing reveals nothing about how much
/// of a guess matched.
pub struct Tokens {
    by_source: HashMap<String, Vec<[u8; 32]>>,
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// The authorization verdict, split three ways so the surface can answer 401
/// (who are you) and 403 (not yours) distinctly — a recorder mis-holding a
/// *valid* neighbour's token is a configuration fault worth naming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allowed,
    UnknownToken,
    WrongSource,
}

impl Tokens {
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let mut by_source: HashMap<String, Vec<[u8; 32]>> = HashMap::new();
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
            by_source
                .entry(source.to_owned())
                .or_default()
                .push(digest(token.trim()));
        }
        Ok(Self { by_source })
    }

    pub fn check(&self, source: &str, bearer: &str) -> Verdict {
        let hashed = digest(bearer);
        if self
            .by_source
            .get(source)
            .is_some_and(|list| list.contains(&hashed))
        {
            return Verdict::Allowed;
        }
        if self.by_source.values().any(|list| list.contains(&hashed)) {
            return Verdict::WrongSource;
        }
        Verdict::UnknownToken
    }
}

/// Equality for single-token gates (the read side), through the same
/// digest-then-compare shape as the table above.
pub fn same_token(presented: &str, expected: &str) -> bool {
    digest(presented) == digest(expected)
}
