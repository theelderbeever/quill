//! One [`ConnectionPool`] over any `datafusion-table-providers` pool.
use std::sync::Arc;

use datafusion::{catalog::TableProvider, common::TableReference, scalar::ScalarValue};
use datafusion_table_providers::sql::db_connection_pool::{
    DbConnectionPool,
    dbconnection::{self, DbConnection},
    indexes::IndexDiscovery,
    query::{NativeQueryExecutor, QueryOptions, QueryParams},
};
use futures::TryStreamExt;
use quill_core::{
    error::{Error, Result},
    metadata::{CatalogRef, ColumnMetadata, SchemaRef, TableMetadata, TableRef},
    query::{ParameterValue, QueryEvent, QueryExecution, QueryId, QueryRequest},
};

use crate::{ConnectionPool, PoolFuture};

/// Shared handle to an upstream provider's pool.
pub type UpstreamPool<T, P> = Arc<dyn DbConnectionPool<T, P> + Send + Sync>;

/// Error type returned by upstream table factories.
pub type FactoryError = Box<dyn std::error::Error + Send + Sync>;

/// Builds federation-ready DataFusion tables over an upstream pool. Each
/// provider's table factory implements it.
pub trait Tables: Send + Sync {
    fn capabilities(&self) -> crate::Capabilities;
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
    executor: Arc<dyn NativeQueryExecutor>,
    indexes: Arc<dyn IndexDiscovery>,
    catalog: CatalogRef,
}

impl<T: 'static, P: 'static> ProviderPool<T, P> {
    pub fn new<U>(pool: Arc<U>, tables: Arc<dyn Tables>, catalog: impl Into<String>) -> Self
    where
        U: DbConnectionPool<T, P> + NativeQueryExecutor + IndexDiscovery + Send + Sync + 'static,
    {
        Self {
            executor: pool.clone(),
            indexes: pool.clone(),
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

fn driver(error: dbconnection::Error) -> Error {
    Error::DriverExecute(error.into())
}

impl<T: 'static, P: 'static> ConnectionPool for ProviderPool<T, P>
where
    Self: Send + Sync,
{
    fn capabilities(&self) -> crate::Capabilities {
        self.tables.capabilities()
    }

    fn execute(&self, request: QueryRequest) -> PoolFuture<'_, QueryExecution> {
        Box::pin(async move {
            let executor = self.executor.clone();
            let id = QueryId::new();
            Ok(crate::execution::start_with_id(
                id,
                move |sender, control| async move {
                    let params = QueryParams {
                        positional: request
                            .bindings
                            .positional
                            .into_iter()
                            .map(parameter_scalar)
                            .collect(),
                        named: request
                            .bindings
                            .named
                            .into_iter()
                            .map(|(k, v)| (k, parameter_scalar(v)))
                            .collect(),
                    };
                    let options = QueryOptions::new().with_query_id(id.to_string());
                    let mut stream = executor
                        .query(&request.sql, &params, &options)
                        .await
                        .map_err(Error::QueryExecution)?;
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
                },
            ))
        })
    }

    fn list_indexes<'a>(
        &'a self,
        table: &'a TableRef,
    ) -> PoolFuture<'a, Vec<quill_core::metadata::IndexMetadata>> {
        Box::pin(async move {
            use quill_core::metadata::{IndexKey, IndexKeySource, IndexMetadata};
            let reference =
                TableReference::partial(table.schema.name.as_str(), table.name.as_str());
            let indexes = self
                .indexes
                .indexes(&reference)
                .await
                .map_err(Error::QueryExecution)?;
            Ok(indexes
                .into_iter()
                .map(|index| IndexMetadata {
                    name: index.name,
                    access_method: index.kind.unwrap_or_default(),
                    unique: index.unique,
                    primary: index.primary,
                    valid: None,
                    keys: index
                        .columns
                        .into_iter()
                        .map(|column| IndexKey {
                            definition: column.clone(),
                            source: IndexKeySource::Expression(column),
                            ordering: None,
                        })
                        .collect(),
                    included_columns: Vec::new(),
                    predicate: None,
                })
                .collect())
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

fn parameter_scalar(value: ParameterValue) -> ScalarValue {
    match value {
        ParameterValue::Null => ScalarValue::Null,
        ParameterValue::Boolean(v) => ScalarValue::Boolean(Some(v)),
        ParameterValue::Int64(v) => ScalarValue::Int64(Some(v)),
        ParameterValue::UInt64(v) => ScalarValue::UInt64(Some(v)),
        ParameterValue::Float64(v) => ScalarValue::Float64(Some(v)),
        ParameterValue::Text(v) => ScalarValue::Utf8(Some(v)),
        ParameterValue::Binary(v) => ScalarValue::Binary(Some(v)),
        ParameterValue::Date(v) => ScalarValue::Date32(Some(
            (v - chrono::DateTime::UNIX_EPOCH.date_naive()).num_days() as i32,
        )),
        ParameterValue::Time(v) => {
            use chrono::Timelike;
            ScalarValue::Time64Nanosecond(Some(
                i64::from(v.num_seconds_from_midnight()) * 1_000_000_000
                    + i64::from(v.nanosecond()),
            ))
        }
        ParameterValue::Timestamp(v) => {
            ScalarValue::TimestampMicrosecond(Some(v.and_utc().timestamp_micros()), None)
        }
        ParameterValue::TimestampWithTimezone(v) => ScalarValue::TimestampMicrosecond(
            Some(v.timestamp_micros()),
            Some(v.offset().to_string().into()),
        ),
        ParameterValue::Uuid(v) => ScalarValue::Utf8(Some(v.to_string())),
    }
}
