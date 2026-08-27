//! Fixed, read-only projection of the retired source schema.

use std::time::Duration;

use serde_json::Value;
use sqlx::postgres::{PgPoolOptions, PgRow};
use sqlx::{FromRow, Row as _};
use time::OffsetDateTime;

use super::{LegacyIntegration, LegacyRepository, LegacySnapshot, LegacySource, LegacySourceError};

impl LegacySource {
    /// Connects one bounded, temporary read source without applying a schema.
    ///
    /// # Errors
    ///
    /// Returns [`LegacySourceError::Connect`] when the isolated source cannot
    /// be reached.
    pub async fn connect(
        url: &str,
        max_connections: u32,
        acquire_timeout: Duration,
    ) -> Result<Self, LegacySourceError> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(acquire_timeout)
            .after_connect(|connection, _metadata| {
                Box::pin(async move {
                    sqlx::query("set default_transaction_read_only = on")
                        .execute(&mut *connection)
                        .await
                        .map(|_result| ())
                })
            })
            .connect(url)
            .await
            .map_err(LegacySourceError::Connect)?;
        Ok(Self { pool })
    }

    /// Creates a source from an operator-provided temporary pool.
    #[must_use]
    pub fn from_pool(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }

    /// Closes the temporary source pool after one bounded operator command.
    pub async fn close(&self) {
        self.pool.close().await;
    }

    /// Validates the exact tables, columns, and compatible types fixed queries read.
    ///
    /// # Errors
    ///
    /// Returns [`LegacySourceError`] when the isolated source is not the reviewed archive schema.
    pub async fn preflight(&self) -> Result<(), LegacySourceError> {
        ensure_columns(
            &self.pool,
            "repositories",
            &[
                RequiredColumn::new("id", &["integer", "bigint"]),
                RequiredColumn::new("github_id", &["bigint"]),
                RequiredColumn::new("owner", &["character varying", "text"]),
                RequiredColumn::new("name", &["character varying", "text"]),
                RequiredColumn::new("user_id", &["integer", "bigint"]),
                RequiredColumn::new("is_starred", &["boolean"]),
                RequiredColumn::new("last_synced_at", &["timestamp with time zone"]),
                RequiredColumn::new("list_names", &["jsonb"]),
            ],
        )
        .await?;
        ensure_columns(
            &self.pool,
            "user_github_integrations",
            &[
                RequiredColumn::new("id", &["integer", "bigint"]),
                RequiredColumn::new("user_id", &["integer", "bigint"]),
                RequiredColumn::new("token_scopes", &["character varying", "text"]),
                RequiredColumn::new("github_login", &["character varying", "text"]),
                RequiredColumn::new("github_user_id", &["integer", "bigint"]),
                RequiredColumn::new("status", &["USER-DEFINED", "character varying", "text"]),
            ],
        )
        .await
    }

    /// Reads the reviewed, non-secret legacy projection using fixed queries.
    ///
    /// # Errors
    ///
    /// Returns [`LegacySourceError`] when preflight or a fixed query fails.
    pub async fn read_snapshot(&self) -> Result<LegacySnapshot, LegacySourceError> {
        self.preflight().await?;
        let repository_rows: Vec<LegacyRepositoryRow> = sqlx::query_as(
            "select id::bigint as legacy_repository_id,
                    github_id as provider_repository_id,
                    owner,
                    name,
                    user_id::bigint as legacy_user_id,
                    is_starred as starred,
                    last_synced_at,
                    list_names
             from public.repositories
             order by id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(LegacySourceError::Query)?;
        let integration_rows: Vec<LegacyIntegrationRow> = sqlx::query_as(
            "select user_id::bigint as legacy_user_id,
                    token_scopes as granted_scopes,
                    github_login as login,
                    github_user_id as provider_user_id,
                    status::text as status
             from public.user_github_integrations
             order by user_id, id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(LegacySourceError::Query)?;
        let repositories = repository_rows
            .into_iter()
            .map(LegacyRepository::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(LegacySnapshot {
            repositories,
            integrations: integration_rows.into_iter().map(Into::into).collect(),
        })
    }
}

struct LegacyRepositoryRow {
    legacy_repository_id: i64,
    provider_repository_id: i64,
    owner: String,
    name: String,
    legacy_user_id: i64,
    starred: bool,
    last_synced_at: Option<OffsetDateTime>,
    list_names: Value,
}

impl<'row> FromRow<'row, PgRow> for LegacyRepositoryRow {
    fn from_row(row: &'row PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            legacy_repository_id: row.try_get("legacy_repository_id")?,
            provider_repository_id: row.try_get("provider_repository_id")?,
            owner: row.try_get("owner")?,
            name: row.try_get("name")?,
            legacy_user_id: row.try_get("legacy_user_id")?,
            starred: row.try_get("starred")?,
            last_synced_at: row.try_get("last_synced_at")?,
            list_names: row.try_get("list_names")?,
        })
    }
}

impl TryFrom<LegacyRepositoryRow> for LegacyRepository {
    type Error = LegacySourceError;

    fn try_from(row: LegacyRepositoryRow) -> Result<Self, Self::Error> {
        let list_names = row
            .list_names
            .as_array()
            .ok_or(LegacySourceError::InvalidData)?
            .iter()
            .map(|value| value.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
            .ok_or(LegacySourceError::InvalidData)?;
        Ok(Self {
            legacy_repository_id: row.legacy_repository_id,
            provider_repository_id: row.provider_repository_id,
            owner: row.owner,
            name: row.name,
            legacy_user_id: row.legacy_user_id,
            starred: row.starred,
            observed_at: row.last_synced_at,
            list_names,
        })
    }
}

struct LegacyIntegrationRow {
    legacy_user_id: i64,
    granted_scopes: Option<String>,
    login: Option<String>,
    provider_user_id: Option<i64>,
    status: String,
}

impl<'row> FromRow<'row, PgRow> for LegacyIntegrationRow {
    fn from_row(row: &'row PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            legacy_user_id: row.try_get("legacy_user_id")?,
            granted_scopes: row.try_get("granted_scopes")?,
            login: row.try_get("login")?,
            provider_user_id: row.try_get("provider_user_id")?,
            status: row.try_get("status")?,
        })
    }
}

impl From<LegacyIntegrationRow> for LegacyIntegration {
    fn from(row: LegacyIntegrationRow) -> Self {
        Self {
            legacy_user_id: row.legacy_user_id,
            granted_scopes: row.granted_scopes,
            login: row.login,
            provider_user_id: row.provider_user_id,
            status: row.status,
        }
    }
}

async fn ensure_columns(
    pool: &sqlx::PgPool,
    table: &str,
    required: &[RequiredColumn],
) -> Result<(), LegacySourceError> {
    let actual: Vec<(String, String)> = sqlx::query_as(
        "select column_name, data_type
         from information_schema.columns
         where table_schema = 'public' and table_name = $1",
    )
    .bind(table)
    .fetch_all(pool)
    .await
    .map_err(LegacySourceError::Query)?;
    if required.iter().any(|required_column| {
        !actual.iter().any(|(actual_name, actual_type)| {
            actual_name == required_column.name
                && required_column
                    .accepted_types
                    .contains(&actual_type.as_str())
        })
    }) {
        return Err(LegacySourceError::Schema);
    }
    Ok(())
}

struct RequiredColumn {
    name: &'static str,
    accepted_types: &'static [&'static str],
}

impl RequiredColumn {
    const fn new(name: &'static str, accepted_types: &'static [&'static str]) -> Self {
        Self {
            name,
            accepted_types,
        }
    }
}
