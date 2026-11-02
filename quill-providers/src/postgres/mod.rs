mod settings;
pub use settings::PostgresSettings;

use std::sync::Arc;

use datafusion::{catalog::TableProvider, common::TableReference};
use datafusion_table_providers::{
    postgres::PostgresTableFactory,
    sql::db_connection_pool::{dbconnection::postgresconn::PostgresPooledConnection, postgrespool},
};
use quill_core::error::{Error, Result};
use secrecy::SecretString;
use tokio_postgres::types::ToSql;

use crate::{
    PoolFuture,
    provider::{ProviderPool, Tables, factory_table},
    settings::{ProviderParams, provider_params},
};

pub type PostgresConnectionPool =
    ProviderPool<PostgresPooledConnection, &'static (dyn ToSql + Sync)>;

impl PostgresSettings {
    /// Provider parameters. Typed fields override `params` of the same name.
    fn provider_params(&self) -> Result<ProviderParams> {
        let mut params = provider_params(&self.params);
        let mut set = |key: &str, value: &str| {
            params.insert(key.into(), SecretString::from(value));
        };
        set("host", &self.host);
        set("port", &self.port.to_string());
        set("db", &self.database);
        set("user", &self.user);
        match &self.tls {
            None => set("sslmode", "disable"),
            Some(tls) => {
                if tls.identity.is_some() {
                    return Err(Error::UnsupportedOperation("postgres client certificates"));
                }
                set("sslmode", "verify-full");
                if let Some(ca) = &tls.ca_cert {
                    set("sslrootcert", &ca.to_string_lossy());
                }
            }
        }
        if let Some(password) = &self.password {
            params.insert("pass".into(), password.clone());
        }
        Ok(params)
    }
}

impl crate::Connect for PostgresConnectionPool {
    type Settings = PostgresSettings;

    async fn connect(settings: &Self::Settings) -> Result<Self> {
        let pool = postgrespool::PostgresConnectionPool::new(settings.provider_params()?)
            .await
            .map_err(|source| Error::DriverConnect {
                connection: "postgres".into(),
                source: source.into(),
            })?;
        let pool = Arc::new(pool);
        let tables = Arc::new(PostgresTableFactory::new(pool.clone()));
        Ok(Self::new(pool, tables, settings.database.clone()))
    }
}

impl Tables for PostgresTableFactory {
    fn table(&self, table: TableReference) -> PoolFuture<'_, Arc<dyn TableProvider>> {
        Box::pin(async move { factory_table(self.table_provider(table).await) })
    }
}
