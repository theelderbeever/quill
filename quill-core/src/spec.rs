use std::{collections::BTreeMap, path::PathBuf};

use indexmap::IndexMap;
use secrecy::SecretString;
use serde::Deserialize;
use url::Url;

use crate::error::{Error, Result};

/// Top-level config tree. Mirrors `quill.toml` 1:1. Maps preserve the
/// order the user wrote in TOML so GUI listings, error messages, and
/// iteration are stable across runs.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub workspace: IndexMap<String, Workspace>,
    #[serde(default)]
    pub connection: IndexMap<String, ConnectionSpec>,
}

impl Config {
    /// Load a `Config` by layering the given TOML files (later paths
    /// override earlier ones) and finally a `QUILL__`-prefixed environment
    /// source. Both file errors and deserialization errors surface as
    /// `Error::Config`.
    pub fn load(paths: Vec<PathBuf>) -> Result<Self> {
        let mut builder = config::Config::builder();

        for path in paths {
            let path_str = path.to_str().ok_or_else(|| {
                Error::Config(format!("non-UTF-8 config path: {}", path.display()))
            })?;
            builder = builder.add_source(config::File::new(path_str, config::FileFormat::Toml));
        }

        builder
            .add_source(
                config::Environment::default()
                    .prefix("QUILL")
                    .prefix_separator("__")
                    .separator("__"),
            )
            .build()
            .and_then(|c| c.try_deserialize::<Self>())
            .map_err(|e| Error::Config(e.to_string()))
    }
}

/// A federation: a named bundle of connection names that DataFusion plans
/// across. Cross-connection joins are only available within a workspace.
#[derive(Debug, Clone, Deserialize)]
pub struct Workspace {
    pub connections: Vec<String>,
}

/// Per-vendor connection definition. The TOML map key (e.g. `pg_prod`) is
/// the connection's identifier; this type captures the vendor-specific
/// shape selected by the `kind` discriminant.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ConnectionSpec {
    Postgres(PostgresSpec),
    Mysql(MysqlSpec),
    Sqlite(SqliteSpec),
    Clickhouse(ClickhouseSpec),
    Duckdb(DuckdbSpec),
    Scylla(ScyllaSpec),
}

/// Free-form driver parameters. Each spec carries one of these as an
/// escape hatch for vendor-specific knobs that don't deserve typed
/// fields (sslmode, application_name, charset, memory_limit, ...).
/// Drivers consume the keys they recognize and pass the rest through.
pub type Params = BTreeMap<String, String>;

#[derive(Debug, Clone, Deserialize)]
pub struct PostgresSpec {
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

#[derive(Debug, Clone, Deserialize)]
pub struct MysqlSpec {
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

#[derive(Debug, Clone, Deserialize)]
pub struct SqliteSpec {
    pub path: PathBuf,
    #[serde(default)]
    pub params: Params,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClickhouseSpec {
    pub url: Url,
    pub database: String,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub password: Option<SecretString>,
    #[serde(default)]
    pub params: Params,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DuckdbSpec {
    pub database: PathBuf,
    #[serde(default)]
    pub base_dir: Option<PathBuf>,
    #[serde(default)]
    pub params: Params,
}

/// ScyllaDB / Cassandra-compatible cluster. `nodes` are contact points in
/// `host` or `host:port` form (port defaults to 9042 at connect time).
/// `keyspace` is the Cassandra analog of a database/schema.
#[derive(Debug, Clone, Deserialize)]
pub struct ScyllaSpec {
    pub nodes: Vec<String>,
    pub keyspace: String,
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
