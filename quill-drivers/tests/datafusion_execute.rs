//! Execution integration tests for datafusion.
#[path = "support/execution.rs"]
mod execution;

use execution::{TestResult, failures, metrics, nulls, rows};
use quill_core::query::*;
use quill_drivers::{ConnectionPool, datafusion::*};

#[tokio::test]
async fn datafusion_execution() -> TestResult {
    let pool = DatafusionConnectionPool::connect(&DatafusionSettings {
        object_stores: vec![],
        retained_metrics: 2,
    })
    .await?;
    assert!(!pool.capabilities().named_parameters && !pool.capabilities().positional_parameters);
    rows(
        &pool,
        QueryRequest::new("SELECT CAST(42 AS BIGINT) AS bound FROM range(2500)"),
        2500,
    )
    .await?;
    rows(
        &pool,
        QueryRequest::new("SELECT CAST(42 AS BIGINT) WHERE false"),
        0,
    )
    .await?;
    nulls(&pool, "SELECT CAST(NULL AS BIGINT)").await?;
    let (id, measured) = metrics(
        &pool,
        QueryRequest::new("SELECT CAST(42 AS BIGINT) AS bound FROM range(2500)"),
    )
    .await?;
    let server = measured.server.ok_or("DataFusion records plan metrics")?;
    assert_eq!(server.result_rows, Some(2500));
    assert!(server.compute_time.is_some());
    assert_eq!(pool.execution_metrics(id).await?, Some(server));
    assert_eq!(pool.execution_metrics(QueryId::new()).await?, None);
    failures(
        &pool,
        "SELECT missing_execution_column",
        "SELECT CAST(42 AS BIGINT)",
    )
    .await
}
