//! Connection files. Each file is read on its own; files never overlay.
use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    sync::Arc,
};

use indexmap::IndexMap;
use quill_core::metadata::TableRef;
use quill_providers::{
    Connect, ConnectionPool, PoolFuture,
    cassandra::CassandraConnectionPool,
    clickhouse::ClickhouseConnectionPool,
    datafusion::DatafusionConnectionPool,
    mysql::MysqlConnectionPool,
    postgres::PostgresConnectionPool,
    settings::{
        CassandraSettings, ClickhouseSettings, DatafusionSettings, MysqlSettings, PostgresSettings,
        SqliteSettings,
    },
    sqlite::SqliteConnectionPool,
};
use quill_tui::{ConnectionGroup, ConnectionSource, quote};
use serde::Deserialize;

use crate::{
    home::QuillHome,
    workspaces::{Members, ResolvedWorkspace, WorkspaceToml},
};
use quill_core::error::{Error, Result};

#[derive(Debug, Deserialize)]
struct ConnectionsToml {
    #[serde(default)]
    connection: IndexMap<String, Connection>,
    #[serde(default)]
    workspace: IndexMap<String, WorkspaceToml>,
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
    Cassandra(CassandraSettings),
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
            Self::Cassandra(_) => "cassandra",
        }
    }

    /// The `user` discriminant as written in config.
    pub fn user(&self) -> Option<&str> {
        match self {
            Self::Datafusion(_) | Self::Sqlite(_) => None,
            Self::Postgres(settings) => Some(settings.user.as_str()),
            Self::Mysql(settings) => Some(settings.user.as_str()),
            Self::Clickhouse(settings) => settings.user.as_deref(),
            Self::Cassandra(settings) => settings.user.as_deref(),
        }
    }

    /// Where the data lives: the server, the Cassandra nodes, or DataFusion's
    /// object stores.
    pub fn host(&self) -> Option<Cow<'_, str>> {
        match self {
            Self::Sqlite(_) => None,
            Self::Postgres(s) => Some(Cow::Owned(format!("{}:{}", s.host, s.port))),
            Self::Mysql(s) => Some(Cow::Owned(format!("{}:{}", s.host, s.port))),
            Self::Clickhouse(s) => Some(Cow::Borrowed(&s.host)),
            Self::Cassandra(s) => listed(s.nodes.iter().map(String::as_str)),
            Self::Datafusion(s) => listed(s.object_stores.iter().map(|o| o.provider.location())),
        }
    }

    /// The database, or its equivalent: Cassandra's keyspace, SQLite's file.
    pub fn database(&self) -> Option<&str> {
        match self {
            Self::Datafusion(_) => None,
            Self::Postgres(s) => Some(&s.database),
            Self::Mysql(s) => Some(&s.database),
            Self::Clickhouse(s) => Some(&s.database),
            Self::Cassandra(s) => s.keyspace.as_deref(),
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
            Self::Cassandra(s) => Arc::new(CassandraConnectionPool::connect(s).await?),
        })
    }
}

/// A connection with its query directory resolved against its file.
/// The first of `items`, with a count of the rest; `None` when empty.
pub(crate) fn listed<'a>(
    mut items: impl ExactSizeIterator<Item = &'a str>,
) -> Option<Cow<'a, str>> {
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

