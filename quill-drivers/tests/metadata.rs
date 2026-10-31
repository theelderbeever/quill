//! Run with the repository's Docker Compose services:
//! For the TLS overlay set `QUILL_TEST_TLS=1`.
//! `cargo test -p quill-drivers --test metadata -- --ignored`
use quill_core::metadata::{IndexKeySource, TableRef};
use quill_drivers::{
    ConnectionPool,
    clickhouse::{ClickhouseConnectionPool, ClickhouseSettings},
    mysql::{MysqlConnectionPool, MysqlSettings},
    postgres::{PostgresConnectionPool, PostgresSettings},
    scylla::{ScyllaConnectionPool, ScyllaSettings},
};

fn tls() -> Option<quill_drivers::settings::TlsSettings> {
    std::env::var_os("QUILL_TEST_TLS").map(|_| quill_drivers::settings::TlsSettings {
        ca_cert: Some(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../docker/tls/certs/ca.pem"),
        ),
        identity: None,
    })
}

async fn discover(
    pool: &dyn ConnectionPool,
    schema_name: &str,
    table_name: &str,
) -> anyhow::Result<TableRef> {
    for catalog in pool.list_catalogs().await? {
        for schema in pool.list_schemas(&catalog).await? {
            if schema.name == schema_name {
                let table = pool
                    .list_tables(&schema)
                    .await?
                    .into_iter()
                    .find(|t| t.name == table_name)
                    .ok_or_else(|| anyhow::anyhow!("fixture table not discovered"))?;
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
    anyhow::bail!("fixture schema not discovered")
}

#[tokio::test]
#[ignore = "requires Docker Compose Postgres"]
async fn postgres_metadata() -> anyhow::Result<()> {
    let pool = PostgresConnectionPool::connect(&PostgresSettings {
        host: "localhost".into(),
        port: 5432,
        database: "quilt_test".into(),
        user: "quill".into(),
        password: Some("password".into()),
        tls: None,
        params: Default::default(),
    })
    .await?;
    sqlx::query("CREATE SCHEMA quill_metadata_test")
        .execute(pool.inner())
        .await?;
    let result = async {
        sqlx::query("CREATE TABLE quill_metadata_test.items (id INTEGER PRIMARY KEY, title TEXT NOT NULL)").execute(pool.inner()).await?;
        sqlx::query("CREATE INDEX title_idx ON quill_metadata_test.items (lower(title) DESC) INCLUDE (id) WHERE id > 0").execute(pool.inner()).await?;
        let table = discover(&pool, "quill_metadata_test", "items").await?;
        let indexes = pool.list_indexes(&table).await?;
        assert_eq!(indexes.len(), 2);
        let index = indexes.iter().find(|i| i.name == "title_idx").ok_or_else(|| anyhow::anyhow!("missing index"))?;
        assert!(matches!(index.keys[0].source, IndexKeySource::Expression(_)));
        assert!(index.keys[0].ordering.is_some_and(|o| o.descending));
        assert_eq!(index.included_columns, ["id"]);
        assert!(index.predicate.is_some());
        Ok::<_, anyhow::Error>(())
    }.await;
    sqlx::query("DROP SCHEMA quill_metadata_test CASCADE")
        .execute(pool.inner())
        .await?;
    result
}

#[tokio::test]
#[ignore = "requires Docker Compose MySQL"]
async fn mysql_metadata() -> anyhow::Result<()> {
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
    sqlx::query("CREATE TABLE quill_metadata_items (id INTEGER PRIMARY KEY, title VARCHAR(100) NOT NULL, INDEX title_idx (title DESC))").execute(pool.inner()).await?;
    let result = async {
        let table = discover(&pool, "quilt_test", "quill_metadata_items").await?;
        let indexes = pool.list_indexes(&table).await?;
        assert_eq!(indexes.len(), 2);
        assert!(indexes.iter().any(|i| i.primary && i.unique));
        let index = indexes
            .iter()
            .find(|i| i.name == "title_idx")
            .ok_or_else(|| anyhow::anyhow!("missing index"))?;
        assert!(index.keys[0].ordering.is_some_and(|o| o.descending));
        Ok::<_, anyhow::Error>(())
    }
    .await;
    sqlx::query("DROP TABLE quill_metadata_items")
        .execute(pool.inner())
        .await?;
    result
}

#[tokio::test]
#[ignore = "requires Docker Compose ClickHouse"]
async fn clickhouse_metadata() -> anyhow::Result<()> {
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
    pool.inner().query("CREATE TABLE quill_metadata_items (id UInt64, title Nullable(String), INDEX title_idx title TYPE bloom_filter GRANULARITY 1) ENGINE = MergeTree ORDER BY id").execute().await?;
    let result = async {
        let table = discover(&pool, "default", "quill_metadata_items").await?;
        let columns = pool.describe_table(&table).await?.columns;
        assert_eq!(columns[1].nullable, Some(true));
        let indexes = pool.list_indexes(&table).await?;
        assert_eq!(indexes.len(), 1);
        assert_eq!(indexes[0].name, "title_idx");
        Ok::<_, anyhow::Error>(())
    }
    .await;
    pool.inner()
        .query("DROP TABLE quill_metadata_items")
        .execute()
        .await?;
    result
}

#[tokio::test]
#[ignore = "requires Docker Compose Scylla"]
async fn scylla_metadata() -> anyhow::Result<()> {
    let pool = ScyllaConnectionPool::connect(&ScyllaSettings {
        nodes: vec!["localhost:9042".into()],
        keyspace: None,
        user: None,
        password: None,
        datacenter: None,
        tls: tls(),
        params: Default::default(),
    })
    .await?;
    pool.inner().query_unpaged("CREATE KEYSPACE quill_metadata_test WITH replication = {'class': 'SimpleStrategy', 'replication_factor': 1}", ()).await?;
    let result = async {
        pool.inner()
            .query_unpaged(
                "CREATE TABLE quill_metadata_test.items (id int PRIMARY KEY, title text)",
                (),
            )
            .await?;
        pool.inner()
            .query_unpaged(
                "CREATE INDEX title_idx ON quill_metadata_test.items (title)",
                (),
            )
            .await?;
        let table = discover(&pool, "quill_metadata_test", "items").await?;
        let indexes = pool.list_indexes(&table).await?;
        assert_eq!(indexes.len(), 1);
        assert_eq!(indexes[0].name, "title_idx");
        Ok::<_, anyhow::Error>(())
    }
    .await;
    pool.inner()
        .query_unpaged("DROP KEYSPACE quill_metadata_test", ())
        .await?;
    result
}
