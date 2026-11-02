//! One [`ConnectionPool`] over any `datafusion-table-providers` pool.
use std::sync::Arc;

use datafusion::{
    catalog::TableProvider, common::TableReference, execution::SendableRecordBatchStream,
};
use datafusion_table_providers::sql::db_connection_pool::{
    DbConnectionPool,
    dbconnection::{self, DbConnection},
};
use futures::TryStreamExt;
use quill_core::{
    error::{Error, Result},
    metadata::{CatalogRef, ColumnMetadata, SchemaRef, TableMetadata, TableRef},
    query::{QueryEvent, QueryExecution, QueryRequest},
};

use crate::{ConnectionPool, PoolFuture};

/// Shared handle to an upstream provider's pool.
pub type UpstreamPool<T, P> = Arc<dyn DbConnectionPool<T, P> + Send + Sync>;

/// Error type returned by upstream table factories.
pub type FactoryError = Box<dyn std::error::Error + Send + Sync>;

/// Builds federation-ready DataFusion tables over an upstream pool. Each
/// provider's table factory implements it.
pub trait Tables: Send + Sync {
    fn table(&self, table: TableReference) -> PoolFuture<'_, Arc<dyn TableProvider>>;
}

/// Adapt an upstream factory's result to quill's error type.
pub(crate) fn factory_table(
    result: std::result::Result<Arc<dyn TableProvider>, FactoryError>,
) -> Result<Arc<dyn TableProvider>> {
    result.map_err(|e| Error::DriverExecute(anyhow::anyhow!(e)))
}

/// Native-dialect execution and catalog discovery through a provider pool.
///
/// SQL is sent unchanged to the upstream; DataFusion does not plan it. The
/// provider reports one catalog — the connected database — because its
/// connections cannot inspect others.
pub struct ProviderPool<T: 'static, P: 'static> {
    pool: UpstreamPool<T, P>,
    tables: Arc<dyn Tables>,
    catalog: CatalogRef,
}

impl<T: 'static, P: 'static> ProviderPool<T, P> {
    pub fn new(
        pool: UpstreamPool<T, P>,
        tables: Arc<dyn Tables>,
        catalog: impl Into<String>,
    ) -> Self {
        Self {
            pool,
            tables,
            catalog: CatalogRef {
                name: catalog.into(),
            },
        }
    }

    /// The upstream pool, for registering federated table providers.
    pub fn upstream(&self) -> &UpstreamPool<T, P> {
        &self.pool
    }

    async fn connection(&self) -> Result<Box<dyn DbConnection<T, P>>> {
        self.pool
            .connect()
            .await
            .map_err(|e| Error::DriverExecute(anyhow::anyhow!(e)))
    }

    async fn schemas(&self) -> Result<Vec<String>> {
        let conn = self.connection().await?;
        let names = if let Some(conn) = conn.as_sync() {
            conn.schemas()
        } else if let Some(conn) = conn.as_async() {
            conn.schemas().await
        } else {
            Err(dbconnection::Error::UnableToDowncastConnection {})
        };
        names.map_err(driver)
    }

    async fn tables(&self, schema: &str) -> Result<Vec<String>> {
        let conn = self.connection().await?;
        let names = if let Some(conn) = conn.as_sync() {
            conn.tables(schema)
        } else if let Some(conn) = conn.as_async() {
            conn.tables(schema).await
        } else {
            Err(dbconnection::Error::UnableToDowncastConnection {})
        };
        names.map_err(driver)
    }
}

/// Providers await the first rows before returning a stream, so this runs
/// inside the execution task where cancellation can reach it.
/// Boxed so the future's `Send` bound is fixed to `'static`.
fn stream<T: 'static, P: 'static>(
    pool: UpstreamPool<T, P>,
    sql: String,
) -> PoolFuture<'static, SendableRecordBatchStream> {
    Box::pin(async move {
        let conn = pool
            .connect()
            .await
            .map_err(|e| Error::DriverExecute(anyhow::anyhow!(e)))?;
        dbconnection::query_arrow(conn, sql, None)
            .await
            .map_err(|e| Error::QueryExecution(Box::new(e)))
    })
}

