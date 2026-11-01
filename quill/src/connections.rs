//! Connection files. Each file is read on its own; files never overlay.
use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    sync::Arc,
};

use indexmap::IndexMap;
use quill_core::metadata::TableRef;
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
use quill_tui::{ConnectionGroup, ConnectionSource, quote};
use serde::Deserialize;

use crate::home::QuillHome;
use quill_core::error::{Error, Result};

#[derive(Debug, Deserialize)]
struct ConnectionsToml {
    #[serde(default)]
    connection: IndexMap<String, Connection>,
}

/// One `[connection.<name>]` table.
#[derive(Debug, Clone, Deserialize)]
pub struct Connection {
    /// Saved query directory, relative to the file's directory.
    pub queries: Option<PathBuf>,
    #[serde(flatten)]
    pub driver: ConnectionSettings,
}

/// Per-vendor connection definition, selected by the `kind` discriminant.
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

    /// The `user` discriminant as written in config.
    pub fn user(&self) -> Option<&str> {
        match self {
            Self::Datafusion(_) | Self::Sqlite(_) => None,
            Self::Postgres(settings) => Some(settings.user.as_str()),
            Self::Mysql(settings) => Some(settings.user.as_str()),
            Self::Clickhouse(settings) => settings.user.as_deref(),
            Self::Scylla(settings) => settings.user.as_deref(),
        }
    }

    /// Where the data lives: the server, the Scylla nodes, or DataFusion's
    /// object stores.
    pub fn host(&self) -> Option<Cow<'_, str>> {
        match self {
            Self::Sqlite(_) => None,
            Self::Postgres(s) => Some(Cow::Owned(format!("{}:{}", s.host, s.port))),
            Self::Mysql(s) => Some(Cow::Owned(format!("{}:{}", s.host, s.port))),
            Self::Clickhouse(s) => Some(Cow::Borrowed(&s.host)),
            Self::Scylla(s) => listed(s.nodes.iter().map(String::as_str)),
            Self::Datafusion(s) => listed(s.object_stores.iter().map(|o| o.provider.location())),
        }
    }

    /// The database, or its equivalent: Scylla's keyspace, SQLite's file.
    pub fn database(&self) -> Option<&str> {
        match self {
            Self::Datafusion(_) => None,
            Self::Postgres(s) => Some(&s.database),
            Self::Mysql(s) => Some(&s.database),
            Self::Clickhouse(s) => Some(&s.database),
            Self::Scylla(s) => s.keyspace.as_deref(),
            Self::Sqlite(s) => s.path.to_str(),
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

/// A connection with its query directory resolved against its file.
/// The first of `items`, with a count of the rest; `None` when empty.
fn listed<'a>(mut items: impl ExactSizeIterator<Item = &'a str>) -> Option<Cow<'a, str>> {
    let rest = items.len().checked_sub(1)?;
    let first = items.next()?;
    Some(match rest {
        0 => Cow::Borrowed(first),
        n => Cow::Owned(format!("{first} +{n}")),
    })
}

pub struct ResolvedConnection {
    settings: ConnectionSettings,
    query_dir: PathBuf,
}

impl ConnectionSource for ResolvedConnection {
    fn user(&self) -> Option<&str> {
        self.settings.user()
    }

    fn kind(&self) -> &'static str {
        self.settings.kind()
    }

    fn host(&self) -> Option<Cow<'_, str>> {
        self.settings.host()
    }

    fn database(&self) -> Option<&str> {
        self.settings.database()
    }

    fn connect(&self) -> PoolFuture<'_, Arc<dyn ConnectionPool>> {
        Box::pin(self.settings.connect())
    }

    fn query_dir(&self) -> &Path {
        &self.query_dir
    }

    /// MySQL quotes with backticks; DataFusion tables may live in any catalog.
    fn table_reference(&self, table: &TableRef) -> String {
        match &self.settings {
            ConnectionSettings::Mysql(_) => {
                format!(
                    "{}.{}",
                    quote(&table.schema.name, '`'),
                    quote(&table.name, '`')
                )
            }
            ConnectionSettings::Datafusion(_) => format!(
                "{}.{}.{}",
                quote(&table.schema.catalog.name, '"'),
                quote(&table.schema.name, '"'),
                quote(&table.name, '"')
            ),
            _ => format!(
                "{}.{}",
                quote(&table.schema.name, '"'),
                quote(&table.name, '"')
            ),
        }
    }
}

/// Connections read from one file.
pub struct ConnectionFile {
    path: PathBuf,
    connections: IndexMap<String, Connection>,
}

