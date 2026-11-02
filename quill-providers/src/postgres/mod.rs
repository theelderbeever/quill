mod settings;
pub use settings::{PostgresSettings, SslMode};

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
        set("sslmode", self.sslmode.as_str());
        if let Some(tls) = &self.tls {
            if let Some(identity) = &tls.identity {
                set("sslcert", &identity.cert.to_string_lossy());
                set("sslkey", &identity.key.to_string_lossy());
            }
            if let Some(ca) = &tls.ca_cert {
                set("sslrootcert", &ca.to_string_lossy());
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
    fn capabilities(&self) -> crate::Capabilities {
        crate::Capabilities {
            named_parameters: false,
            positional_parameters: true,
        }
    }
    fn table(&self, table: TableReference) -> PoolFuture<'_, Arc<dyn TableProvider>> {
        Box::pin(async move { factory_table(self.table_provider(table).await) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    const CONNECTION: &str = "host = 'localhost'\ndatabase = 'app'\nuser = 'user'\n";

    #[test]
    fn defaults_to_require_without_tls_settings() {
        let settings: PostgresSettings = toml::from_str(CONNECTION).unwrap();
        assert_eq!(settings.sslmode, SslMode::Require);
        assert_eq!(
            settings.provider_params().unwrap()["sslmode"].expose_secret(),
            "require"
        );
    }

    #[test]
    fn forwards_explicit_modes_and_certificate_settings() {
        for mode in ["disable", "prefer", "require", "verify-ca", "verify-full"] {
            let text = format!(
                "{CONNECTION}sslmode = '{mode}'\n[tls]\nca_cert = 'ca.pem'\n[tls.identity]\ncert = 'client.pem'\nkey = 'client.key'\n[params]\nsslmode = 'invalid'"
            );
            let settings: PostgresSettings = toml::from_str(&text).unwrap();
            let params = settings.provider_params().unwrap();
            assert_eq!(params["sslmode"].expose_secret(), mode);
            assert_eq!(params["sslrootcert"].expose_secret(), "ca.pem");
            assert_eq!(params["sslcert"].expose_secret(), "client.pem");
            assert_eq!(params["sslkey"].expose_secret(), "client.key");
        }
        assert!(
            toml::from_str::<PostgresSettings>(&format!("{CONNECTION}sslmode = 'invalid'"))
                .is_err()
        );
    }
}
