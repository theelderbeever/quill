//! Execution integration tests for sqlite.
#[path = "support/execution.rs"]
mod execution;

use execution::{TestResult, failures, metrics, nulls, rows};
use quill_core::query::*;
use quill_providers::{Connect, sqlite::*};

#[tokio::test]
async fn sqlite_execution() -> TestResult {
    let pool = SqliteConnectionPool::connect(&SqliteSettings {
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
