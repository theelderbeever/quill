use quill_core::{ConnectionSpec, Error, Result};

use datafusion_table_providers::sql::db_connection_pool::postgrespool::PostgresConnectionPool;

pub enum ConnectionPool {
    Postgres(PostgresConnectionPool),
}

pub async fn connect(name: &str, spec: &ConnectionSpec) -> Result<ConnectionPool> {
    match spec {
        ConnectionSpec::Postgres(pg) => crate::postgres::connect(name, pg).await,
        other => Err(Error::DriverConnect {
            connection: name.to_string(),
            source: anyhow::anyhow!("Source not yet supported: {other:?}"),
        }),
    }
}
