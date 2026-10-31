mod settings;
pub use settings::ClickhouseSettings;

use crate::ConnectionPool;
use quill_core::error::{Error, Result};
use secrecy::ExposeSecret;

/// Official ClickHouse client backed by a reusable HTTP connection pool.
pub struct ClickhouseConnectionPool {
    client: clickhouse::Client,
}

impl ClickhouseConnectionPool {
    /// Access the official client for driver-specific operations.
    pub fn inner(&self) -> &clickhouse::Client {
        &self.client
    }

    /// Stream query results as Arrow batches without collecting the result set.
    ///
    /// The query must omit a FORMAT clause; the Arrow extension supplies it.
    ///
    /// ```no_run
    /// use quill_drivers::clickhouse::ClickhouseConnectionPool;
    ///
    /// async fn first_batch(pool: &ClickhouseConnectionPool) -> anyhow::Result<()> {
    ///     let mut cursor = pool.query_arrow("SELECT 1")?;
    ///     let batch: Option<arrow::record_batch::RecordBatch> = cursor.next().await?;
    ///     Ok(())
    /// }
    /// ```
    pub fn query_arrow(&self, sql: &str) -> Result<clickhouse_ext_arrow::ArrowCursor> {
        use clickhouse_ext_arrow::ArrowQueryExt;
        self.client
            .query(sql)
            .fetch_arrow()
            .map_err(|error| Error::DriverExecute(error.into()))
    }
}

impl ConnectionPool for ClickhouseConnectionPool {
    type Settings = ClickhouseSettings;

    async fn connect(settings: &Self::Settings) -> Result<Self> {
        let connect = async {
            anyhow::ensure!(
                settings.user.is_some() || settings.password.is_none(),
                "ClickHouse password requires a user"
            );
            let scheme = if settings.tls.is_some() {
                "https"
            } else {
                "http"
            };
            let endpoint = url::Url::parse(&format!("{scheme}://{}", settings.host))?;
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
            if let Some(tls) = &settings.tls {
                let mut builder = native_tls::TlsConnector::builder();
                if let Some(ca) = &tls.ca_cert {
                    let pem = tokio::fs::read(ca).await?;
                    for cert in openssl::x509::X509::stack_from_pem(&pem)? {
                        builder.add_root_certificate(native_tls::Certificate::from_der(
                            &cert.to_der()?,
                        )?);
                    }
                }
                if let Some(identity) = &tls.identity {
                    let cert = tokio::fs::read(&identity.cert).await?;
                    let key = secrecy::SecretString::from(
                        tokio::fs::read_to_string(&identity.key).await?,
                    );
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
                let http = hyper_util::client::legacy::Client::builder(
                    hyper_util::rt::TokioExecutor::new(),
                )
                .build(connector);
                client = clickhouse::Client::with_http_client(http);
            }
            client = client
                .with_url(endpoint.as_str())
                .with_database(&settings.database);
            if let Some(user) = &settings.user {
                client = client.with_user(user);
            }
            if let Some(password) = &settings.password {
                client = client.with_password(password.expose_secret());
            }
            for (key, value) in &settings.params {
                client = client.with_setting(key, value);
            }
            client.query("SELECT 1").execute().await?;
            let pool = Self { client };
            Ok::<_, anyhow::Error>(pool)
        };
        connect.await.map_err(|source| Error::DriverConnect {
            connection: "clickhouse".into(),
            source,
        })
    }
}
