mod metadata;
use metadata::{ColumnRow, IndexKeyRow, IndexRow, NameRow, SchemaRow, TableRow};

mod settings;
pub use settings::SqliteSettings;

use crate::{ConnectionPool, PoolFuture};
use quill_core::{
    error::{Error, Result},
    metadata::*,
};
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

    /// SQLite has one connection-local catalog; attached databases are schemas.
    fn list_catalogs(&self) -> PoolFuture<'_, Vec<CatalogRef>> {
        Box::pin(async {
            Ok(vec![CatalogRef {
                name: "sqlite".into(),
            }])
        })
    }

    fn list_schemas<'a>(&'a self, catalog: &'a CatalogRef) -> PoolFuture<'a, Vec<SchemaRef>> {
        Box::pin(async move {
            if catalog.name != "sqlite" {
                return Ok(Vec::new());
            }
            let names: Vec<NameRow> =
                sqlx::query_as("SELECT name FROM pragma_database_list ORDER BY name")
                    .fetch_all(&self.inner)
                    .await
                    .map_err(|e| Error::DriverExecute(e.into()))?;
            Ok(names
                .into_iter()
                .map(|row| SchemaRef::from(SchemaRow { row, catalog }))
                .collect())
        })
    }

    fn list_tables<'a>(&'a self, schema: &'a SchemaRef) -> PoolFuture<'a, Vec<TableRef>> {
        Box::pin(async move {
            if schema.catalog.name != "sqlite" {
                return Ok(Vec::new());
            }
            let names: Vec<NameRow> = sqlx::query_as("SELECT name FROM pragma_table_list WHERE schema = ? AND type IN ('table', 'view', 'virtual') AND name NOT LIKE 'sqlite_%' ORDER BY name")
                .bind(&schema.name).fetch_all(&self.inner).await.map_err(|e| Error::DriverExecute(e.into()))?;
            Ok(names
                .into_iter()
                .map(|row| TableRef::from(TableRow { row, schema }))
                .collect())
        })
    }

    fn describe_table<'a>(&'a self, table: &'a TableRef) -> PoolFuture<'a, TableMetadata> {
        Box::pin(async move {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_list WHERE schema = ? AND name = ?)",
            )
            .bind(&table.schema.name)
            .bind(&table.name)
            .fetch_one(&self.inner)
            .await
            .map_err(|e| Error::DriverExecute(e.into()))?;
            if table.schema.catalog.name != "sqlite" || !exists {
                return Err(Error::DriverExecute(sqlx::Error::RowNotFound.into()));
            }
            let rows: Vec<ColumnRow> = sqlx::query_as("SELECT name, type AS native_type, \"notnull\" AS not_null, pk, (SELECT strict OR wr FROM pragma_table_list WHERE schema = ?2 AND name = ?1) AS strict FROM pragma_table_xinfo(?1, ?2) WHERE hidden != 1 ORDER BY cid")
                .bind(&table.name).bind(&table.schema.name).fetch_all(&self.inner).await.map_err(|e| Error::DriverExecute(e.into()))?;
            Ok(TableMetadata {
                table: table.clone(),
                columns: rows.into_iter().map(ColumnMetadata::from).collect(),
            })
        })
    }

    fn list_indexes<'a>(&'a self, table: &'a TableRef) -> PoolFuture<'a, Vec<IndexMetadata>> {
        Box::pin(async move {
            self.describe_table(table).await?;
            let rows: Vec<IndexRow> = sqlx::query_as("SELECT name, \"unique\" AS \"unique\", origin, partial FROM pragma_index_list(?, ?) ORDER BY name")
                .bind(&table.name).bind(&table.schema.name).fetch_all(&self.inner).await.map_err(|e| Error::DriverExecute(e.into()))?;
            let mut indexes = Vec::new();
            for row in rows {
                let name = &row.name;
                let partial = row.partial;
                let keys: Vec<IndexKeyRow> = sqlx::query_as("SELECT cid, name AS column_name, desc AS descending, coll AS collation FROM pragma_index_xinfo(?, ?) WHERE key = 1 ORDER BY seqno")
                    .bind(name).bind(&table.schema.name).fetch_all(&self.inner).await.map_err(|e| Error::DriverExecute(e.into()))?;
                // PRAGMAs omit expression SQL and predicates; do not invent their text.
                if partial || keys.iter().any(|key| key.cid < 0) {
                    return Err(Error::UnsupportedOperation(
                        "SQLite expression or partial index descriptions",
                    ));
                }
                let keys = keys
                    .into_iter()
                    .map(IndexKey::try_from)
                    .collect::<Result<Vec<_>>>()?;
                let mut index = IndexMetadata::from(row);
                index.keys = keys;
                indexes.push(index);
            }
            Ok(indexes)
        })
    }

    async fn connect(settings: &Self::Settings) -> Result<Self> {
        let connect = async {
            if !settings.params.is_empty() {
                anyhow::bail!("SQLite parameters are not yet supported");
            }
            let options = SqliteConnectOptions::new().filename(&settings.path);
            // Attached databases and in-memory state belong to a connection.
            let inner = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(options.disable_statement_logging())
                .await?;
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
    async fn discovers_columns_and_indexes() -> anyhow::Result<()> {
        let pool = SqliteConnectionPool::connect(&SqliteSettings {
            path: ":memory:".into(),
            params: Default::default(),
        })
        .await?;
        sqlx::query("CREATE TABLE items (id INTEGER PRIMARY KEY, title TEXT NOT NULL)")
            .execute(pool.inner())
            .await?;
        sqlx::query("CREATE UNIQUE INDEX title_idx ON items(title DESC)")
            .execute(pool.inner())
            .await?;
        let pool: Arc<dyn ConnectionPool> = Arc::new(pool);
        let catalog = pool.list_catalogs().await?.remove(0);
        let schema = pool
            .list_schemas(&catalog)
            .await?
            .into_iter()
            .find(|s| s.name == "main")
            .ok_or_else(|| anyhow::anyhow!("missing main"))?;
        let table = pool.list_tables(&schema).await?.remove(0);
        let description = pool.describe_table(&table).await?;
        assert_eq!(description.columns.len(), 2);
        assert_eq!(description.columns[1].nullable, Some(false));
        let indexes = pool.list_indexes(&table).await?;
        assert_eq!(indexes.len(), 1);
        assert!(indexes[0].unique);
        assert!(indexes[0].keys[0].ordering.is_some_and(|o| o.descending));
        Ok(())
    }

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
