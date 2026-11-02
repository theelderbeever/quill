mod settings;
pub use settings::SqliteSettings;

use std::sync::Arc;

use datafusion::{catalog::TableProvider, common::TableReference};
use datafusion_table_providers::{
    sql::db_connection_pool::{Mode, sqlitepool},
    sqlite::SqliteTableFactory,
};
use quill_core::error::{Error, Result};
use tokio_rusqlite::{Connection, ToSql};

use crate::{
    PoolFuture,
    provider::{ProviderPool, Tables, factory_table},
};

pub type SqliteConnectionPool = ProviderPool<Connection, &'static (dyn ToSql + Sync)>;

/// SQLite has one connection-local catalog.
const CATALOG: &str = "sqlite";

/// SQLite's name for a private in-memory database.
const MEMORY: &str = ":memory:";

impl crate::Connect for SqliteConnectionPool {
    type Settings = SqliteSettings;

    async fn connect(settings: &Self::Settings) -> Result<Self> {
        let connect = async {
            anyhow::ensure!(
                settings.params.is_empty(),
                "SQLite parameters are not yet supported"
            );
            let path = settings
                .path
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("SQLite path must be valid UTF-8"))?;
            let mode = if path == MEMORY {
                Mode::Memory
            } else {
                Mode::File
            };
            let pool =
                sqlitepool::SqliteConnectionPoolFactory::new(path, mode, settings.busy_timeout())
                    .with_databases(Some(
                        settings
                            .databases
                            .iter()
                            .map(|path| Arc::from(path.as_str()))
                            .collect(),
                    ))
                    .build()
                    .await
                    .map_err(|e| anyhow::anyhow!(e))?;
            Ok::<_, anyhow::Error>(pool)
        };
        let pool = connect.await.map_err(|source| Error::DriverConnect {
            connection: "sqlite".into(),
            source,
        })?;
        let pool = Arc::new(pool);
        let tables = Arc::new(SqliteTableFactory::new(pool.clone()));
        Ok(Self::new(pool, tables, CATALOG))
    }
}

impl Tables for SqliteTableFactory {
    fn capabilities(&self) -> crate::Capabilities {
        crate::Capabilities {
            named_parameters: true,
            positional_parameters: true,
        }
    }
    fn table(&self, table: TableReference) -> PoolFuture<'_, Arc<dyn TableProvider>> {
        Box::pin(async move { factory_table(self.table_provider(table).await) })
    }
}