fn driver(error: dbconnection::Error) -> Error {
    Error::DriverExecute(error.into())
}

impl<T: 'static, P: 'static> ConnectionPool for ProviderPool<T, P>
where
    Self: Send + Sync,
{
    fn execute(&self, request: QueryRequest) -> PoolFuture<'_, QueryExecution> {
        Box::pin(async move {
            if !request.bindings.named.is_empty() || !request.bindings.positional.is_empty() {
                return Err(Error::UnsupportedOperation("query parameters"));
            }
            let pool = self.pool.clone();
            Ok(crate::execution::start(move |sender, control| async move {
                let mut stream = stream(pool, request.sql).await?;
                // Commands return no columns; they report no result schema.
                let schema = stream.schema();
                if !schema.fields().is_empty() {
                    let event = QueryEvent::Schema {
                        result_set: 0,
                        schema,
                    };
                    control.send(&sender, event).await?;
                }
                while let Some(batch) = stream
                    .try_next()
                    .await
                    .map_err(|e| Error::QueryExecution(Box::new(e)))?
                {
                    let event = QueryEvent::Batch {
                        result_set: 0,
                        batch,
                    };
                    control.send(&sender, event).await?;
                }
                let done = QueryEvent::ResultSetComplete {
                    result_set: 0,
                    affected_rows: None,
                };
                control.send(&sender, done).await
            }))
        })
    }

    fn list_catalogs(&self) -> PoolFuture<'_, Vec<CatalogRef>> {
        Box::pin(async { Ok(vec![self.catalog.clone()]) })
    }

    fn list_schemas<'a>(&'a self, catalog: &'a CatalogRef) -> PoolFuture<'a, Vec<SchemaRef>> {
        Box::pin(async move {
            if *catalog != self.catalog {
                return Ok(Vec::new());
            }
            let mut names = self.schemas().await?;
            names.sort_unstable();
            Ok(names
                .into_iter()
                .map(|name| SchemaRef {
                    catalog: catalog.clone(),
                    name,
                })
                .collect())
        })
    }

    fn list_tables<'a>(&'a self, schema: &'a SchemaRef) -> PoolFuture<'a, Vec<TableRef>> {
        Box::pin(async move {
            let mut names = self.tables(&schema.name).await?;
            names.sort_unstable();
            Ok(names
                .into_iter()
                .map(|name| TableRef {
                    schema: schema.clone(),
                    name,
                })
                .collect())
        })
    }

    fn table_provider<'a>(&'a self, table: &'a TableRef) -> PoolFuture<'a, Arc<dyn TableProvider>> {
        let reference = TableReference::partial(table.schema.name.as_str(), table.name.as_str());
        self.tables.table(reference)
    }

    /// Columns as the provider maps them to Arrow; `native_type` is the Arrow
    /// type unless the provider annotates the source type.
    fn describe_table<'a>(&'a self, table: &'a TableRef) -> PoolFuture<'a, TableMetadata> {
        Box::pin(async move {
            let reference =
                TableReference::partial(table.schema.name.as_str(), table.name.as_str());
            let schema = dbconnection::get_schema(self.connection().await?, &reference)
                .await
                .map_err(driver)?;
            let columns = schema
                .fields()
                .iter()
                .map(|field| ColumnMetadata {
                    name: field.name().clone(),
                    native_type: field
                        .metadata()
                        .get(datafusion_table_providers::SOURCE_TYPE_METADATA_KEY)
                        .cloned()
                        .unwrap_or_else(|| field.data_type().to_string()),
                    nullable: Some(field.is_nullable()),
                })
                .collect();
            Ok(TableMetadata {
                table: table.clone(),
                columns,
            })
        })
    }
}
