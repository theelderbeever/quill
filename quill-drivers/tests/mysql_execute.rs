//! Execution integration tests for mysql.
#[path = "support/execution.rs"]
mod execution;

use execution::{TestResult, failures, nulls, rows, tls};
use quill_core::query::*;
use quill_drivers::{ConnectionPool, mysql::*};

#[tokio::test]
#[ignore = "requires Docker Compose MySQL"]
async fn mysql_execution() -> TestResult {
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
    assert!(pool.capabilities().positional_parameters);
    assert!(!pool.capabilities().named_parameters);
    let mut request = QueryRequest::new(
        "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<50) SELECT CAST(? AS SIGNED) AS bound FROM n a CROSS JOIN n b",
    );
    request.bindings.positional.push(ParameterValue::Int64(42));
    rows(&pool, request, 2500).await?;
    nulls(&pool, "SELECT CAST(NULL AS SIGNED)").await?;
    rows(
        &pool,
        QueryRequest::new("SELECT CAST(42 AS SIGNED) WHERE false"),
        0,
    )
    .await?;
    failures(
        &pool,
        "SELECT missing_execution_column",
        "SELECT CAST(42 AS SIGNED)",
    )
    .await
}
