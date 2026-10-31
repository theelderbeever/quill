//! Execution integration tests for scylla.
#[path = "support/execution.rs"]
mod execution;

use execution::{TestResult, failures, rows, tls};
use futures::StreamExt;
use quill_core::query::*;
use quill_drivers::{ConnectionPool, scylla::*};

#[tokio::test]
#[ignore = "requires Docker Compose Scylla"]
async fn scylla_execution() -> TestResult {
    let pool = ScyllaConnectionPool::connect(&ScyllaSettings {
        nodes: vec!["localhost:9042".into()],
        keyspace: None,
        user: None,
        password: None,
        datacenter: None,
        tls: tls(),
        params: Default::default(),
    })
    .await?;
    assert!(pool.capabilities().named_parameters && pool.capabilities().positional_parameters);
    pool.inner().query_unpaged("CREATE KEYSPACE IF NOT EXISTS quill_execution_test WITH replication = {'class':'SimpleStrategy','replication_factor':1}", ()).await?;
    pool.inner()
        .query_unpaged(
            "CREATE TABLE quill_execution_test.items (id bigint PRIMARY KEY, value bigint)",
            (),
        )
        .await?;
    let result = async {
        let mut request =
            QueryRequest::new("INSERT INTO quill_execution_test.items (id, value) VALUES (?, ?)");
        request.bindings.positional.push(ParameterValue::Int64(1));
        request
            .bindings
            .named
            .insert("value".into(), ParameterValue::Int64(42));
        let mut execution = pool.execute(request).await?;
        while let Some(event) = execution.events.next().await {
            event?;
        }
        assert_eq!(
            execution.control.snapshot().status,
            ExecutionStatus::Completed
        );
        let mut request =
            QueryRequest::new("SELECT value FROM quill_execution_test.items WHERE id = :id");
        request
            .bindings
            .named
            .insert("id".into(), ParameterValue::Int64(1));
        rows(&pool, request, 1).await?;
        let mut positional = QueryRequest::new("SELECT value FROM quill_execution_test.items WHERE id = ?");
        positional.bindings.positional.push(ParameterValue::Int64(1));
        rows(&pool, positional, 1).await?;
        let mut mixed = QueryRequest::new("SELECT value FROM quill_execution_test.items WHERE id = ? AND value = :value ALLOW FILTERING");
        mixed.bindings.positional.push(ParameterValue::Int64(1));
        mixed.bindings.named.insert("value".into(), ParameterValue::Int64(42));
        rows(&pool, mixed, 1).await?;
        rows(
            &pool,
            QueryRequest::new("SELECT value FROM quill_execution_test.items WHERE id = 2"),
            0,
        )
        .await?;
        for (sql, positional, named) in [
            ("SELECT value FROM quill_execution_test.items WHERE id = ? AND value = :value ALLOW FILTERING", vec![], vec![("value", 42)]),
            ("SELECT value FROM quill_execution_test.items WHERE id = ?", vec![1, 2], vec![]),
            ("SELECT value FROM quill_execution_test.items WHERE id = ?", vec![1], vec![("unknown", 42)]),
        ] {
            let mut request = QueryRequest::new(sql);
            request.bindings.positional = positional.into_iter().map(ParameterValue::Int64).collect();
            request.bindings.named = named.into_iter().map(|(name, value)| (name.into(), ParameterValue::Int64(value))).collect();
            let mut execution = pool.execute(request).await?;
            assert!(matches!(execution.events.next().await, Some(Err(quill_core::error::Error::QueryExecution(_)))));
            assert_eq!(execution.control.snapshot().status, ExecutionStatus::Failed);
        }
        failures(
            &pool,
            "SELECT missing_execution_column FROM quill_execution_test.items",
            "SELECT value FROM quill_execution_test.items",
        )
        .await
    }
    .await;
    pool.inner()
        .query_unpaged("DROP KEYSPACE quill_execution_test", ())
        .await?;
    result
}
