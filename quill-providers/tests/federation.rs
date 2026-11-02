//! Federated sessions over several connections.
#[path = "support/execution.rs"]
mod execution;

use std::sync::Arc;

use arrow::array::{Array, Int64Array, StringArray};
use execution::{TestResult, tls};
use futures::StreamExt;
use quill_core::query::*;
use quill_providers::{
    Connect, ConnectionPool,
    federation::{Member, federate},
    mysql::{MysqlConnectionPool, MysqlSettings},
    postgres::{PostgresConnectionPool, PostgresSettings},
    sqlite::{SqliteConnectionPool, SqliteSettings},
};

async fn run(pool: &dyn ConnectionPool, sql: &str) -> TestResult {
    let mut execution = pool.execute(QueryRequest::new(sql)).await?;
    while let Some(event) = execution.events.next().await {
        event?;
    }
    Ok(())
}

/// `(id, label)` rows from a two-column query, in result order.
async fn pairs(
    pool: &dyn ConnectionPool,
    sql: &str,
) -> Result<Vec<(i64, String)>, Box<dyn std::error::Error>> {
    let mut execution = pool.execute(QueryRequest::new(sql)).await?;
    let mut rows = Vec::new();
    while let Some(event) = execution.events.next().await {
        if let QueryEvent::Batch { batch, .. } = event? {
            let ids = arrow::compute::cast(batch.column(0), &arrow::datatypes::DataType::Int64)?;
            let ids = ids.as_any().downcast_ref::<Int64Array>().ok_or("ids")?;
            let labels = arrow::compute::cast(batch.column(1), &arrow::datatypes::DataType::Utf8)?;
            let labels = labels
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or("labels")?;
            for i in 0..batch.num_rows() {
                rows.push((ids.value(i), labels.value(i).to_owned()));
            }
        }
    }
    Ok(rows)
}

async fn sqlite(
    path: &std::path::Path,
) -> Result<Arc<dyn ConnectionPool>, Box<dyn std::error::Error>> {
    let pool = SqliteConnectionPool::connect(&SqliteSettings {
        path: path.into(),
        busy_timeout_ms: 5000,
        params: Default::default(),
    })
    .await?;
    Ok(Arc::new(pool))
}

#[tokio::test]
async fn joins_tables_across_connections() -> TestResult {
    let dir = tempfile::tempdir()?;
    let users = sqlite(&dir.path().join("users.db")).await?;
    let orders = sqlite(&dir.path().join("orders.db")).await?;
    run(&*users, "CREATE TABLE users (id INTEGER, name TEXT)").await?;
    run(&*users, "INSERT INTO users VALUES (1, 'ada'), (2, 'bo')").await?;
    run(&*orders, "CREATE TABLE orders (user_id INTEGER, item TEXT)").await?;
    run(
        &*orders,
        "INSERT INTO orders VALUES (2, 'lamp'), (1, 'desk'), (2, 'mug')",
    )
    .await?;

    let pool = federate(
        vec![
            Member {
                name: "people".into(),
                pool: users,
            },
            Member {
                name: "sales".into(),
                pool: orders,
            },
        ],
        16,
    )
    .await?;
    let catalogs = pool.list_catalogs().await?;
    let names: Vec<_> = catalogs.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["people", "sales"]);

    let joined = pairs(
        &pool,
        r#"SELECT u.id, o.item FROM "people"."main"."users" u
           JOIN "sales"."main"."orders" o ON o.user_id = u.id
           ORDER BY u.id, o.item"#,
    )
    .await?;
    assert_eq!(
        joined,
        [(1, "desk".into()), (2, "lamp".into()), (2, "mug".into())]
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires Docker Compose Postgres and MySQL (TLS overlay)"]
async fn joins_postgres_with_mysql() -> TestResult {
    let pg: Arc<dyn ConnectionPool> = Arc::new(
        PostgresConnectionPool::connect(&PostgresSettings {
            host: "localhost".into(),
            port: 5432,
            database: "quilt_test".into(),
            user: "quill".into(),
            password: Some("password".into()),
            tls: None,
            params: Default::default(),
        })
        .await?,
    );
    let my: Arc<dyn ConnectionPool> = Arc::new(
        MysqlConnectionPool::connect(&MysqlSettings {
            host: "localhost".into(),
            port: 3306,
            database: "quilt_test".into(),
            user: "quill".into(),
            password: Some("password".into()),
            tls: tls(),
            params: Default::default(),
        })
        .await?,
    );
    run(&*pg, "CREATE TABLE quill_fed_users (id bigint, name text)").await?;
    run(
        &*my,
        "CREATE TABLE quill_fed_orders (user_id BIGINT, item VARCHAR(20))",
    )
    .await?;
    let result = async {
        run(
            &*pg,
            "INSERT INTO quill_fed_users VALUES (1, 'ada'), (2, 'bo')",
        )
        .await?;
        run(
            &*my,
            "INSERT INTO quill_fed_orders VALUES (2, 'lamp'), (1, 'desk')",
        )
        .await?;
        let pool = federate(
            vec![
                Member {
                    name: "pg".into(),
                    pool: pg.clone(),
                },
                Member {
                    name: "my".into(),
                    pool: my.clone(),
                },
            ],
            16,
        )
        .await?;
        let joined = pairs(
            &pool,
            r#"SELECT u.id, o.item FROM "pg"."public"."quill_fed_users" u
               JOIN "my"."quilt_test"."quill_fed_orders" o ON o.user_id = u.id
               ORDER BY u.id"#,
        )
        .await?;
        assert_eq!(joined, [(1, "desk".into()), (2, "lamp".into())]);
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    run(&*pg, "DROP TABLE quill_fed_users").await?;
    run(&*my, "DROP TABLE quill_fed_orders").await?;
    result
}
