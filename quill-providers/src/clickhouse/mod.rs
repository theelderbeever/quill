mod settings;
pub use settings::ClickhouseSettings;

use std::sync::Arc;

use datafusion::{catalog::TableProvider, common::TableReference};
use datafusion_table_providers::{
    clickhouse::ClickHouseTableFactory,
    sql::db_connection_pool::{JoinPushDown, clickhousepool::ClickHouseConnectionPool},
};
use quill_core::error::{Error, Result};
use secrecy::ExposeSecret;

use crate::{
    PoolFuture,
    provider::{ProviderPool, Tables, factory_table},
};

pub type ClickhouseConnectionPool = ProviderPool<clickhouse::Client, ()>;

impl ClickhouseSettings {
    /// Build the client here rather than through provider params, which
    /// cannot express a custom CA or client identity.
    async fn client(&self) -> anyhow::Result<(clickhouse::Client, url::Url)> {
        anyhow::ensure!(
            self.user.is_some() || self.password.is_none(),
            "ClickHouse password requires a user"
        );
        let scheme = if self.tls.is_some() { "https" } else { "http" };
        let endpoint = url::Url::parse(&format!("{scheme}://{}", self.host))?;
        anyhow::ensure!(
            endpoint.host_str().is_some()
                && endpoint.username().is_empty()
                && endpoint.password().is_none()
                && endpoint.path() == "/"
                && endpoint.query().is_none()
                && endpoint.fragment().is_none(),
            "ClickHouse host must contain only a hostname and optional port"
        );
        let mut client = clickhouse::Client::default();
        if let Some(tls) = &self.tls {
            let mut builder = native_tls::TlsConnector::builder();
            if let Some(ca) = &tls.ca_cert {
                let pem = tokio::fs::read(ca).await?;
                for cert in openssl::x509::X509::stack_from_pem(&pem)? {
                    builder
                        .add_root_certificate(native_tls::Certificate::from_der(&cert.to_der()?)?);
                }
            }
            if let Some(identity) = &tls.identity {
                let cert = tokio::fs::read(&identity.cert).await?;
                let key =
                    secrecy::SecretString::from(tokio::fs::read_to_string(&identity.key).await?);
                builder.identity(native_tls::Identity::from_pkcs8(
                    &cert,
                    key.expose_secret().as_bytes(),
                )?);
            }
            let mut http = hyper_util::client::legacy::connect::HttpConnector::new();
            http.enforce_http(false);
            let connector = hyper_tls::HttpsConnector::from((
                http,
                tokio_native_tls::TlsConnector::from(builder.build()?),
            ));
            let http =
                hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                    .build(connector);
            client = clickhouse::Client::with_http_client(http);
        }
        client = client
            .with_url(endpoint.as_str())
            .with_database(&self.database);
        if let Some(user) = &self.user {
            client = client.with_user(user);
        }
        if let Some(password) = &self.password {
            client = client.with_password(password.expose_secret());
        }
        for (key, value) in &self.params {
            client = client.with_setting(key, value);
        }
        client.query("SELECT 1").execute().await?;
        Ok((client, endpoint))
    }
}

impl crate::Connect for ClickhouseConnectionPool {
    type Settings = ClickhouseSettings;

    async fn connect(settings: &Self::Settings) -> Result<Self> {
        let (client, endpoint) =
            settings
                .client()
                .await
                .map_err(|source| Error::DriverConnect {
                    connection: "clickhouse".into(),
                    source,
                })?;
        // Matches the provider's own context so joins push down within one server.
        let context = format!("url={endpoint},db={}", settings.database);
        let pool = ClickHouseConnectionPool {
            client,
            join_push_down: JoinPushDown::AllowedFor(context),
            schemas: Arc::default(),
        };
        let pool = Arc::new(pool);
        let tables = Arc::new(ClickHouseTableFactory::new(pool.clone()));
        Ok(Self::new(pool, tables, settings.database.clone()))
    }
}

impl Tables for ClickHouseTableFactory {
    fn capabilities(&self) -> crate::Capabilities {
        crate::Capabilities {
            named_parameters: true,
            positional_parameters: true,
        }
    }
    fn table(&self, table: TableReference) -> PoolFuture<'_, Arc<dyn TableProvider>> {
        Box::pin(async move { factory_table(self.table_provider(table, None).await) })
    }
}
