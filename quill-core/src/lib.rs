pub mod error;
pub mod query;
pub mod spec;

pub use error::{Error, Result};
pub use query::QueryId;
pub use spec::{
    ClickhouseSpec, Config, ConnectionSpec, DuckdbSpec, MysqlSpec, Params, PostgresSpec,
    ScyllaSpec, SqliteSpec, Workspace,
};

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;

    use pretty_assertions::assert_eq;
    use secrecy::ExposeSecret;

    use super::*;

    #[test]
    fn config_deserializes_quill_toml_shape() {
        let input = r#"
            [workspace.analytics]
            connections = ["pg_prod", "ch_prod"]

            [connection.pg_prod]
            kind = "postgres"
            host = "db.example.com"
            database = "app"
            user = "reader"
            password = "hunter2"
            params = { sslmode = "require", application_name = "quill" }

            [connection.ch_prod]
            kind = "clickhouse"
            url = "http://ch.example.com:8123"
            database = "events"

            [connection.duckdb_project]
            kind = "duckdb"
            database = "/tmp/duck.db"
            base_dir = "/data/project"
        "#;

        let config: Config = toml::from_str(input).unwrap();

        assert_eq!(
            config.workspace["analytics"].connections,
            vec!["pg_prod".to_string(), "ch_prod".to_string()],
        );
        assert_eq!(config.connection.len(), 3);

        match &config.connection["pg_prod"] {
            ConnectionSpec::Postgres(pg) => {
                assert_eq!(pg.host, "db.example.com");
                assert_eq!(pg.port, 5432);
                assert_eq!(pg.database, "app");
                assert_eq!(pg.user, "reader");
                assert_eq!(pg.password.as_ref().unwrap().expose_secret(), "hunter2");
                assert_eq!(
                    pg.params.get("sslmode").map(String::as_str),
                    Some("require")
                );
                assert_eq!(
                    pg.params.get("application_name").map(String::as_str),
                    Some("quill"),
                );
            }
            other => panic!("expected postgres, got {other:?}"),
        }

        match &config.connection["duckdb_project"] {
            ConnectionSpec::Duckdb(d) => {
                assert_eq!(d.database, PathBuf::from("/tmp/duck.db"));
                assert_eq!(d.base_dir, Some(PathBuf::from("/data/project")));
            }
            other => panic!("expected duckdb, got {other:?}"),
        }
    }

    #[test]
    fn scylla_spec_deserializes() {
        let input = r#"
            [connection.events]
            kind = "scylla"
            nodes = ["scylla-0.internal:9042", "scylla-1.internal:9042"]
            keyspace = "telemetry"
            user = "quill"
            password = "hunter2"
            datacenter = "us-east-1"
            params = { compression = "lz4" }
        "#;

        let config: Config = toml::from_str(input).unwrap();
        match &config.connection["events"] {
            ConnectionSpec::Scylla(s) => {
                assert_eq!(s.nodes.len(), 2);
                assert_eq!(s.keyspace, "telemetry");
                assert_eq!(s.user.as_deref(), Some("quill"));
                assert_eq!(s.password.as_ref().unwrap().expose_secret(), "hunter2");
                assert_eq!(s.datacenter.as_deref(), Some("us-east-1"));
                assert_eq!(s.params.get("compression").map(String::as_str), Some("lz4"));
            }
            other => panic!("expected scylla, got {other:?}"),
        }
    }

    #[test]
    fn connection_order_matches_toml_order() {
        let input = r#"
            [connection.beta]
            kind = "sqlite"
            path = "/tmp/b.db"

            [connection.alpha]
            kind = "sqlite"
            path = "/tmp/a.db"

            [connection.gamma]
            kind = "sqlite"
            path = "/tmp/g.db"
        "#;

        let config: Config = toml::from_str(input).unwrap();
        let names: Vec<&str> = config.connection.keys().map(String::as_str).collect();
        assert_eq!(names, vec!["beta", "alpha", "gamma"]);
    }

    #[test]
    fn empty_config_deserializes() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.workspace.is_empty());
        assert!(config.connection.is_empty());
    }

    #[test]
    fn load_reads_toml_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quill.toml");
        std::fs::write(
            &path,
            r#"
                [connection.local]
                kind = "sqlite"
                path = "/tmp/x.db"
            "#,
        )
        .unwrap();

        let config = Config::load(vec![path]).unwrap();
        match &config.connection["local"] {
            ConnectionSpec::Sqlite(s) => {
                assert_eq!(s.path, PathBuf::from("/tmp/x.db"));
            }
            other => panic!("expected sqlite, got {other:?}"),
        }
    }

    #[test]
    fn load_layers_later_files_over_earlier() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base.toml");
        let overlay = dir.path().join("overlay.toml");

        std::fs::write(
            &base,
            r#"
                [connection.db]
                kind = "sqlite"
                path = "/base.db"
            "#,
        )
        .unwrap();

        std::fs::write(
            &overlay,
            r#"
                [connection.db]
                kind = "sqlite"
                path = "/overlay.db"
            "#,
        )
        .unwrap();

        let config = Config::load(vec![base, overlay]).unwrap();
        match &config.connection["db"] {
            ConnectionSpec::Sqlite(s) => {
                assert_eq!(s.path, PathBuf::from("/overlay.db"));
            }
            other => panic!("expected sqlite, got {other:?}"),
        }
    }

    #[test]
    fn query_id_is_unique_per_call() {
        let a = QueryId::new();
        let b = QueryId::new();
        assert_ne!(a, b);
    }
}
