use crate::settings::{Params, TlsSettings};
use secrecy::SecretString;
use serde::Deserialize;

/// ScyllaDB / Cassandra-compatible cluster. `nodes` are contact points in
/// `host` or `host:port` form (port defaults to 9042 at connect time).
/// `keyspace` is the Cassandra analog of a database/schema.
#[derive(Debug, Clone, Deserialize)]
pub struct ScyllaSettings {
    /// Enables TLS when present.
    pub tls: Option<TlsSettings>,
    pub nodes: Vec<String>,
    pub keyspace: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub password: Option<SecretString>,
    /// Local datacenter name for DC-aware load balancing.
    #[serde(default)]
    pub datacenter: Option<String>,
    #[serde(default)]
    pub params: Params,
}
