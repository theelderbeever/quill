//! Requires the Compose Postgres service.
#[path = "support/execution.rs"]
mod execution;

use execution::{TestResult, collect, single};
use quill_core::{query::*, value::Value};
use quill_providers::{
    Connect, ConnectionPool,
    postgres::{PostgresConnectionPool, PostgresSettings},
};

async fn pool() -> quill_core::error::Result<PostgresConnectionPool> {
    PostgresConnectionPool::connect(&PostgresSettings {
        sslmode: quill_providers::postgres::SslMode::Disable,
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
async fn streams_batches_and_reports_empty_results_and_commands() -> TestResult {
    let pool = pool().await?;
    let pool: &dyn ConnectionPool = &pool;
    let set = single(
        pool,
        "SELECT 42::bigint AS bound, n FROM generate_series(1, 10000) n",
    )
    .await?;
    assert_eq!(set.rows.len(), 10000);
    assert!(set.batches > 1);
    assert_eq!(set.rows[0][0], Value::Int(42));
    let columns = set.columns.ok_or("columns")?;
    assert_eq!(columns[0].native_type, "int8");

    let empty = single(pool, "SELECT 1 AS one WHERE false").await?;
    assert_eq!(
        empty.columns.ok_or("empty results keep columns")?[0].name,
        "one"
    );
    assert!(empty.rows.is_empty());

    let command = single(pool, "CREATE TEMP TABLE execution_fixture (id int)").await?;
    assert!(command.columns.is_none());
    Ok(())
}

#[tokio::test]
#[ignore = "requires Docker Compose Postgres"]
async fn scripts_yield_one_result_per_statement() -> TestResult {
    let pool = pool().await?;
    let sets = collect(
        &pool,
        QueryRequest::new(
            "CREATE TEMP TABLE script_fixture (id int); \
             INSERT INTO script_fixture VALUES (1), (2); \
             SELECT id FROM script_fixture ORDER BY id; \
             SELECT 'two'",
        ),
    )
    .await?;
    assert_eq!(sets.len(), 4);
    assert_eq!(sets[1].affected_rows, Some(2));
    assert_eq!(sets[2].rows.len(), 2);
    assert_eq!(sets[3].rows[0][0], Value::Text("two".into()));
    Ok(())
}

#[tokio::test]
#[ignore = "requires Docker Compose Postgres"]
async fn binds_positional_parameters() -> TestResult {
    let pool = pool().await?;
    let mut request = QueryRequest::new("SELECT $1::bigint + 1, $2::text, $3::numeric");
    request.bindings.positional.push(ParameterValue::Int64(41));
    request
        .bindings
        .positional
        .push(ParameterValue::Text("x".into()));
    request
        .bindings
        .positional
        .push(ParameterValue::UInt64(u64::MAX));
    let sets = collect(&pool, request).await?;
    assert_eq!(
        sets[0].rows[0].to_vec(),
        [
            Value::Int(42),
            Value::Text("x".into()),
            Value::decimal_or_other("18446744073709551615"),
        ]
    );
    let mut named = QueryRequest::new("SELECT 1");
    named
        .bindings
        .named
        .insert("x".into(), ParameterValue::Int64(1));
    let mut execution = pool.execute(named).await?;
    let mut failed = false;
    while let Some(event) = futures::StreamExt::next(&mut execution.events).await {
        failed |= event.is_err();
    }
    assert!(failed, "named parameters are unsupported");
    Ok(())
}

#[tokio::test]
#[ignore = "requires Docker Compose Postgres"]
async fn failures_and_server_cancellation() -> TestResult {
    let pool = pool().await?;
    execution::failures(
        &pool,
        "SELECT missing_execution_column",
        "SELECT n FROM generate_series(1, 100000000) n",
    )
    .await?;
    let execution = pool
        .execute(QueryRequest::new("SELECT 1 FROM pg_sleep(30)"))
        .await?;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let started = std::time::Instant::now();
    execution.control.cancel();
    drop(execution);
    // The cancelled backend is released and the pool stays usable.
    single(&pool, "SELECT 1").await?;
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    Ok(())
}

#[tokio::test]
#[ignore = "requires Docker Compose Postgres"]
async fn decodes_every_type_without_failing() -> TestResult {
    let pool = pool().await?;
    for bound in [false, true] {
        let sql = "SELECT true, 2::smallint, 1.5::real, 2.5::double precision, \
             'hello'::text, decode('abcd', 'hex'), DATE '2026-01-01', TIME '12:34:56', \
             TIMESTAMP '2026-01-01 12:34:56', TIMESTAMPTZ '2026-01-01 12:34:56+00', \
             '00000000-0000-0000-0000-000000000001'::uuid, 'US'::char(2), NULL::text, \
             1234567890123456789012345678901234567890.12345678::numeric(60,8), \
             'NaN'::numeric, '12:00:00+02'::timetz, ARRAY[1, NULL, 3]::int4[], \
             ARRAY['a', NULL]::varchar[], '{\"b\": 1}'::jsonb, pg_sleep(0), \
             '1 day'::interval, '10.0.0.1'::inet";
        let mut request = QueryRequest::new(if bound {
            format!("{sql} WHERE $1")
        } else {
            sql.to_owned()
        });
        if bound {
            request
                .bindings
                .positional
                .push(ParameterValue::Boolean(true));
        }
        let sets = collect(&pool, request).await?;
        let row = &sets[0].rows[0];
        assert_eq!(row[0], Value::Bool(true));
        assert_eq!(row[1], Value::Int(2));
        assert_eq!(row[4], Value::Text("hello".into()));
        assert_eq!(row[5], Value::Bytes(vec![0xab, 0xcd]));
        assert_eq!(row[11], Value::Text("US".into()));
        assert_eq!(row[12], Value::Null);
        assert_eq!(
            row[13].to_string(),
            "1234567890123456789012345678901234567890.12345678"
        );
        assert_eq!(
            row[16],
            Value::List(vec![Value::Int(1), Value::Null, Value::Int(3)])
        );
        assert_eq!(
            row[17],
            Value::List(vec![Value::Text("a".into()), Value::Null])
        );
        assert_eq!(row[19], Value::Null);
        if !bound {
            assert_eq!(row[14], Value::Other("NaN".into()));
            assert_eq!(row[15], Value::Other("12:00:00+02".into()));
            assert_eq!(row[20], Value::Other("1 day".into()));
            assert_eq!(row[21], Value::Other("10.0.0.1".into()));
        }
    }
    Ok(())
}
