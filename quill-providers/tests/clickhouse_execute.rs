//! Execution integration tests for clickhouse.
#[path = "support/execution.rs"]
mod execution;

use execution::{TestResult, failures, nulls, rows, tls};
use quill_core::query::*;
use quill_providers::{Connect, ConnectionPool, clickhouse::*};

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
#[ignore = "upstream provider decodes String as Utf8 and fails on arbitrary bytes"]
async fn strings_preserve_arbitrary_bytes() -> TestResult {
    use arrow::{
        array::{Array, BinaryArray},
        datatypes::DataType,
    };
    use futures::StreamExt;

    let pool = ClickhouseConnectionPool::connect(&settings("quill")).await?;
    let sql = "SELECT if(number % 3 = 0, NULL, if(number % 3 = 1, unhex('FF0080'), 'hello')) AS bytes FROM numbers(2500) SETTINGS max_block_size = 1000";
    let mut execution = pool.execute(QueryRequest::new(sql)).await?;
    let mut schema = None;
    let mut rows = 0;
    let mut batches = 0;
    while let Some(event) = execution.events.next().await {
        match event? {
            QueryEvent::Schema { schema: value, .. } => {
                assert_eq!(value.field(0).data_type(), &DataType::Binary);
                assert!(value.field(0).is_nullable());
                schema = Some(value);
            }
            QueryEvent::Batch { batch, .. } => {
                assert_eq!(Some(batch.schema()), schema);
                let values = batch
                    .column(0)
                    .as_any()
                    .downcast_ref::<BinaryArray>()
                    .unwrap();
                for i in 0..batch.num_rows() {
                    match (rows + i) % 3 {
                        0 => assert!(values.is_null(i)),
                        1 => assert_eq!(values.value(i), &[0xff, 0x00, 0x80]),
                        _ => assert_eq!(values.value(i), b"hello"),
                    }
                }
                rows += batch.num_rows();
                batches += 1;
            }
            _ => {}
        }
    }
    assert_eq!(rows, 2500);
    assert!(batches > 1);

    Ok(())
}
