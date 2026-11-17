//! Run with the repository's Docker Compose services:
//! For the TLS overlay set `QUILL_TEST_TLS=1`.
//! `cargo test -p quill-providers --test metadata -- --ignored`
use quill_core::metadata::TableRef;
use quill_providers::{
    Connect, ConnectionPool,
    cassandra::{CassandraConnectionPool, CassandraSettings},
    clickhouse::{ClickhouseConnectionPool, ClickhouseSettings},
    mysql::{MysqlConnectionPool, MysqlSettings},
    postgres::{PostgresConnectionPool, PostgresSettings},
};

fn tls() -> Option<quill_providers::settings::TlsSettings> {
    std::env::var_os("QUILL_TEST_TLS").map(|_| quill_providers::settings::TlsSettings {
        ca_cert: Some(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../docker/tls/certs/ca.pem"),
        ),
        identity: None,
    })
}

async fn run(
    pool: &dyn ConnectionPool,
    sql: &str,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    use futures::StreamExt;
    let mut execution = pool
        .execute(quill_core::query::QueryRequest::new(sql))
        .await?;
    while let Some(event) = execution.events.next().await {
        event?;
    }
    Ok(())
}

async fn discover(
    pool: &dyn ConnectionPool,
    schema_name: &str,
    table_name: &str,
) -> std::result::Result<TableRef, Box<dyn std::error::Error>> {
    for catalog in pool.list_catalogs().await? {
        for schema in pool.list_schemas(&catalog).await? {
            if schema.name == schema_name {
                let table = pool
                    .list_tables(&schema)
                    .await?
                    .into_iter()
                    .find(|t| t.name == table_name)
                    .ok_or("fixture table not discovered")?;
                let description = pool.describe_table(&table).await?;
                assert_eq!(description.columns.len(), 2);
                assert_eq!(description.columns[0].name, "id");
                assert!(!description.columns[0].native_type.is_empty());
                let mut missing = table.clone();
                missing.name.push_str("_missing");
                assert!(pool.describe_table(&missing).await.is_err());
                assert!(pool.list_indexes(&missing).await.is_err());
                return Ok(table);
            }
        }
    }
    Err("fixture schema not discovered".into())
}

#[tokio::test]
#[ignore = "requires Docker Compose Postgres"]
async fn postgres_metadata() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let pool = PostgresConnectionPool::connect(&PostgresSettings {
        sslmode: quill_providers::postgres::SslMode::Disable,
        host: "localhost".into(),
        port: 5432,
        database: "quilt_test".into(),
        user: "quill".into(),
        password: Some("password".into()),
        tls: None,
        params: Default::default(),
    })
    .await?;
    for sql in [
        "CREATE SCHEMA quill_metadata_test",
        "CREATE TABLE quill_metadata_test.items (id INTEGER PRIMARY KEY, title TEXT NOT NULL)",
    ] {
        run(&pool, sql).await?;
    }
    let result = discover(&pool, "quill_metadata_test", "items")
        .await
        .map(drop);
    run(&pool, "DROP SCHEMA quill_metadata_test CASCADE").await?;
    result
}

#[tokio::test]
#[ignore = "requires Docker Compose MySQL"]
async fn mysql_metadata() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let pool = MysqlConnectionPool::connect(&MysqlSettings {
        host: "localhost".into(),
        port: 3306,
        database: "quilt_test".into(),
        user: "quill".into(),
        password: Some("password".into()),
        tls: tls(),
        params: Default::default(),
    })
    .await?;
    run(
        &pool,
        "CREATE TABLE quill_metadata_items (id INTEGER PRIMARY KEY, title VARCHAR(100) NOT NULL)",
    )
    .await?;
    let result = discover(&pool, "quilt_test", "quill_metadata_items")
        .await
        .map(drop);
    run(&pool, "DROP TABLE quill_metadata_items").await?;
    result
}

#[tokio::test]
#[ignore = "requires Docker Compose ClickHouse"]
async fn clickhouse_metadata() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let pool = ClickhouseConnectionPool::connect(&ClickhouseSettings {
        host: if tls().is_some() {
            "localhost:8443"
        } else {
            "localhost:8123"
        }
        .into(),
        database: "default".into(),
        user: Some("quill".into()),
        password: Some("password".into()),
        tls: tls(),
        params: Default::default(),
    })
    .await?;
    run(&pool, "CREATE TABLE quill_metadata_items (id UInt64, title Nullable(String)) ENGINE = MergeTree ORDER BY id").await?;
    let result = async {
        let table = discover(&pool, "default", "quill_metadata_items").await?;
        let columns = pool.describe_table(&table).await?.columns;
        assert_eq!(columns[1].nullable, Some(true));
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    run(&pool, "DROP TABLE quill_metadata_items").await?;
    result
}

#[tokio::test]
#[ignore = "requires Docker Compose Scylla"]
async fn cassandra_metadata() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let pool = CassandraConnectionPool::connect(&CassandraSettings {
        nodes: vec!["localhost:9042".into()],
        keyspace: None,
        user: None,
        password: None,
        datacenter: None,
        tls: tls(),
        params: Default::default(),
    })
    .await?;
    run(&pool, "CREATE KEYSPACE quill_metadata_test WITH replication = {'class': 'NetworkTopologyStrategy', 'replication_factor': 1}").await?;
    let result = async {
        run(
            &pool,
            "CREATE TABLE quill_metadata_test.items (id int PRIMARY KEY, title text)",
        )
        .await?;
        discover(&pool, "quill_metadata_test", "items")
            .await
            .map(drop)
    }
    .await;
    run(&pool, "DROP KEYSPACE quill_metadata_test").await?;
    result
}
