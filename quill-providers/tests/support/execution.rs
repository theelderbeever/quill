#![allow(dead_code)]
//! Server tests require the repository's Docker Compose services.
use std::sync::Arc;

use futures::StreamExt;
use quill_core::{
    query::*,
    value::{Column, Row, Value},
};
use quill_providers::ConnectionPool;

pub fn tls() -> Option<quill_providers::settings::TlsSettings> {
    std::env::var_os("QUILL_TEST_TLS").map(|_| quill_providers::settings::TlsSettings {
        ca_cert: Some(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../docker/tls/certs/ca.pem"),
        ),
        identity: None,
    })
}

pub type TestResult = Result<(), Box<dyn std::error::Error>>;

/// One collected result set.
#[derive(Debug, Default)]
pub struct Set {
    pub columns: Option<Arc<[Column]>>,
    pub rows: Vec<Row>,
    pub batches: usize,
    pub affected_rows: Option<u64>,
}

/// Run `request` to completion, checking event order.
pub async fn collect(
    pool: &dyn ConnectionPool,
    request: QueryRequest,
) -> Result<Vec<Set>, Box<dyn std::error::Error>> {
    let mut execution = pool.execute(request).await?;
    let mut sets: Vec<Set> = Vec::new();
    let mut open: Option<Set> = None;
    let mut complete = false;
    while let Some(event) = execution.events.next().await {
        assert!(!complete, "events after completion");
        match event? {
            QueryEvent::Columns {
                result_set,
                columns,
            } => {
                assert!(open.is_none());
                assert_eq!(result_set, sets.len());
                open = Some(Set {
                    columns: Some(columns),
                    ..Set::default()
                });
            }
            QueryEvent::Rows { result_set, batch } => {
                assert_eq!(result_set, sets.len());
                let set = open.as_mut().ok_or("rows before columns")?;
                assert_eq!(Some(batch.columns()), set.columns.as_ref());
                set.rows.extend(batch.into_rows());
                set.batches += 1;
            }
            QueryEvent::ResultSetComplete {
                result_set,
                affected_rows,
            } => {
                assert_eq!(result_set, sets.len());
                let mut set = open.take().unwrap_or_default();
                set.affected_rows = affected_rows;
                sets.push(set);
            }
            QueryEvent::Complete => complete = true,
        }
    }
    assert!(complete);
    let snapshot = execution.control.snapshot();
    assert_eq!(snapshot.status, ExecutionStatus::Completed);
    let rows: usize = sets.iter().map(|s| s.rows.len()).sum();
    assert_eq!(snapshot.metrics.delivered_rows, rows as u64);
    assert_eq!(snapshot.metrics.first_row.is_some(), rows > 0);
    Ok(sets)
}

/// The only result set of `sql`.
pub async fn single(
    pool: &dyn ConnectionPool,
    sql: &str,
) -> Result<Set, Box<dyn std::error::Error>> {
    let mut sets = collect(pool, QueryRequest::new(sql)).await?;
    assert_eq!(sets.len(), 1, "{sql}");
    Ok(sets.remove(0))
}

/// `request` yields `expected` rows of a single column holding 42.
pub async fn rows(pool: &dyn ConnectionPool, request: QueryRequest, expected: usize) -> TestResult {
    let sets = collect(pool, request).await?;
    assert_eq!(sets.len(), 1);
    let set = &sets[0];
    assert!(
        set.columns.is_some(),
        "result sets announce columns, even when empty"
    );
    assert_eq!(set.rows.len(), expected);
    for row in &set.rows {
        assert!(
            matches!(row[0], Value::Int(42) | Value::UInt(42)),
            "{:?}",
            row[0]
        );
    }
    Ok(())
}

/// Drain one successful execution and combine its client counters with
/// whatever server metrics the pool reports immediately afterwards.
pub async fn metrics(
    pool: &dyn ConnectionPool,
    request: QueryRequest,
) -> Result<(QueryId, QueryMetrics), Box<dyn std::error::Error>> {
    let mut execution = pool.execute(request).await?;
    while let Some(event) = execution.events.next().await {
        event?;
    }
    let snapshot = execution.control.snapshot();
    assert_eq!(snapshot.status, ExecutionStatus::Completed);
    let client = snapshot.metrics;
    if client.delivered_rows > 0 {
        assert!(
            client
                .first_row
                .is_some_and(|first| first <= client.elapsed)
        );
    }
    let server = pool.execution_metrics(execution.id).await?;
    Ok((execution.id, QueryMetrics { client, server }))
}

/// `sql` yields one row whose single cell is NULL.
pub async fn nulls(pool: &dyn ConnectionPool, sql: &str) -> TestResult {
    let set = single(pool, sql).await?;
    assert_eq!(set.rows.len(), 1);
    assert_eq!(set.rows[0].len(), 1);
    assert_eq!(set.rows[0][0], Value::Null);
    Ok(())
}

pub async fn failures(pool: &dyn ConnectionPool, invalid: &str, streaming: &str) -> TestResult {
    let mut execution = pool.execute(QueryRequest::new(invalid)).await?;
    let mut failed = false;
    while let Some(event) = execution.events.next().await {
        if event.is_err() {
            failed = true;
            break;
        }
    }
    assert!(failed);
    assert_eq!(execution.control.snapshot().status, ExecutionStatus::Failed);
    let mut execution = pool.execute(QueryRequest::new(streaming)).await?;
    assert!(matches!(
        execution.events.next().await.transpose()?,
        Some(QueryEvent::Columns { .. })
    ));
    execution.control.cancel();
    let control = execution.control.clone();
    let snapshot = control.snapshot();
    assert_eq!(snapshot.status, ExecutionStatus::Cancelled);
    drop(execution);
    tokio::task::yield_now().await;
    assert_eq!(control.snapshot(), snapshot);
    Ok(())
}
