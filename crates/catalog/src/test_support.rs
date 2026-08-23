//! Disposable database support for integration tests.

use sqlx::Executor as _;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use uuid::Uuid;

use crate::{Database, PersistenceError};

/// An isolated disposable catalog database.
#[derive(Debug)]
pub struct TestDatabase {
    /// Connected catalog database.
    pub database: Database,
    name: String,
}

impl TestDatabase {
    /// Creates an empty isolated database from the current schema definition.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError`] when database creation or connection fails.
    pub async fn create() -> Result<Self, PersistenceError> {
        let name = format!("github_catalog_test_{}", Uuid::now_v7().simple());
        let admin_url = admin_url();
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .map_err(PersistenceError::Connect)?;
        admin
            .execute(format!(r#"create database "{name}""#).as_str())
            .await
            .map_err(PersistenceError::Query)?;
        admin.close().await;

        let options = admin_url
            .parse::<PgConnectOptions>()
            .map_err(PersistenceError::Connect)?
            .database(&name);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .map_err(PersistenceError::Connect)?;
        let database = Database::from_pool(pool);
        database.apply_schema().await?;
        Ok(Self { database, name })
    }

    /// Closes and drops the database.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError`] when cleanup fails.
    pub async fn cleanup(self) -> Result<(), PersistenceError> {
        self.database.close().await;
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url())
            .await
            .map_err(PersistenceError::Connect)?;
        admin
            .execute(format!(r#"drop database if exists "{}" with (force)"#, self.name).as_str())
            .await
            .map_err(PersistenceError::Query)?;
        admin.close().await;
        Ok(())
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "test-only database location is not process configuration"
)]
fn admin_url() -> String {
    match std::env::var("GITHUB_CATALOG_TEST_DATABASE_URL") {
        Ok(value) => value,
        Err(_) => "postgres://github:github@127.0.0.1:5435/github".to_owned(),
    }
}
