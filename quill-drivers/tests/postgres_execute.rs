//! Requires the Compose Postgres service.
use futures::StreamExt;
use quill_core::query::*;
use quill_drivers::{
    ConnectionPool,
    postgres::{PostgresConnectionPool, PostgresSettings},
};

async fn pool() -> quill_core::error::Result<PostgresConnectionPool> {
    PostgresConnectionPool::connect(&PostgresSettings {
        host: "localhost".into(),
        port: 5432,
        database: "quilt_test".into(),
        user: "quill".into(),
        password: Some("password".into()),
        tls: None,
        params: Default::default(),
    })
    .await
}

#[tokio::test]
#[ignore = "requires Docker Compose Postgres"]
async fn parameters_streaming_empty_results_and_commands()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let pool = pool().await?;
    let pool: &dyn ConnectionPool = &pool;
    assert!(pool.capabilities().positional_parameters);
    let mut request = QueryRequest::new("SELECT $1 AS bound, n FROM generate_series(1, 2500) n");
    request.bindings.positional.push(ParameterValue::Int64(42));
    let mut execution = pool.execute(request).await?;
    let mut rows = 0;
    let mut batches = 0;
    let mut schema_seen = false;
    while let Some(event) = execution.events.next().await {
        match event? {
            QueryEvent::Schema { schema, .. } => {
                assert_eq!(
                    schema.field(0).data_type(),
                    &arrow::datatypes::DataType::Int64
                );
                schema_seen = true;
            }
            QueryEvent::Batch { batch, .. } => {
                assert!(schema_seen);
                rows += batch.num_rows();
                batches += 1;
            }
            _ => {}
        }
    }
    assert_eq!(rows, 2500);
    assert!(batches > 1);
    let snapshot = execution.control.snapshot();
    assert_eq!(snapshot.status, ExecutionStatus::Completed);
    assert_eq!(snapshot.metrics.delivered_rows, 2500);
    assert!(
        snapshot
            .metrics
            .first_row
            .is_some_and(|first| first <= snapshot.metrics.elapsed)
    );
    assert_eq!(pool.execution_metrics(execution.id).await?, None);
    for (sql, has_schema) in [
        ("SELECT 1 WHERE false", true),
        ("CREATE TEMP TABLE execution_fixture (id int)", false),
    ] {
        let mut execution = pool.execute(QueryRequest::new(sql)).await?;
        let mut schemas = 0;
        let mut complete = false;
        while let Some(event) = execution.events.next().await {
            match event? {
                QueryEvent::Schema { .. } => schemas += 1,
                QueryEvent::Complete => complete = true,
                _ => {}
            }
        }
        assert_eq!(schemas, usize::from(has_schema));
        assert!(complete);
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires Docker Compose Postgres"]
async fn failures_and_cancellation_remain_observable()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let pool = pool().await?;
    let mut execution = pool
        .execute(QueryRequest::new("SELECT missing_execution_column"))
        .await?;
    let Some(Err(error)) = execution.events.next().await else {
        panic!("invalid SQL must fail")
    };
    let quill_core::error::Error::QueryExecution(source) = error else {
        panic!("expected a typed query error")
    };
    assert!(
        matches!(source.downcast_ref::<quill_drivers::postgres::ExecutionError>(), Some(quill_drivers::postgres::ExecutionError::Database(sqlx::Error::Database(error))) if error.code().as_deref() == Some("42703"))
    );
    assert_eq!(execution.control.snapshot().status, ExecutionStatus::Failed);
    let execution = pool
        .execute(QueryRequest::new("SELECT pg_sleep(30)"))
        .await?;
    let control = execution.control.clone();
    control.cancel();
    let snapshot = control.snapshot();
    assert_eq!(snapshot.status, ExecutionStatus::Cancelled);
    drop(execution);
    assert_eq!(control.snapshot(), snapshot);
    let mut streaming = pool
        .execute(QueryRequest::new(
            "SELECT n FROM generate_series(1, 100000) n",
        ))
        .await?;
    assert!(matches!(
        streaming.events.next().await.transpose()?,
        Some(QueryEvent::Schema { .. })
    ));
    streaming.control.cancel();
    assert_eq!(
        streaming.control.snapshot().status,
        ExecutionStatus::Cancelled
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires Docker Compose Postgres"]
async fn converts_native_result_types_and_nulls()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let pool = pool().await?;
    let mut execution = pool
        .execute(QueryRequest::new(
            "SELECT true AS flag, 2::smallint AS small, 1.5::real AS real_value, \
         2.5::double precision AS double_value, 'hello'::text AS text_value, \
         decode('abcd', 'hex') AS bytes, DATE '2026-01-01' AS day, \
         TIME '12:34:56' AS clock, TIMESTAMP '2026-01-01 12:34:56' AS local_time, \
         TIMESTAMPTZ '2026-01-01 12:34:56+00' AS instant, \
         '00000000-0000-0000-0000-000000000001'::uuid AS identifier, \
         'US'::char(2) AS country, 'abc'::varchar(5) AS short_text, \
         NULL::text AS missing",
        ))
        .await?;
    let mut batches = 0;
    while let Some(event) = execution.events.next().await {
        if let QueryEvent::Batch { batch, .. } = event? {
            assert_eq!(batch.num_rows(), 1);
            assert_eq!(batch.num_columns(), 14);
            let country = batch
                .column(11)
                .as_any()
                .downcast_ref::<arrow::array::StringArray>();
            assert_eq!(country.map(|c| c.value(0)), Some("US"));
            assert_eq!(batch.column(13).null_count(), 1);
            batches += 1;
        }
    }
    assert_eq!(batches, 1);
    Ok(())
}

#[tokio::test]
#[ignore = "requires Docker Compose Postgres"]
async fn decimal_schemas_and_coefficients_remain_exact_across_batches()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    use arrow::{
        array::{Array, Decimal128Array, Decimal256Array},
        datatypes::DataType,
    };
    let pool = pool().await?;
    sqlx::query("CREATE TABLE quill_execution_decimals (id int, small numeric(10,2), wide numeric(60,8), negative_scale numeric(2,-3), fractional numeric(3,5), unconstrained numeric, excessive numeric(77,0))").execute(pool.inner()).await?;
    let result = async {
        sqlx::query("INSERT INTO quill_execution_decimals VALUES (1,12.34,1234567890123456789012345678901234567890.12345678,99000,0.00123,1,1), (2,-12.34,-1234567890123456789012345678901234567890.12345678,-99000,-0.00123,1,1), (3,0,0,0,0,1,1), (4,NULL,NULL,NULL,NULL,NULL,NULL)").execute(pool.inner()).await?;
        let mut execution = pool.execute(QueryRequest::new("SELECT small, wide, negative_scale, fractional FROM quill_execution_decimals CROSS JOIN generate_series(1,600) n ORDER BY n,id")).await?;
        let expected = [DataType::Decimal128(10,2), DataType::Decimal256(60,8), DataType::Decimal128(2,-3), DataType::Decimal128(5,5)];
        let mut rows = 0;
        let mut batches = 0;
        while let Some(event) = execution.events.next().await {
            match event? {
                QueryEvent::Schema { schema, .. } => {
                    for (field, expected) in schema.fields().iter().zip(&expected) { assert_eq!(field.data_type(), expected); }
                }
                QueryEvent::Batch { batch, .. } => {
                    let small = batch.column(0).as_any().downcast_ref::<Decimal128Array>().ok_or("expected Decimal128")?;
                    let wide = batch.column(1).as_any().downcast_ref::<Decimal256Array>().ok_or("expected Decimal256")?;
                    let negative = batch.column(2).as_any().downcast_ref::<Decimal128Array>().ok_or("expected negative scale decimal")?;
                    let fractional = batch.column(3).as_any().downcast_ref::<Decimal128Array>().ok_or("expected fractional decimal")?;
                    for i in 0..batch.num_rows() {
                        let position = rows % 4;
                        if position == 3 {
                            for column in batch.columns() { assert!(column.is_null(i)); }
                        } else {
                            let sign = [1, -1, 0][position];
                            assert_eq!(small.value(i), sign * 1234);
                            assert_eq!(negative.value(i), sign * 99);
                            assert_eq!(fractional.value(i), sign * 123);
                            let coefficient: arrow::datatypes::i256 = "123456789012345678901234567890123456789012345678".parse()?;
                            assert_eq!(wide.value(i), coefficient * arrow::datatypes::i256::from_i128(sign));
                        }
                        rows += 1;
                    }
                    batches += 1;
                }
                _ => {}
            }
        }
        assert_eq!(rows, 2400);
        assert!(batches > 1);
        for sql in ["SELECT excessive FROM quill_execution_decimals"] {
            let mut execution = pool.execute(QueryRequest::new(sql)).await?;
            assert!(execution.events.next().await.transpose().is_err());
        }
        for sql in ["SELECT unconstrained FROM quill_execution_decimals ORDER BY id", "SELECT 1.23::numeric(10,2)"] {
            let mut execution = pool.execute(QueryRequest::new(sql)).await?;
            let mut saw_batch = false;
            while let Some(event) = execution.events.next().await {
                match event? {
                    QueryEvent::Schema { schema, .. } => {
                        assert_eq!(schema.field(0).data_type(), &DataType::Utf8);
                        assert_numeric_text_metadata(schema.field(0));
                    },
                    QueryEvent::Batch { batch, .. } => {
                        assert_eq!(batch.column(0).data_type(), &DataType::Utf8);
                        assert_numeric_text_metadata(batch.schema().field(0));
                        saw_batch = true;
                    }
                    _ => {}
                }
            }
            assert!(saw_batch);
        }
        sqlx::query("UPDATE quill_execution_decimals SET small = 'NaN' WHERE id = 1").execute(pool.inner()).await?;
        let mut execution = pool.execute(QueryRequest::new("SELECT small FROM quill_execution_decimals WHERE id = 1")).await?;
        let mut failed = false;
        while let Some(event) = execution.events.next().await { if event.is_err() { failed = true; break; } }
        assert!(failed);
        Ok::<_, Box<dyn std::error::Error>>(())
    }.await;
    sqlx::query("DROP TABLE quill_execution_decimals")
        .execute(pool.inner())
        .await?;
    result
}

