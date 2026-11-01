//! Execution integration tests for clickhouse.
#[path = "support/execution.rs"]
mod execution;

use execution::{TestResult, failures, metrics, nulls, rows, tls};
use quill_core::query::*;
use quill_drivers::{ConnectionPool, clickhouse::*};

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

#[tokio::test]
#[ignore = "requires Docker Compose ClickHouse"]
async fn clickhouse_server_metrics() -> TestResult {
    // The connect-time log probe must not mask authentication failures.
    let mut wrong_password = settings("quill");
    wrong_password.password = Some("wrong".into());
    assert!(
        ClickhouseConnectionPool::connect(&wrong_password)
            .await
            .is_err()
    );
    let pool = ClickhouseConnectionPool::connect(&settings("quill")).await?;
    let flush = || pool.inner().query("SYSTEM FLUSH LOGS").execute();
    let (select, measured) = metrics(
        &pool,
        QueryRequest::new("SELECT toInt64(42) FROM numbers(2500)"),
    )
    .await?;
    assert_eq!(measured.client.delivered_rows, 2500);
    flush().await?;
    let server = pool
        .execution_metrics(select)
        .await?
        .ok_or("flushed query log has the execution")?;
    assert!(server.read_rows.is_some_and(|rows| rows >= 2500));
    assert_eq!(server.result_rows, Some(2500));
    assert!(server.elapsed.is_some() && server.peak_memory_bytes.is_some());
    assert_eq!(pool.execution_metrics(QueryId::new()).await?, None);

    pool.inner()
        .query("CREATE OR REPLACE TABLE quill_metrics_insert (n UInt64) ENGINE = Memory")
        .execute()
        .await?;
    let result = async {
        let (insert, _) = metrics(
            &pool,
            QueryRequest::new("INSERT INTO quill_metrics_insert SELECT number FROM numbers(100)"),
        )
        .await?;
        flush().await?;
        let server = pool
            .execution_metrics(insert)
            .await?
            .ok_or("flushed query log has the insert")?;
        assert_eq!(server.written_rows, Some(100));
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    pool.inner()
        .query("DROP TABLE quill_metrics_insert")
        .execute()
        .await?;
    result?;

    pool.inner()
        .query("CREATE USER OR REPLACE quill_metrics_denied IDENTIFIED BY 'password'")
        .execute()
        .await?;
    let denied = async {
        let restricted =
            ClickhouseConnectionPool::connect(&settings("quill_metrics_denied")).await?;
        let outcome = restricted.execution_metrics(QueryId::new()).await;
        assert!(
            matches!(
                outcome,
                Err(quill_core::error::Error::ServerMetrics(
                    ServerMetricsError::PermissionDenied(_)
                ))
            ),
            "{outcome:?}"
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    pool.inner()
        .query("DROP USER quill_metrics_denied")
        .execute()
        .await?;
    denied
}

#[tokio::test]
#[ignore = "requires Docker Compose ClickHouse"]
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

    let mut cursor = pool.query_arrow("SELECT unhex('FF0080') AS bytes")?;
    let batch = cursor.next().await?.ok_or("expected a batch")?;
    assert_eq!(batch.schema().field(0).data_type(), &DataType::Binary);
    let values = batch
        .column(0)
        .as_any()
        .downcast_ref::<BinaryArray>()
        .unwrap();
    assert_eq!(values.value(0), &[0xff, 0x00, 0x80]);
    Ok(())
}
