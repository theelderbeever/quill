use std::{future::Future, pin::Pin};

use quill_core::{
    error::{Error, Result},
    metadata::{CatalogRef, IndexMetadata, SchemaRef, TableMetadata, TableRef},
    query::{QueryExecution, QueryRequest},
};

/// A borrowed, sendable future usable through dynamic dispatch.
pub type PoolFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Parameter support currently implemented by a driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub named_parameters: bool,
    pub positional_parameters: bool,
}

/// A shared, thread-safe connection pool.
///
/// Construction uses concrete driver settings
///
/// ```
/// use std::sync::Arc;
/// use quill_drivers::ConnectionPool;
///
/// fn share<P: ConnectionPool + 'static>(pool: P) -> Arc<dyn ConnectionPool> {
///     Arc::new(pool)
/// }
/// ```
pub trait ConnectionPool: Send + Sync {
    /// Report implemented parameter support independently for each binding style.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            named_parameters: false,
            positional_parameters: false,
        }
    }

    /// Begin executing native SQL without rewriting its placeholders.
    ///
    /// The returned execution owns work through the end of streaming. Drivers
    /// start timing and spawn query work before returning it; submission errors
    /// after that point are reported through the event stream.
    ///
    /// ```no_run
    /// use quill_core::{error::Result, query::{QueryRequest, ParameterValue}};
    /// use quill_drivers::ConnectionPool;
    /// async fn execute(pool: &dyn ConnectionPool) -> Result<()> {
    ///     let mut request = QueryRequest::new("SELECT $1");
    ///     request.bindings.positional.push(ParameterValue::Int64(42));
    ///     let execution = pool.execute(request).await?;
    ///     let snapshot = execution.control.snapshot();
    ///     Ok(())
    /// }
    /// ```
    fn execute(&self, _request: QueryRequest) -> PoolFuture<'_, QueryExecution> {
        Box::pin(async { Err(Error::UnsupportedOperation("execute query")) })
    }

    /// Retrieve optional server measurements for this execution only.
    /// Call again later if asynchronous server logs have not appeared yet.
    /// Local timing and counters remain available through execution control,
    /// including when a driver has no server measurements.
    ///
    /// ```no_run
    /// # async fn inspect(pool: &dyn quill_drivers::ConnectionPool, id: quill_core::query::QueryId) -> quill_core::error::Result<()> {
    /// let metrics = pool.execution_metrics(id).await?;
    /// # Ok(())
    /// # }
    /// ```
    fn execution_metrics(
        &self,
        _id: quill_core::query::QueryId,
    ) -> PoolFuture<'_, Option<quill_core::query::ServerMetrics>> {
        Box::pin(async { Ok(None) })
    }

    /// List catalogs visible to this connection.
    fn list_catalogs(&self) -> PoolFuture<'_, Vec<CatalogRef>> {
        Box::pin(async { Err(Error::UnsupportedOperation("list catalogs")) })
    }

    /// List schemas within the supplied catalog.
    fn list_schemas<'a>(&'a self, _catalog: &'a CatalogRef) -> PoolFuture<'a, Vec<SchemaRef>> {
        Box::pin(async { Err(Error::UnsupportedOperation("list schemas")) })
    }

    /// List tables within the supplied schema.
    fn list_tables<'a>(&'a self, _schema: &'a SchemaRef) -> PoolFuture<'a, Vec<TableRef>> {
        Box::pin(async { Err(Error::UnsupportedOperation("list tables")) })
    }

    /// List indexes on a table. An empty list means the table has no indexes.
    /// Drivers without index discovery return `UnsupportedOperation`.
    ///
    /// ```no_run
    /// use quill_drivers::ConnectionPool;
    /// use quill_core::{error::Result, metadata::TableRef};
    /// async fn inspect(pool: &dyn ConnectionPool, table: &TableRef) -> Result<()> {
    ///     for index in pool.list_indexes(table).await? {
    ///         assert!(!index.name.is_empty());
    ///     }
    ///     Ok(())
    /// }
    /// ```
    fn list_indexes<'a>(&'a self, _table: &'a TableRef) -> PoolFuture<'a, Vec<IndexMetadata>> {
        Box::pin(async { Err(Error::UnsupportedOperation("list indexes")) })
    }

    /// Describe a table without interpreting its names as SQL text.
    fn describe_table<'a>(&'a self, _table: &'a TableRef) -> PoolFuture<'a, TableMetadata> {
        Box::pin(async { Err(Error::UnsupportedOperation("describe table")) })
    }

    /// Driver-specific parameters used only during construction.
    type Settings
    where
        Self: Sized;

    /// Establish a pool using the concrete driver's settings.
    fn connect(settings: &Self::Settings) -> impl Future<Output = Result<Self>> + Send
    where
        Self: Sized;
}