impl ConnectionFile {
    /// Parse `path`; `Ok(None)` when it does not exist.
    pub fn load(path: &Path) -> Result<Option<Self>> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Error::Config(format!("{}: {e}", path.display()))),
        };
        let parsed: ConnectionsToml =
            toml::from_str(&text).map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
        let path = std::path::absolute(path)
            .map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
        Ok(Some(Self {
            path,
            connections: parsed.connection,
        }))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Resolve query directories. An explicit `queries` is relative to the
    /// file; otherwise home connections use `QUILL_HOME/queries/<name>` and
    /// others `<file dir>/.quill/queries/<name>`.
    pub fn into_group(self, home: &QuillHome) -> ConnectionGroup {
        let dir = self
            .path
            .parent()
            .map_or_else(PathBuf::new, Path::to_path_buf);
        let default_root = if self.path == home.connections() {
            home.queries()
        } else {
            dir.join(".quill/queries")
        };
        let connections = self
            .connections
            .into_iter()
            .map(|(name, connection)| {
                let query_dir = connection
                    .queries
                    .map_or_else(|| default_root.join(&name), |q| dir.join(q));
                let source: Box<dyn ConnectionSource> = Box::new(ResolvedConnection {
                    settings: connection.driver,
                    query_dir,
                });
                (name, source)
            })
            .collect();
        ConnectionGroup {
            label: display_path(&self.path),
            connections,
        }
    }
}

/// `path` with the home directory written as `~`.
fn display_path(path: &Path) -> String {
    dirs::home_dir()
        .and_then(|home| path.strip_prefix(home).ok())
        .map_or_else(
            || path.display().to_string(),
            |rest| Path::new("~").join(rest).display().to_string(),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(home: &QuillHome, path: &Path) -> ConnectionGroup {
        ConnectionFile::load(path)
            .unwrap()
            .unwrap()
            .into_group(home)
    }

    #[test]
    fn details_show_each_drivers_host_and_database() {
        let toml = r#"
            [connection.pg]
            kind = "postgres"
            host = "db"
            database = "app"
            user = "me"
            [connection.lite]
            kind = "sqlite"
            path = "/tmp/app.db"
            [connection.cass]
            kind = "scylla"
            nodes = ["a:9042", "b:9042", "c:9042"]
            keyspace = "events"
            [connection.lake]
            kind = "datafusion"
            [[connection.lake.object_stores]]
            kind = "local"
            path = "/data"
        "#;
        let file: ConnectionsToml = toml::from_str(toml).unwrap();
        let details: Vec<_> = file
            .connection
            .values()
            .map(|c| (c.driver.host(), c.driver.database()))
            .collect();
        assert_eq!(
            details,
            [
                (Some("db:5432".into()), Some("app")),
                (None, Some("/tmp/app.db")),
                (Some("a:9042 +2".into()), Some("events")),
                (Some("/data".into()), None),
            ]
        );
    }

    #[test]
    fn resolves_query_directories_by_file_location() {
        let root = tempfile::tempdir().unwrap();
        let home = QuillHome::at(root.path().join("home"));
        std::fs::create_dir_all(home.path()).unwrap();
        std::fs::write(
            home.connections(),
            "[connection.a]\nkind = \"sqlite\"\npath = \":memory:\"\n",
        )
        .unwrap();
        let project = root.path().join("project/.quill.toml");
        std::fs::create_dir_all(project.parent().unwrap()).unwrap();
        std::fs::write(
            &project,
            "[connection.a]\nkind = \"sqlite\"\npath = \":memory:\"\n\
             [connection.b]\nkind = \"sqlite\"\npath = \":memory:\"\nqueries = \"sql\"\n",
        )
        .unwrap();

        let home_group = group(&home, &home.connections());
        assert_eq!(
            home_group.connections[0].1.query_dir(),
            home.queries().join("a")
        );

        let project_group = group(&home, &project);
        let project_dir = root.path().join("project");
        assert_eq!(
            project_group.connections[0].1.query_dir(),
            project_dir.join(".quill/queries/a")
        );
        assert_eq!(
            project_group.connections[1].1.query_dir(),
            project_dir.join("sql")
        );
        assert_eq!(project_group.connections[1].1.kind(), "sqlite");
    }

    #[test]
    fn missing_file_is_none() {
        assert!(
            ConnectionFile::load(Path::new("/nonexistent/quill.toml"))
                .unwrap()
                .is_none()
        );
    }
}
