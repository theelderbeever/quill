use thiserror::Error;

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Any driver's own error, kept whole so its source chain survives.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("query execution failed")]
    QueryExecution(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error(transparent)]
    ServerMetrics(#[from] crate::query::ServerMetricsError),

    #[error("required metadata missing: {0}")]
    MissingMetadata(&'static str),

    #[error("operation is not supported: {0}")]
    UnsupportedOperation(&'static str),

    #[error("config: {0}")]
    Config(String),

    #[error("driver connect failed for {connection}")]
    DriverConnect {
        connection: String,
        #[source]
        source: BoxError,
    },

    #[error("driver execute failed")]
    DriverExecute(#[source] BoxError),

    #[error("table {schema}.{table} not found")]
    TableNotFound { schema: String, table: String },

    #[error("gui channel closed")]
    GuiChannel,
}
