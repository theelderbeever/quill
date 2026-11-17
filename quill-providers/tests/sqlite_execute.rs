//! Execution integration tests for sqlite.
#[path = "support/execution.rs"]
mod execution;

use execution::{TestResult, failures, metrics, nulls, rows};
use quill_core::query::*;
use quill_providers::{Connect, sqlite::*};

#[tokio::test]
async fn sqlite_execution() -> TestResult {
    let pool = SqliteConnectionPool::connect(&SqliteSettings {
        databases: Vec::new(),
        path: ":memory:".into(),
        busy_timeout_ms: 5000,
        params: Default::default(),
    })
    .await?;
    let request = QueryRequest::new(
        "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<2500) SELECT CAST(42 AS INTEGER) AS bound FROM n",
    );
    rows(&pool, request, 2500).await?;
    rows(&pool, QueryRequest::new("SELECT 42 WHERE false"), 0).await?;
    nulls(&pool, "SELECT CAST(NULL AS INTEGER)").await?;
    let (_, measured) = metrics(&pool, QueryRequest::new("SELECT 42")).await?;
    assert_eq!(measured.client.delivered_rows, 1);
    assert_eq!(measured.server, None);
    failures(&pool, "SELECT missing_execution_column", "SELECT 42").await
}

#[tokio::test]
async fn sqlite_bindings_scripts_and_empty_columns() -> TestResult {
    use quill_core::value::Value;
    use quill_providers::ConnectionPool;
    let pool = SqliteConnectionPool::connect(&SqliteSettings {
        path: ":memory:".into(),
        databases: Vec::new(),
        busy_timeout_ms: 5000,
        params: Default::default(),
    })
    .await?;
    assert!(pool.capabilities().positional_parameters);
    let mut request = QueryRequest::new("SELECT ? AS bound");
    request.bindings.positional.push(ParameterValue::Int64(42));
    rows(&pool, request, 1).await?;
    let empty =
        execution::single(&pool, "SELECT CAST(NULL AS INTEGER) AS value WHERE false").await?;
    assert_eq!(empty.columns.ok_or("empty results keep columns")?.len(), 1);
    let sets = execution::collect(
        &pool,
        QueryRequest::new(
            "CREATE TABLE t (id INTEGER, label TEXT, data BLOB); \
             INSERT INTO t VALUES (1, 'a', x'ff00'), (2, NULL, NULL); \
             SELECT * FROM t ORDER BY id",
        ),
    )
    .await?;
    assert_eq!(sets.len(), 3);
    assert_eq!(sets[1].affected_rows, Some(2));
    assert_eq!(
        sets[2].rows[0].to_vec(),
        [
            Value::Int(1),
            Value::Text("a".into()),
            Value::Bytes(vec![0xff, 0])
        ]
    );
    assert_eq!(sets[2].rows[1][1], Value::Null);
    Ok(())
}
