//! Execution integration tests for datafusion.
#[path = "support/execution.rs"]
mod execution;

use execution::{TestResult, failures, nulls, rows};
use quill_core::query::*;
use quill_drivers::{ConnectionPool, datafusion::*};

#[tokio::test]
async fn datafusion_execution() -> TestResult {
    let pool = DatafusionConnectionPool::connect(&DatafusionSettings {
        object_stores: vec![],
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
    failures(
        &pool,
        "SELECT missing_execution_column",
        "SELECT CAST(42 AS BIGINT)",
    )
    .await
}