fn assert_numeric_text_metadata(field: &arrow::datatypes::Field) {
    for (key, value) in [
        ("quill.native_type", "numeric"),
        ("quill.logical_type", "decimal"),
        ("quill.encoding", "text"),
    ] {
        assert_eq!(field.metadata().get(key).map(String::as_str), Some(value));
    }
}

#[tokio::test]
#[ignore = "requires Docker Compose Postgres"]
async fn array_values_and_nulls() -> std::result::Result<(), Box<dyn std::error::Error>> {
    use arrow::{
        array::{Array, Int32Array, ListArray, StringArray},
        datatypes::{DataType, Field},
    };
    use std::sync::Arc;
    let pool = pool().await?;
    let mut execution = pool.execute(QueryRequest::new(
        "SELECT CASE n % 3 WHEN 0 THEN NULL::text[] WHEN 1 THEN ARRAY[]::text[] ELSE ARRAY['hello', NULL, 'NULL', 'a,b', '☃'] END AS texts, ARRAY[1, NULL, 3]::int4[] AS ints FROM generate_series(0, 2499) n"
    )).await?;
    let mut schema = None;
    let mut rows = 0;
    let mut batches = 0;
    while let Some(event) = execution.events.next().await {
        match event? {
            QueryEvent::Schema { schema: value, .. } => {
                assert_eq!(
                    value.field(0).data_type(),
                    &DataType::List(Arc::new(Field::new("item", DataType::Utf8, true)))
                );
                assert!(value.field(0).metadata().is_empty());
                schema = Some(value);
            }
            QueryEvent::Batch { batch, .. } => {
                assert_eq!(Some(batch.schema()), schema);
                let texts = batch
                    .column(0)
                    .as_any()
                    .downcast_ref::<ListArray>()
                    .unwrap();
                let ints = batch
                    .column(1)
                    .as_any()
                    .downcast_ref::<ListArray>()
                    .unwrap();
                for i in 0..batch.num_rows() {
                    match (rows + i) % 3 {
                        0 => assert!(texts.is_null(i)),
                        1 => {
                            assert!(!texts.is_null(i));
                            assert_eq!(texts.value(i).len(), 0);
                        }
                        _ => {
                            let values = texts.value(i);
                            let values = values.as_any().downcast_ref::<StringArray>().unwrap();
                            assert_eq!(
                                values.iter().collect::<Vec<_>>(),
                                vec![Some("hello"), None, Some("NULL"), Some("a,b"), Some("☃")]
                            );
                        }
                    }
                    let values = ints.value(i);
                    let values = values.as_any().downcast_ref::<Int32Array>().unwrap();
                    assert_eq!(
                        values.iter().collect::<Vec<_>>(),
                        vec![Some(1), None, Some(3)]
                    );
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
