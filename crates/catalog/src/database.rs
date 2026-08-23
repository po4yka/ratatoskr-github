use std::time::Duration;

use sqlx::Executor as _;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

const SCHEMA: &str = include_str!("../../../schema.sql");
// "ratatoskr" in hexadecimal prefixes the fleet's per-repository schema locks.
const SCHEMA_LOCK: i64 = 0x7261_7461_736b_7203;

/// Catalog persistence failure with no connection details.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PersistenceError {
    /// Database connection failed.
    #[error("the database connection could not be established")]
    Connect(#[source] sqlx::Error),
    /// Current schema application failed.
    #[error("the github_catalog schema could not be applied")]
    Schema(#[source] sqlx::Error),
    /// A catalog-owned query failed.
    #[error("a github_catalog database query failed")]
    Query(#[source] sqlx::Error),
}

/// One finite database pool owned by the GitHub Catalog.
#[derive(Debug, Clone)]
pub struct Database {
    pool: PgPool,
}

impl Database {
    /// Connects the finite pool.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Connect`] when the database is unavailable.
    pub async fn connect(
        url: &str,
        max_connections: u32,
        acquire_timeout: Duration,
    ) -> Result<Self, PersistenceError> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(acquire_timeout)
            .connect(url)
            .await
            .map_err(PersistenceError::Connect)?;
        Ok(Self { pool })
    }

    /// Applies the current editable schema definition.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Schema`] when the database refuses the schema.
    pub async fn apply_schema(&self) -> Result<(), PersistenceError> {
        let mut transaction = self.pool.begin().await.map_err(PersistenceError::Schema)?;
        sqlx::query("select pg_advisory_xact_lock($1)")
            .bind(SCHEMA_LOCK)
            .execute(&mut *transaction)
            .await
            .map_err(PersistenceError::Schema)?;
        transaction
            .execute(SCHEMA)
            .await
            .map_err(PersistenceError::Schema)?;
        transaction.commit().await.map_err(PersistenceError::Schema)
    }

    /// Returns the owned pool for catalog queries.
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Closes the finite pool.
    pub async fn close(&self) {
        self.pool.close().await;
    }

    #[cfg(feature = "test-support")]
    pub(crate) const fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }
}
