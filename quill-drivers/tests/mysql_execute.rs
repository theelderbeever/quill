//! Execution integration tests for mysql.
#[path = "support/execution.rs"]
mod execution;

use execution::{TestResult, failures, metrics, nulls, rows, tls};
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
    let (_, measured) = metrics(&pool, QueryRequest::new("SELECT CAST(42 AS SIGNED)")).await?;
    assert_eq!(measured.client.delivered_rows, 1);
    assert_eq!(measured.server, None);
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

#[tokio::test]
#[ignore = "requires Docker Compose MySQL"]
async fn native_decimal_json_and_signed_time() -> TestResult {
    use arrow::array::{Array, DurationMicrosecondArray, StringArray};
    use futures::TryStreamExt;
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
    let mut execution = pool.execute(QueryRequest::new("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<600) SELECT CAST('12345678901234567890.12345678901234567890' AS DECIMAL(40,20)), CAST('-838:59:59' AS TIME), CAST('{\"n\":12345678901234567890}' AS JSON), CAST(NULL AS DECIMAL(40,20)) FROM n a CROSS JOIN (SELECT 1 UNION ALL SELECT 2) b")).await?;
    let mut schema = None;
    let mut count = 0;
    let mut batches = 0;
    while let Some(event) = execution.events.try_next().await? {
        match event {
            QueryEvent::Schema { schema: value, .. } => schema = Some(value),
            QueryEvent::Batch { batch, .. } => {
                assert_eq!(Some(batch.schema()), schema);
                let values = batch
                    .column(0)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .ok_or("decimal array")?;
                let times = batch
                    .column(1)
                    .as_any()
                    .downcast_ref::<DurationMicrosecondArray>()
                    .ok_or("duration array")?;
                for row in 0..batch.num_rows() {
                    assert_eq!(
                        values.value(row),
                        "12345678901234567890.12345678901234567890"
                    );
                    assert_eq!(times.value(row), -3_020_399_000_000);
                    let json = batch
                        .column(2)
                        .as_any()
                        .downcast_ref::<StringArray>()
                        .ok_or("json array")?;
                    assert!(json.value(row).contains("12345678901234567890"));
                    assert!(batch.column(3).is_null(row));
                }
                count += batch.num_rows();
                batches += 1;
            }
            _ => {}
        }
    }
    assert_eq!(count, 1200);
    assert!(batches > 1);
    assert_eq!(
        schema.ok_or("schema")?.field(0).metadata()["quill.logical_type"],
        "decimal"
    );
    Ok(())
}
