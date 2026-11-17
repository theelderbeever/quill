use std::{collections::BTreeMap, path::PathBuf};

use serde::Deserialize;

/// Optional TLS certificates. Postgres selects transport and verification with
/// its separate `sslmode` setting. For other providers, presence enables verified
/// TLS and absence selects plaintext. System trust roots are used unless
/// `ca_cert` supplies a PEM CA bundle.
///
/// ```
/// use quill_providers::settings::TlsSettings;
///
/// let tls = TlsSettings { ca_cert: None, identity: None };
/// assert!(tls.identity.is_none());
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct TlsSettings {
    /// PEM CA bundle used to verify the server.
    pub ca_cert: Option<PathBuf>,
    /// Client identity for mutual TLS.
    pub identity: Option<TlsIdentity>,
}

/// A PEM client certificate chain and its matching PEM private key.
/// Keeping them together prevents configuring only half of an identity.
#[derive(Debug, Clone, Deserialize)]
pub struct TlsIdentity {
    pub cert: PathBuf,
    /// Path only; private key contents must be handled as secrets by drivers.
    pub key: PathBuf,
}

/// Free-form driver parameters. Each spec carries one of these as an
/// escape hatch for vendor-specific knobs that don't deserve typed
/// fields (sslmode, application_name, charset, memory_limit, ...).
/// Drivers consume the keys they recognize and pass the rest through.
pub type Params = BTreeMap<String, String>;

/// A connection's settings cannot be used as written.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("{0} connection parameters are not yet supported")]
    UnsupportedParams(&'static str),
    #[error("ClickHouse password requires a user")]
    PasswordWithoutUser,
    #[error("ClickHouse host `{0}` must contain only a hostname and optional port")]
    InvalidHost(String),
    #[error("Cassandra authentication requires both user and password")]
    IncompleteCredentials,
    #[error("SQLite database `{0}` has no file name")]
    NoFileName(String),
    #[error("SQLite path must be valid UTF-8")]
    NonUtf8Path,
    #[error("duplicate object store authority `{0}`")]
    DuplicateObjectStore(String),
    #[error("table path `{0}` must belong to its parent object store")]
    TableOutsideStore(String),
    #[error("duplicate table name `{0}`")]
    DuplicateTable(String),
}

pub use crate::postgres::PostgresSettings;

pub use crate::mysql::MysqlSettings;

pub use crate::sqlite::SqliteSettings;

pub use crate::clickhouse::ClickhouseSettings;

pub use crate::cassandra::CassandraSettings;

pub use crate::datafusion::DatafusionSettings;
