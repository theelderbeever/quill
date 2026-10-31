mod settings;
pub use settings::SqliteSettings;

use crate::ConnectionPool;
use quill_core::error::{Error, Result};
use sqlx::{ConnectOptions, SqlitePool, sqlite::SqliteConnectOptions};

/// Native Sqlite connection pool.
pub struct SqliteConnectionPool {
    pub(crate) inner: SqlitePool,
}

impl SqliteConnectionPool {
    /// Access the native pool for driver-specific operations.
    pub fn inner(&self) -> &SqlitePool {
        &self.inner
    }
}

impl ConnectionPool for SqliteConnectionPool {
    type Settings = SqliteSettings;

    async fn connect(settings: &Self::Settings) -> Result<Self> {
        let connect = async {
            if !settings.params.is_empty() {
                anyhow::bail!("SQLite parameters are not yet supported");
            }
            let options = SqliteConnectOptions::new().filename(&settings.path);
            let inner = SqlitePool::connect_with(options.disable_statement_logging()).await?;
            Ok::<_, anyhow::Error>(Self { inner })
        };
        connect.await.map_err(|source| Error::DriverConnect {
            connection: "sqlite".into(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn connects_and_shares_an_in_memory_pool() -> Result<()> {
        let settings = SqliteSettings {
            path: ":memory:".into(),
            params: Default::default(),
        };
        let pool = SqliteConnectionPool::connect(&settings).await?;
        let value: i64 = sqlx::query_scalar("SELECT 1")
            .fetch_one(pool.inner())
            .await
            .map_err(|error| Error::DriverExecute(error.into()))?;
        assert_eq!(value, 1);
        let shared: Arc<dyn ConnectionPool> = Arc::new(pool);
        assert_eq!(Arc::strong_count(&shared), 1);
        Ok(())
    }
}
