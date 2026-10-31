#![allow(dead_code)]
//! Server tests require the repository's Docker Compose services.
use arrow::array::{Array, Int64Array};
use futures::StreamExt;
use quill_core::query::*;
use quill_drivers::ConnectionPool;
pub fn tls() -> Option<quill_drivers::settings::TlsSettings> {
    std::env::var_os("QUILL_TEST_TLS").map(|_| quill_drivers::settings::TlsSettings {
        ca_cert: Some(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../docker/tls/certs/ca.pem"),
        ),
        identity: None,
    })
}

pub type TestResult = Result<(), Box<dyn std::error::Error>>;

pub async fn rows(pool: &dyn ConnectionPool, request: QueryRequest, expected: usize) -> TestResult {
    let mut execution = pool.execute(request).await?;
    let mut schema = None;
    let mut count = 0;
    let mut complete = false;
    let mut result_complete = false;
    while let Some(event) = execution.events.next().await {
        match event? {
            QueryEvent::Schema { schema: value, .. } => {
                assert!(schema.is_none());
                schema = Some(value);
            }
            QueryEvent::Batch { batch, .. } => {
                assert_eq!(schema.as_ref(), Some(&batch.schema()));
                let values = batch
                    .column(0)
                    .as_any()
                    .downcast_ref::<Int64Array>()
                    .ok_or("expected Int64")?;
                for i in 0..values.len() {
                    assert!(!values.is_null(i));
                    assert_eq!(values.value(i), 42);
                }
                count += batch.num_rows();
            }
            QueryEvent::ResultSetComplete { .. } => result_complete = true,
            QueryEvent::Complete => complete = true,
        }
    }
    assert!(schema.is_some());
    assert!(complete && result_complete);
    assert_eq!(count, expected);
    let snapshot = execution.control.snapshot();
    assert_eq!(snapshot.status, ExecutionStatus::Completed);
    assert_eq!(snapshot.metrics.delivered_rows, expected as u64);
    Ok(())
}

pub async fn nulls(pool: &dyn ConnectionPool, sql: &str) -> TestResult {
    let mut execution = pool.execute(QueryRequest::new(sql)).await?;
    let mut schema = None;
    let mut count = 0;
    while let Some(event) = execution.events.next().await {
        match event? {
            QueryEvent::Schema { schema: value, .. } => schema = Some(value),
            QueryEvent::Batch { batch, .. } => {
                assert_eq!(schema.as_ref(), Some(&batch.schema()));
                assert_eq!(batch.num_columns(), 1);
                assert_eq!(batch.column(0).null_count(), batch.num_rows());
                count += batch.num_rows();
            }
            _ => {}
        }
    }
    assert_eq!(count, 1);
    assert_eq!(
        execution.control.snapshot().status,
        ExecutionStatus::Completed
    );
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
        Some(QueryEvent::Schema { .. })
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
