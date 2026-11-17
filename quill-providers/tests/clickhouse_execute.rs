//! Execution integration tests for clickhouse.
#[path = "support/execution.rs"]
mod execution;

use execution::{TestResult, failures, nulls, rows, tls};
use quill_core::query::*;
use quill_providers::{Connect, clickhouse::*};

fn settings(user: &str) -> ClickhouseSettings {
    ClickhouseSettings {
        host: if tls().is_some() {
            "localhost:8443"
        } else {
            "localhost:8123"
        }
        .into(),
        database: "default".into(),
        user: Some(user.into()),
        password: Some("password".into()),
        tls: tls(),
        params: Default::default(),
    }
}

#[tokio::test]
#[ignore = "requires Docker Compose ClickHouse"]
async fn clickhouse_execution() -> TestResult {
    let pool = ClickhouseConnectionPool::connect(&settings("quill")).await?;
    rows(
        &pool,
        QueryRequest::new("SELECT toInt64(42) AS bound FROM numbers(2500)"),
        2500,
    )
    .await?;
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

#[tokio::test]
#[ignore = "requires Docker Compose ClickHouse"]
async fn strings_preserve_arbitrary_bytes() -> TestResult {
    use quill_core::value::Value;
    let pool = ClickhouseConnectionPool::connect(&settings("quill")).await?;
    let sql = "SELECT if(number % 3 = 0, NULL, if(number % 3 = 1, unhex('FF0080'), 'hello')) AS bytes, \
               toDecimal256('123456789012345678901234567890.5', 2) AS wide, [1, 2] AS list \
               FROM numbers(2500) SETTINGS max_block_size = 1000";
    let set = execution::single(&pool, sql).await?;
    assert_eq!(set.rows.len(), 2500);
    for (i, row) in set.rows.iter().enumerate() {
        match i % 3 {
            0 => assert_eq!(row[0], Value::Null),
            1 => assert_eq!(row[0], Value::Bytes(vec![0xff, 0x00, 0x80])),
            _ => assert_eq!(row[0], Value::Text("hello".into())),
        }
    }
    assert_eq!(
        set.rows[0][1].to_string(),
        "123456789012345678901234567890.50"
    );
    assert_eq!(set.rows[0][2], Value::Other("[1,2]".into()));
    Ok(())
}
