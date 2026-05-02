use quill_core::{ConnectionSpec, Error, Result};

#[cfg(feature = "postgres")]
use datafusion_table_providers::sql::db_connection_pool::postgrespool::PostgresConnectionPool;

pub enum ConnectionPool {
    #[cfg(feature = "postgres")]
    Postgres(PostgresConnectionPool),
}

pub async fn connect(name: &str, spec: &ConnectionSpec) -> Result<ConnectionPool> {
    match spec {
        #[cfg(feature = "postgres")]
        ConnectionSpec::Postgres(pg) => crate::postgres::connect(name, pg).await,
        #[cfg(not(feature = "postgres"))]
        ConnectionSpec::Postgres(_) => Err(Error::DriverConnect {
            connection: name.to_string(),
            source: anyhow::anyhow!("quill-drivers built without `postgres` feature"),
        }),
        other => Err(Error::DriverConnect {
            connection: name.to_string(),
            source: anyhow::anyhow!("vendor not yet supported: {other:?}"),
        }),
    }
}
