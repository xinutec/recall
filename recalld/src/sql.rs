//! Every SQL statement recalld runs, declared where a test can check it.
//!
//! A statement is a named [`Sql`] in its module's [`statements!`](crate::statements) block and
//! runs only through [`Sql`]'s methods: clippy refuses rusqlite's
//! string-taking ones (`recalld/clippy.toml`). [`ALL`] gathers every module's
//! block, and `tests/integration/sql.rs` prepares each statement against the
//! migrated schema of the database it names, so a renamed column fails the
//! gate instead of the pass that first runs it.

use rusqlite::{Connection, Params, Row, Statement, Transaction, TransactionBehavior};

/// Which database a statement is written for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Db {
    /// `recall.sqlite`: turns, speakers, corrections ([`crate::meaning_schema`]).
    Meaning,
    /// `ingest.sqlite`: segments, jobs, ledgers ([`crate::ingest_schema`]).
    Ingest,
}

/// One declared statement.
#[derive(Debug, Clone, Copy)]
pub struct Sql {
    db: Db,
    text: &'static str,
}

#[expect(
    clippy::disallowed_methods,
    reason = "the one place rusqlite sees statement text"
)]
impl Sql {
    /// Only through [`statements!`](crate::statements), which also lists it for the schema test.
    #[doc(hidden)]
    pub const fn declared(db: Db, text: &'static str) -> Self {
        Self { db, text }
    }

    pub const fn db(&self) -> Db {
        self.db
    }

    pub const fn text(&self) -> &'static str {
        self.text
    }

    pub fn execute<P: Params>(&self, conn: &Connection, params: P) -> rusqlite::Result<usize> {
        conn.execute(self.text, params)
    }

    pub fn query_row<T, P, F>(&self, conn: &Connection, params: P, f: F) -> rusqlite::Result<T>
    where
        P: Params,
        F: FnOnce(&Row<'_>) -> rusqlite::Result<T>,
    {
        conn.query_row(self.text, params, f)
    }

    pub fn prepare<'c>(&self, conn: &'c Connection) -> rusqlite::Result<Statement<'c>> {
        conn.prepare(self.text)
    }
}

/// A write transaction, holding the write lock from its first statement.
///
/// ⚠ Not rusqlite's default (deferred): a transaction that reads first and
/// writes later cannot take the lock over a view another writer changed in
/// between, and fails at once ("database is locked") instead of waiting out
/// the busy timeout. Clippy refuses the default (`recalld/clippy.toml`).
pub fn write(conn: &mut Connection) -> rusqlite::Result<Transaction<'_>> {
    conn.transaction_with_behavior(TransactionBehavior::Immediate)
}

/// [`write`](fn@write) on a shared connection, for a caller holding only `&Connection`.
pub fn write_shared(conn: &Connection) -> rusqlite::Result<Transaction<'_>> {
    Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
}

/// Declare a module's statements, each with the database it runs against:
///
/// ```ignore
/// statements! {
///     /// Doc comments stay on the constant.
///     LOOKUP: Meaning = "SELECT id FROM speakers WHERE name = ?1";
/// }
/// ```
///
/// The text is `concat!`ed, so a shared fragment can be a literal-producing
/// macro ([`crate::human_owned!`]). The block also defines `STATEMENTS`, the
/// module's entry in [`ALL`].
#[macro_export]
macro_rules! statements {
    ($($(#[$meta:meta])* $vis:vis $name:ident: $db:ident = $($part:expr),+;)+) => {
        $(
            $(#[$meta])*
            $vis const $name: $crate::sql::Sql =
                $crate::sql::Sql::declared($crate::sql::Db::$db, concat!($($part),+));
        )+
        /// Every statement this module runs, for the schema test.
        pub const STATEMENTS: &[$crate::sql::Sql] = &[$($name),+];
    };
}

/// Every module's statements, by module name; the schema test also checks
/// that each module with a `statements!` block is here.
pub const ALL: &[(&str, &[Sql])] = &[
    ("assign", crate::assign::STATEMENTS),
    ("audio", crate::audio::STATEMENTS),
    ("capture", crate::capture::STATEMENTS),
    ("devices", crate::devices::STATEMENTS),
    ("diarized", crate::diarized::STATEMENTS),
    ("enrol", crate::enrol::STATEMENTS),
    ("identify", crate::identify::STATEMENTS),
    ("labels", crate::labels::STATEMENTS),
    ("labels_write", crate::labels_write::STATEMENTS),
    ("ledger", crate::ledger::STATEMENTS),
    ("live_tier", crate::live_tier::STATEMENTS),
    ("queue", crate::queue::STATEMENTS),
    ("reads", crate::reads::STATEMENTS),
    ("record_health", crate::record_health::STATEMENTS),
    ("rematch", crate::rematch::STATEMENTS),
    ("retranscribe", crate::retranscribe::STATEMENTS),
    ("sessions", crate::sessions::STATEMENTS),
    ("sources", crate::sources::STATEMENTS),
    ("speech", crate::speech::STATEMENTS),
    ("store", crate::store::STATEMENTS),
    ("turn_store", crate::turn_store::STATEMENTS),
    ("turns", crate::turns::STATEMENTS),
    ("upload", crate::upload::STATEMENTS),
    ("work", crate::work::STATEMENTS),
];
