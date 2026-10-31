//! Execution integration tests for clickhouse.
#[path = "support/execution.rs"]
mod execution;

use execution::{TestResult, failures, nulls, rows, tls};
use quill_core::query::*;
use quill_drivers::{ConnectionPool, clickhouse::*};

#[tokio::test]
#[ignore = "requires Docker Compose ClickHouse"]
async fn clickhouse_execution() -> TestResult {
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
    assert!(pool.capabilities().named_parameters && pool.capabilities().positional_parameters);
    let mut request =
        QueryRequest::new("SELECT CAST(? + {offset:Int64} AS Int64) AS bound FROM numbers(2500)");
    request.bindings.positional.push(ParameterValue::Int64(40));
    request
        .bindings
        .named
        .insert("offset".into(), ParameterValue::Int64(2));
    rows(&pool, request, 2500).await?;
    let mut positional =
        QueryRequest::new("SELECT CAST(? + ? AS Int64) AS bound FROM numbers(2500)");
    positional
        .bindings
        .positional
        .extend([ParameterValue::Int64(40), ParameterValue::Int64(2)]);
    rows(&pool, positional, 2500).await?;
    let mut named =
        QueryRequest::new("SELECT {base:Int64} + {offset:Int64} AS bound FROM numbers(2500)");
    named
        .bindings
        .named
        .insert("base".into(), ParameterValue::Int64(40));
    named
        .bindings
        .named
        .insert("offset".into(), ParameterValue::Int64(2));
    rows(&pool, named, 2500).await?;
    rows(
        &pool,
        QueryRequest::new("SELECT toInt64(42) WHERE false"),
        0,
    )
    .await?;
    nulls(&pool, "SELECT CAST(NULL AS Nullable(Int64))").await?;
    failures(
        &pool,
        "SELECT missing_execution_column",
        "SELECT toInt64(42) FROM numbers(100000)",
    )
    .await
}
