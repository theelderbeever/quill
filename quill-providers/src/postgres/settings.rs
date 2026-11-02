use crate::settings::{Params, TlsSettings};
use secrecy::SecretString;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct PostgresSettings {
    /// Native Postgres TLS policy; defaults to encrypted transport.
    #[serde(default = "default_sslmode")]
    pub sslmode: SslMode,
    /// Optional CA bundle and client identity.
    pub tls: Option<TlsSettings>,
    pub host: String,
    #[serde(default = "default_pg_port")]
    pub port: u16,
    pub database: String,
    pub user: String,
    #[serde(default)]
    pub password: Option<SecretString>,
    #[serde(default)]
    pub params: Params,
}

const fn default_pg_port() -> u16 {
    5432
}

/// Postgres transport security policy.
///
/// `Require` encrypts without verifying the server certificate. `VerifyCa` checks
/// its trust chain; `VerifyFull` additionally checks the hostname.
///
/// ```
/// use quill_providers::postgres::SslMode;
/// assert_eq!(SslMode::VerifyFull.as_str(), "verify-full");
/// ```
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SslMode {
    Disable,
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

impl SslMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disable => "disable",
            Self::Prefer => "prefer",
            Self::Require => "require",
            Self::VerifyCa => "verify-ca",
            Self::VerifyFull => "verify-full",
        }
    }
}

const fn default_sslmode() -> SslMode {
    SslMode::Require
}
