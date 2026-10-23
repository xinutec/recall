//! Naming a voice from its embedding (`transcript::voice`), against the
//! voiceprints stored here.

crate::statements! {
    ENROLLED: Meaning =
        "SELECT s.owner, s.name, e.vector FROM speakers s
         JOIN speaker_embeddings e ON e.speaker_id = s.id";
    SOURCE_OWNERS: Meaning = "SELECT id, owner FROM sources";
}

pub use transcript::voice::{Guess, SOFTMAX_TEMPERATURE, Voiceprint, match_one};

// --- reading the enrolled people ---------------------------------------------

/// Each person's enrolled voiceprints, and whose each recording is: a voice
/// is named only among its recording's owner's people.
#[derive(Debug, Default)]
pub struct Roster {
    prints: std::collections::HashMap<Option<String>, Vec<Voiceprint>>,
    owners: std::collections::HashMap<String, Option<String>>,
}

impl Roster {
    /// Every enrolled voiceprint, by owner. A row that will not parse is
    /// skipped, not made a zero vector, which could become somebody's best
    /// match on quiet audio.
    ///
    /// # Errors
    /// If the database refuses.
    pub fn read(conn: &rusqlite::Connection) -> rusqlite::Result<Self> {
        let mut roster = Self::default();
        let mut stmt = ENROLLED.prepare(conn)?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (owner, person, raw) = row?;
            if let Ok(vector) = serde_json::from_str::<Vec<f64>>(&raw) {
                roster
                    .prints
                    .entry(owner)
                    .or_default()
                    .push(Voiceprint { person, vector });
            }
        }
        let mut stmt = SOURCE_OWNERS.prepare(conn)?;
        for row in stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))? {
            let (source, owner) = row?;
            roster.owners.insert(source, owner);
        }
        Ok(roster)
    }

    /// Whose recordings `source` holds.
    #[must_use]
    pub fn owner_of(&self, source: &str) -> Option<&str> {
        self.owners.get(source).and_then(Option::as_deref)
    }

    /// Every source with a row.
    pub fn sources(&self) -> impl Iterator<Item = &str> {
        self.owners.keys().map(String::as_str)
    }

    /// The prints a voice on `source` is matched against.
    #[must_use]
    pub fn for_source(&self, source: &str) -> &[Voiceprint] {
        let owner = self.owner_of(source).map(str::to_owned);
        self.prints.get(&owner).map_or(&[], Vec::as_slice)
    }
}
