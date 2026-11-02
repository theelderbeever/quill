mod settings;
pub use settings::MysqlSettings;

use std::sync::Arc;

use datafusion::{catalog::TableProvider, common::TableReference};
use datafusion_table_providers::{mysql::MySQLTableFactory, sql::db_connection_pool::mysqlpool};
use mysql_async::prelude::ToValue;
use quill_core::error::{Error, Result};
use secrecy::SecretString;

use crate::{
    PoolFuture,
    provider::{ProviderPool, Tables, factory_table},
    settings::{ProviderParams, provider_params},
};

pub type MysqlConnectionPool = ProviderPool<mysql_async::Conn, &'static (dyn ToValue + Sync)>;

impl MysqlSettings {
    /// Provider parameters. Typed fields override `params` of the same name.
    fn provider_params(&self) -> Result<ProviderParams> {
        let mut params = provider_params(&self.params);
        let mut set = |key: &str, value: &str| {
            params.insert(key.into(), SecretString::from(value));
        };
        set("host", &self.host);
        set("tcp_port", &self.port.to_string());
        set("db", &self.database);
        set("user", &self.user);
        match &self.tls {
            None => set("sslmode", "disabled"),
            Some(tls) => {
                if tls.identity.is_some() {
                    return Err(Error::UnsupportedOperation("mysql client certificates"));
                }
                // The provider's "required" verifies the chain and hostname.
                set("sslmode", "required");
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

impl crate::Connect for MysqlConnectionPool {
    type Settings = MysqlSettings;

    async fn connect(settings: &Self::Settings) -> Result<Self> {
        let pool = mysqlpool::MySQLConnectionPool::new(settings.provider_params()?)
            .await
            .map_err(|source| Error::DriverConnect {
                connection: "mysql".into(),
                source: source.into(),
            })?;
        let pool = Arc::new(pool);
        let tables = Arc::new(MySQLTableFactory::new(pool.clone()));
        Ok(Self::new(pool, tables, settings.database.clone()))
    }
}

impl Tables for MySQLTableFactory {
    fn table(&self, table: TableReference) -> PoolFuture<'_, Arc<dyn TableProvider>> {
        Box::pin(async move { factory_table(self.table_provider(table).await) })
    }
}
