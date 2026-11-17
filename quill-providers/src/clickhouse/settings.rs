use crate::settings::{Params, TlsSettings};
use secrecy::SecretString;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct ClickhouseSettings {
    /// Enables TLS when present.
    pub tls: Option<TlsSettings>,
    pub host: String,
    pub database: String,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub password: Option<SecretString>,
    #[serde(default)]
    pub params: Params,
}
