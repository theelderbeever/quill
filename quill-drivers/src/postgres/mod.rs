mod settings;
pub use settings::PostgresSettings;

use crate::ConnectionPool;
use quill_core::error::{Error, Result};
use secrecy::ExposeSecret;
use sqlx::{
    ConnectOptions, PgPool,
    postgres::{PgConnectOptions, PgSslMode},
};

/// Native Postgres connection pool.
pub struct PostgresConnectionPool {
    pub(crate) inner: PgPool,
}

impl PostgresConnectionPool {
    /// Access the native pool for driver-specific operations.
    pub fn inner(&self) -> &PgPool {
        &self.inner
    }
}

impl ConnectionPool for PostgresConnectionPool {
    type Settings = PostgresSettings;

    async fn connect(settings: &Self::Settings) -> Result<Self> {
        let connect = async {
            if !settings.params.is_empty() {
                anyhow::bail!("Postgres parameters are not yet supported");
            }
            let mut options = PgConnectOptions::new()
                .host(&settings.host)
                .port(settings.port)
                .database(&settings.database)
                .username(&settings.user)
                .ssl_mode(PgSslMode::Disable);
            if let Some(password) = &settings.password {
                options = options.password(password.expose_secret());
            }
            if let Some(tls) = &settings.tls {
                options = options.ssl_mode(PgSslMode::VerifyFull);
                if let Some(ca) = &tls.ca_cert {
                    options = options.ssl_root_cert(ca);
                }
                if let Some(identity) = &tls.identity {
                    options = options
                        .ssl_client_cert(&identity.cert)
                        .ssl_client_key(&identity.key);
                }
            }
            let inner = PgPool::connect_with(options.disable_statement_logging()).await?;
            Ok::<_, anyhow::Error>(Self { inner })
        };
        connect.await.map_err(|source| Error::DriverConnect {
            connection: "postgres".into(),
            source,
        })
    }
}
