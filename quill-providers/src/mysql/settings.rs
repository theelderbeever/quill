use crate::settings::{Params, TlsSettings};
use secrecy::SecretString;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct MysqlSettings {
    /// Enables TLS when present.
    pub tls: Option<TlsSettings>,
    pub host: String,
    #[serde(default = "default_mysql_port")]
    pub port: u16,
    pub database: String,
    pub user: String,
    #[serde(default)]
    pub password: Option<SecretString>,
    #[serde(default)]
    pub params: Params,
}

const fn default_mysql_port() -> u16 {
    3306
}
