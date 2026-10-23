//! Whose recordings and names are whose. A source and a name belong to one
//! sign-in user; what was recorded before owners existed belongs to nobody
//! until an operator claims it (`recalld claim-unowned`).

use rusqlite::Connection;
use serde::Serialize;

crate::statements! {
    UNOWNED_SOURCES: Meaning = "SELECT count(*) FROM sources WHERE owner IS NULL";
    UNOWNED_SPEAKERS: Meaning = "SELECT count(*) FROM speakers WHERE owner IS NULL";
    CLAIM_SOURCES: Meaning = "UPDATE sources SET owner = ?1 WHERE owner IS NULL";
    CLAIM_SPEAKERS: Meaning = "UPDATE speakers SET owner = ?1 WHERE owner IS NULL";
}

/// What a claim found, and whether it was made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Claimed {
    pub owner: String,
    pub sources: i64,
    pub speakers: i64,
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
        applied: apply,
    };
    if apply {
        CLAIM_SOURCES.execute(&tx, [owner])?;
        CLAIM_SPEAKERS.execute(&tx, [owner])?;
        tx.commit()?;
    }
    Ok(claimed)
}
