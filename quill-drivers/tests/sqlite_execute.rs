//! Execution integration tests for sqlite.
#[path = "support/execution.rs"]
mod execution;

use execution::{TestResult, failures, metrics, nulls, rows};
use quill_core::query::*;
use quill_drivers::{ConnectionPool, sqlite::*};

#[tokio::test]
async fn sqlite_execution() -> TestResult {
    let pool = SqliteConnectionPool::connect(&SqliteSettings {
        path: ":memory:".into(),
        params: Default::default(),
    })
    .await?;
    let mut request = QueryRequest::new(
        "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<2500) SELECT CAST(42 + ? AS INTEGER) AS bound FROM n",
    );
    request.bindings.positional.push(ParameterValue::Int64(0));
    rows(&pool, request, 2500).await?;
    rows(&pool, QueryRequest::new("SELECT 42 WHERE false"), 0).await?;
    nulls(&pool, "SELECT CAST(NULL AS INTEGER)").await?;
    let (_, measured) = metrics(&pool, QueryRequest::new("SELECT 42")).await?;
    assert_eq!(measured.client.delivered_rows, 1);
    assert_eq!(measured.server, None);
    failures(&pool, "SELECT missing_execution_column", "SELECT 42").await
}

#[tokio::test]
async fn mixed_storage_classes_fail_without_coercion() -> TestResult {
    use futures::TryStreamExt;
    let pool = SqliteConnectionPool::connect(&SqliteSettings {
        path: ":memory:".into(),
        params: Default::default(),
    })
    .await?;
    sqlx::query("CREATE TABLE mixed(value INTEGER)")
        .execute(pool.inner())
        .await?;
    sqlx::query("INSERT INTO mixed VALUES(42),('not an integer')")
        .execute(pool.inner())
        .await?;
    let mut execution = pool
        .execute(QueryRequest::new("SELECT value FROM mixed"))
        .await?;
    let mut failed = false;
    loop {
        match execution.events.try_next().await {
            Ok(Some(QueryEvent::Batch { .. })) => panic!("mixed values must not be coerced"),
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {
                failed = true;
                break;
            }
        }
    }
    assert!(failed);
    Ok(())
}