/// Connections and workspaces read from one file.
pub struct ConnectionFile {
    path: PathBuf,
    connections: IndexMap<String, Connection>,
    workspaces: IndexMap<String, (Members, WorkspaceToml)>,
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
        let in_file =
            |reason: &dyn std::fmt::Display| Error::Config(format!("{}: {reason}", path.display()));
        let mut workspaces = IndexMap::new();
        for (name, workspace) in parsed.workspace {
            if parsed.connection.contains_key(&name) {
                let reason = format!("workspace `{name}` shares its name with a connection");
                return Err(in_file(&reason));
            }
            let members = Members::resolve(&name, &workspace.connections, &parsed.connection)
                .map_err(|e| in_file(&e))?;
            workspaces.insert(name, (members, workspace));
        }
        let path = std::path::absolute(path)
            .map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
        Ok(Some(Self {
            path,
            connections: parsed.connection,
            workspaces,
        }))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Resolve query directories. An explicit `queries` is relative to the
    /// file; otherwise home connections use `QUILL_HOME/queries/<name>` and
    /// others `<file dir>/.quill/queries/<name>`. Workspaces follow the same
    /// rule and are listed after the connections.
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
        let connections = self.connections.into_iter().map(|(name, connection)| {
            let query_dir = connection
                .queries
                .map_or_else(|| default_root.join(&name), |q| dir.join(q));
            let source: Box<dyn ConnectionSource> = Box::new(ResolvedConnection {
                settings: connection.driver,
                query_dir,
            });
            (name, source)
        });
        let workspaces = self
            .workspaces
            .into_iter()
            .map(|(name, (members, workspace))| {
                let query_dir = workspace
                    .queries
                    .map_or_else(|| default_root.join(&name), |q| dir.join(q));
                let source: Box<dyn ConnectionSource> = Box::new(ResolvedWorkspace {
                    members,
                    query_dir,
                    retained_metrics: workspace.retained_metrics,
                });
                (name, source)
            });
        let connections = connections.chain(workspaces).collect();
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
            kind = "cassandra"
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
    fn workspaces_follow_connections_and_name_their_members() {
        let root = tempfile::tempdir().unwrap();
        let home = QuillHome::at(root.path().join("home"));
        let path = root.path().join(".quill.toml");
        std::fs::write(
            &path,
            "[connection.a]\nkind = \"sqlite\"\npath = \":memory:\"\n\
             [connection.b]\nkind = \"sqlite\"\npath = \":memory:\"\n\
             [workspace.both]\nconnections = [\"a\", { conn = \"b\", alias = \"bee\" }]\n",
        )
        .unwrap();
        let group = group(&home, &path);
        let (name, source) = &group.connections[2];
        assert_eq!(name, "both");
        assert_eq!(source.kind(), "workspace");
        assert_eq!(source.host().as_deref(), Some("a +1"));
        let Some(Ok(file)) = ConnectionFile::load(&path).transpose() else {
            panic!("loads");
        };
        assert_eq!(
            file.workspaces["both"].0.names().collect::<Vec<_>>(),
            ["a", "bee"]
        );
        assert_eq!(source.query_dir(), root.path().join(".quill/queries/both"));
    }

    #[tokio::test]
    async fn workspace_connects_its_members_and_joins_across_them() {
        use futures::StreamExt;
        use quill_core::query::{QueryEvent, QueryRequest};

        let dir = tempfile::tempdir().unwrap();
        let run = |pool: Arc<dyn ConnectionPool>, sql: &'static str| async move {
            let mut execution = pool.execute(QueryRequest::new(sql)).await.unwrap();
            let mut rows = 0;
            while let Some(event) = execution.events.next().await {
                if let QueryEvent::Batch { batch, .. } = event.unwrap() {
                    rows += batch.num_rows();
                }
            }
            rows
        };
        let path = dir.path().join("connections.toml");
        let db = |name: &str| dir.path().join(name).display().to_string();
        std::fs::write(
            &path,
            format!(
                "[connection.a]\nkind = \"sqlite\"\npath = \"{}\"\n\
                 [connection.b]\nkind = \"sqlite\"\npath = \"{}\"\n\
                 [workspace.both]\nconnections = [\"a\", {{ conn = \"b\", alias = \"bee\" }}]\n",
                db("a.db"),
                db("b.db")
            ),
        )
        .unwrap();
        let home = QuillHome::at(dir.path().join("home"));
        let group = group(&home, &path);
        let a = group.connections[0].1.connect().await.unwrap();
        let b = group.connections[1].1.connect().await.unwrap();
        run(a.clone(), "CREATE TABLE t (id INTEGER)").await;
        run(a, "INSERT INTO t VALUES (1), (2)").await;
        run(b.clone(), "CREATE TABLE u (id INTEGER)").await;
        run(b, "INSERT INTO u VALUES (2), (3)").await;

        let workspace = group.connections[2].1.connect().await.unwrap();
        let joined = run(
            workspace,
            r#"SELECT * FROM "a"."main"."t" JOIN "bee"."main"."u" USING (id)"#,
        )
        .await;
        assert_eq!(joined, 1);
    }

    #[test]
    fn rejects_unusable_workspaces() {
        let dir = tempfile::tempdir().unwrap();
        let connection = "[connection.a]\nkind = \"sqlite\"\npath = \":memory:\"\n";
        for (workspace, reason) in [
            ("[workspace.w]\nconnections = []\n", "lists no connections"),
            (
                "[workspace.w]\nconnections = [\"a\", \"a\"]\n",
                "names catalog `a` twice",
            ),
            (
                "[workspace.w]\nconnections = [\"a\", { conn = \"z\", alias = \"a\" }]\n",
                "names catalog `a` twice",
            ),
            ("[workspace.w]\nconnections = [\"z\"]\n", "names `z`"),
            ("[workspace.a]\nconnections = [\"a\"]\n", "shares its name"),
        ] {
            let path = dir.path().join("connections.toml");
            std::fs::write(&path, format!("{connection}{workspace}")).unwrap();
            let Err(error) = ConnectionFile::load(&path) else {
                panic!("{workspace} must be rejected");
            };
            assert!(error.to_string().contains(reason), "{error}");
        }
    }

    #[test]
    fn repository_dev_config_loads() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.quill.toml");
        let file = ConnectionFile::load(&path).unwrap().unwrap();
        assert!(!file.workspaces.is_empty());
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
