use std::{num::NonZeroU64, path::PathBuf, sync::Arc, time::Duration};

use figment::{
    Figment,
    providers::{Env, Format, Toml},
};
use indexmap::{IndexMap, IndexSet};
use quill_drivers::{
    ConnectionPool, PoolFuture,
    clickhouse::ClickhouseConnectionPool,
    datafusion::DatafusionConnectionPool,
    mysql::MysqlConnectionPool,
    postgres::PostgresConnectionPool,
    scylla::ScyllaConnectionPool,
    settings::{
        ClickhouseSettings, DatafusionSettings, MysqlSettings, PostgresSettings, ScyllaSettings,
        SqliteSettings,
    },
    sqlite::SqliteConnectionPool,
};
use quill_tui::ConnectionSource;
use serde::Deserialize;

use quill_core::error::{Error, Result};

/// Top-level config tree. Mirrors `quill.toml` 1:1.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub connection: IndexMap<String, ConnectionSettings>,
    #[serde(default)]
    pub results: ResultSettings,
    #[serde(default)]
    pub ui: UiSettings,
}

/// Where drained query results are written. Each query session gets its own
/// temporary directory, removed when the session ends.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ResultSettings {
    /// Parent of per-session directories; the system temp directory when unset.
    pub dir: Option<PathBuf>,
    /// Bytes written per query before streaming stops; unlimited when unset.
    pub max_bytes: Option<NonZeroU64>,
}

/// Terminal UI behaviour.
#[derive(Debug, Clone, Deserialize)]
pub struct UiSettings {
    /// Milliseconds between redraws while the screen has changed.
    #[serde(default = "default_redraw_ms")]
    pub redraw_ms: NonZeroU64,
}

impl UiSettings {
    pub const fn redraw_interval(&self) -> Duration {
        Duration::from_millis(self.redraw_ms.get())
    }
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            redraw_ms: default_redraw_ms(),
        }
    }
}

const fn default_redraw_ms() -> NonZeroU64 {
    // Evaluated at compile time, so the zero arm cannot be reached at runtime.
    const DEFAULT: NonZeroU64 = match NonZeroU64::new(100) {
        Some(ms) => ms,
        None => panic!("redraw interval must be non-zero"),
    };
    DEFAULT
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

impl ConnectionSettings {
    /// The `kind` discriminant as written in config.
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Datafusion(_) => "datafusion",
            Self::Postgres(_) => "postgres",
            Self::Mysql(_) => "mysql",
            Self::Sqlite(_) => "sqlite",
            Self::Clickhouse(_) => "clickhouse",
            Self::Scylla(_) => "scylla",
        }
    }

    pub async fn connect(&self) -> Result<Arc<dyn ConnectionPool>> {
        Ok(match self {
            Self::Datafusion(s) => Arc::new(DatafusionConnectionPool::connect(s).await?),
            Self::Postgres(s) => Arc::new(PostgresConnectionPool::connect(s).await?),
            Self::Mysql(s) => Arc::new(MysqlConnectionPool::connect(s).await?),
            Self::Sqlite(s) => Arc::new(SqliteConnectionPool::connect(s).await?),
            Self::Clickhouse(s) => Arc::new(ClickhouseConnectionPool::connect(s).await?),
            Self::Scylla(s) => Arc::new(ScyllaConnectionPool::connect(s).await?),
        })
    }
}

impl ConnectionSource for ConnectionSettings {
    fn kind(&self) -> &'static str {
        Self::kind(self)
    }

    fn connect(&self) -> PoolFuture<'_, Arc<dyn ConnectionPool>> {
        Box::pin(Self::connect(self))
    }
}
