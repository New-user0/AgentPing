//! SQLite persistence for AgentPing: events, sessions, the durable notification queue and
//! delivery history.
//!
//! The database runs in WAL mode with a busy timeout so short-lived hook processes, the CLI
//! and the daemon can safely share it. All multi-statement updates run in transactions.

mod models;
mod queue;
mod repo;

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, TransactionBehavior};

pub use models::*;

/// Errors produced by the storage layer.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// SQLite reported an error.
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Stored data could not be decoded.
    #[error("corrupt record: {0}")]
    Corrupt(String),
    /// The database schema is newer than this build.
    #[error("database schema version {found} is newer than this build supports ({supported}); please upgrade AgentPing")]
    SchemaTooNew {
        /// Version found.
        found: u32,
        /// Newest supported version.
        supported: u32,
    },
    /// Filesystem error.
    #[error("{0}")]
    Io(String),
}

/// Result alias for storage operations.
pub type Result<T, E = StorageError> = std::result::Result<T, E>;

const MIGRATIONS: &[&str] = &[include_str!("migrations/001_init.sql")];

/// Newest schema version known to this build.
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

/// An open database connection.
pub struct Database {
    conn: Connection,
}

/// Repository operations over a connection or an open transaction.
pub struct Store<'c> {
    conn: &'c Connection,
}

impl Database {
    /// Opens (creating if needed) and migrates the database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| StorageError::Io(format!("{}: {e}", parent.display())))?;
        }
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let _ = agentping_core::fsutil::restrict_file_permissions(path);
        Self::init(conn)
    }

    /// Opens a private in-memory database (tests and dry runs).
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let mut db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&mut self) -> Result<()> {
        let current: u32 = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        if current > SCHEMA_VERSION {
            return Err(StorageError::SchemaTooNew {
                found: current,
                supported: SCHEMA_VERSION,
            });
        }
        for (index, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
            let version = index as u32 + 1;
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Exclusive)?;
            // Another process may have migrated while we waited for the lock.
            let now: u32 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
            if now >= version {
                tx.commit()?;
                continue;
            }
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", version)?;
            tx.commit()?;
            tracing::info!(version, "applied database migration");
        }
        Ok(())
    }

    /// Current schema version.
    pub fn schema_version(&self) -> Result<u32> {
        Ok(self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?)
    }

    /// Read access (each statement runs in its own implicit transaction).
    pub fn store(&self) -> Store<'_> {
        Store { conn: &self.conn }
    }

    /// Runs `f` inside an immediate (write-locking) transaction and commits on success.
    pub fn write<T>(&mut self, f: impl FnOnce(&Store<'_>) -> Result<T>) -> Result<T> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let out = f(&Store { conn: &tx })?;
        tx.commit()?;
        Ok(out)
    }

    /// Runs SQLite's quick integrity check. Returns `Ok(None)` when healthy.
    pub fn quick_check(&self) -> Result<Option<String>> {
        let result: String = self
            .conn
            .pragma_query_value(None, "quick_check", |r| r.get(0))?;
        Ok((result != "ok").then_some(result))
    }

    /// Reclaims free pages after large deletions.
    pub fn vacuum(&self) -> Result<()> {
        self.conn.execute_batch("VACUUM")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
