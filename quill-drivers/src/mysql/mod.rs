mod settings;
pub use settings::MysqlSettings;

use crate::ConnectionPool;
use quill_core::error::{Error, Result};
use secrecy::ExposeSecret;
use sqlx::{
    ConnectOptions, MySqlPool,
    mysql::{MySqlConnectOptions, MySqlSslMode},
};

/// Native Mysql connection pool.
pub struct MysqlConnectionPool {
    pub(crate) inner: MySqlPool,
}

impl MysqlConnectionPool {
    /// Access the native pool for driver-specific operations.
    pub fn inner(&self) -> &MySqlPool {
        &self.inner
    }
}

impl ConnectionPool for MysqlConnectionPool {
    type Settings = MysqlSettings;

    async fn connect(settings: &Self::Settings) -> Result<Self> {
        let connect = async {
            if !settings.params.is_empty() {
                anyhow::bail!("Mysql parameters are not yet supported");
            }
            let mut options = MySqlConnectOptions::new()
                .host(&settings.host)
                .port(settings.port)
                .database(&settings.database)
                .username(&settings.user)
                .ssl_mode(MySqlSslMode::Disabled);
            if let Some(password) = &settings.password {
                options = options.password(password.expose_secret());
            }
            if let Some(tls) = &settings.tls {
                options = options.ssl_mode(MySqlSslMode::VerifyIdentity);
                if let Some(ca) = &tls.ca_cert {
                    options = options.ssl_ca(ca);
                }
                if let Some(identity) = &tls.identity {
                    options = options
                        .ssl_client_cert(&identity.cert)
                        .ssl_client_key(&identity.key);
                }
            }
            let inner = MySqlPool::connect_with(options.disable_statement_logging()).await?;
            Ok::<_, anyhow::Error>(Self { inner })
        };
        connect.await.map_err(|source| Error::DriverConnect {
            connection: "mysql".into(),
            source,
        })
    }
}
