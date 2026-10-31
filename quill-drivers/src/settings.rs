use std::{collections::BTreeMap, path::PathBuf};

use serde::Deserialize;

/// TLS transport configuration. Presence enables TLS with certificate and
/// hostname verification; absence on a connection selects plaintext.
/// System trust roots are used unless `ca_cert` supplies a PEM CA bundle.
///
/// ```
/// use quill_drivers::settings::TlsSettings;
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

pub use crate::postgres::PostgresSettings;

pub use crate::mysql::MysqlSettings;

pub use crate::sqlite::SqliteSettings;

pub use crate::clickhouse::ClickhouseSettings;

pub use crate::scylla::ScyllaSettings;

pub use crate::datafusion::DatafusionSettings;
