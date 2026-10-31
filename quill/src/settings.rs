use std::path::PathBuf;

use figment::{
    Figment,
    providers::{Env, Format, Toml},
};
use indexmap::{IndexMap, IndexSet};
use quill_drivers::settings::{
    ClickhouseSettings, DatafusionSettings, MysqlSettings, PostgresSettings, ScyllaSettings,
    SqliteSettings,
};
use serde::Deserialize;

use quill_core::error::{Error, Result};

/// Top-level config tree. Mirrors `quill.toml` 1:1.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub connection: IndexMap<String, ConnectionSettings>,
}

impl Settings {
    /// Load existing config files in order: home, platform config directory,
    /// colon-separated `QUILL_CONFIG`, then explicit paths. Duplicate paths
    /// are loaded once. Later files and finally `QUILL__` environment values
    /// override earlier values. Read and deserialization errors are `Error::Config`.
    pub fn load(paths: Vec<PathBuf>) -> Result<Self> {
        let mut figment = Figment::new();

        let config_paths = std::env::var("QUILL_CONFIG").unwrap_or_default();
        let paths: Vec<_> = dirs::config_dir()
            .map(|p| p.join("quill/connections.toml"))
            .into_iter()
            .chain(dirs::home_dir().map(|p| p.join(".quill/connections.toml")))
            .chain(
                config_paths
                    .split(':')
                    .filter(|p| !p.is_empty())
                    .map(PathBuf::from),
            )
            .chain(std::iter::once(PathBuf::from(".quill.toml")))
            .chain(paths)
            .filter(|p| p.is_file())
            .collect::<IndexSet<_>>()
            .into_iter()
            .collect();

        for path in paths {
            figment = figment.merge(Toml::file_exact(path));
        }

        figment
            .merge(Env::prefixed("QUILL__").split("__"))
            .extract::<Self>()
            .map_err(|e| Error::Config(e.to_string()))
    }
}

/// Per-vendor connection definition. The TOML map key (e.g. `pg_prod`) is
/// the connection's identifier; this type captures the vendor-specific
/// shape selected by the `kind` discriminant.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ConnectionSettings {
    Datafusion(DatafusionSettings),
    Postgres(PostgresSettings),
    Mysql(MysqlSettings),
    Sqlite(SqliteSettings),
    Clickhouse(ClickhouseSettings),
    Scylla(ScyllaSettings),
}
