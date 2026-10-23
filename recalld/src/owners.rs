//! Whose recordings, names and glossary terms are whose. Each belongs to one
//! sign-in user; what was recorded before owners existed belongs to nobody
//! until an operator claims it (`recalld claim-unowned`).

use rusqlite::Connection;
use serde::Serialize;

crate::statements! {
    UNOWNED_SOURCES: Meaning = "SELECT count(*) FROM sources WHERE owner IS NULL";
    UNOWNED_SPEAKERS: Meaning = "SELECT count(*) FROM speakers WHERE owner IS NULL";
    CLAIM_SOURCES: Meaning = "UPDATE sources SET owner = ?1 WHERE owner IS NULL";
    CLAIM_SPEAKERS: Meaning = "UPDATE speakers SET owner = ?1 WHERE owner IS NULL";
    UNOWNED_TERMS: Meaning = "SELECT count(*) FROM vocabulary WHERE owner IS NULL";
    CLAIM_TERMS: Meaning = "UPDATE vocabulary SET owner = ?1 WHERE owner IS NULL";
    SOURCE_OWNER: Meaning = "SELECT owner FROM sources WHERE id = ?1";
}

/// What a claim found, and whether it was made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Claimed {
    pub owner: String,
    pub sources: i64,
    pub speakers: i64,
    /// Glossary terms.
    pub terms: i64,
    pub applied: bool,
}

/// Give every source and name nobody owns to `owner`; without `apply`, only
/// count them. One transaction: sources and names change together or not.
///
/// # Errors
/// If the meaning plane refuses.
pub fn claim_unowned(meaning: &Connection, owner: &str, apply: bool) -> rusqlite::Result<Claimed> {
    let tx = crate::sql::write_shared(meaning)?;
    let count = |stmt: &crate::sql::Sql| stmt.query_row(&tx, [], |r| r.get::<_, i64>(0));
    let claimed = Claimed {
        owner: owner.to_owned(),
        sources: count(&UNOWNED_SOURCES)?,
        speakers: count(&UNOWNED_SPEAKERS)?,
        terms: count(&UNOWNED_TERMS)?,
        applied: apply,
    };
    if apply {
        CLAIM_SOURCES.execute(&tx, [owner])?;
        CLAIM_SPEAKERS.execute(&tx, [owner])?;
        CLAIM_TERMS.execute(&tx, [owner])?;
        tx.commit()?;
    }
    Ok(claimed)
}

/// Whether the server may serve the people `allowed` admits (empty admits
/// anyone): with more than one, whatever nobody owns must be claimed first.
///
/// # Errors
/// What is unclaimed, or why it could not be read.
pub fn ready_for<S: std::hash::BuildHasher>(
    meaning: &Connection,
    allowed: &std::collections::HashSet<String, S>,
) -> Result<(), String> {
    if allowed.len() == 1 {
        return Ok(());
    }
    let count = |stmt: &crate::sql::Sql| {
        stmt.query_row(meaning, [], |r| r.get::<_, i64>(0))
            .map_err(|err| err.to_string())
    };
    let (sources, speakers, terms) = (
        count(&UNOWNED_SOURCES)?,
        count(&UNOWNED_SPEAKERS)?,
        count(&UNOWNED_TERMS)?,
    );
    if sources + speakers + terms == 0 {
        return Ok(());
    }
    Err(format!(
        "{sources} sources, {speakers} names and {terms} glossary terms belong to nobody, and \
         more than one person can sign in: claim them first \
         (`recalld claim-unowned --owner <user> --apply`)"
    ))
}

/// Whose recordings `source` holds; `None` for an unknown or unclaimed one.
///
/// # Errors
/// If the database refuses.
pub fn owner_of_source(meaning: &Connection, source: &str) -> rusqlite::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    Ok(SOURCE_OWNER
        .query_row(meaning, [source], |r| r.get(0))
        .optional()?
        .flatten())
}
