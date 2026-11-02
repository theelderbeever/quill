//! Execution integration tests for cassandra.
#[path = "support/execution.rs"]
mod execution;

use execution::{TestResult, failures, rows, tls};
use futures::StreamExt;
use quill_core::query::*;
use quill_providers::{Connect, ConnectionPool, cassandra::*};

async fn run(pool: &dyn ConnectionPool, sql: &str) -> TestResult {
    let mut execution = pool.execute(QueryRequest::new(sql)).await?;
    while let Some(event) = execution.events.next().await {
        event?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires Docker Compose Scylla"]
async fn cassandra_execution() -> TestResult {
    let pool = CassandraConnectionPool::connect(&CassandraSettings {
        nodes: vec!["localhost:9042".into()],
        keyspace: None,
        user: None,
        password: None,
        datacenter: None,
        tls: tls(),
        params: Default::default(),
    })
    .await?;
    run(&pool, "CREATE KEYSPACE IF NOT EXISTS quill_execution_test WITH replication = {'class':'NetworkTopologyStrategy','replication_factor':1}").await?;
    run(
        &pool,
        "CREATE TABLE IF NOT EXISTS quill_execution_test.items (id bigint PRIMARY KEY, value bigint)",
    )
    .await?;
    let result = async {
        run(
            &pool,
            "INSERT INTO quill_execution_test.items (id, value) VALUES (1, 42)",
        )
        .await?;
        let select = "SELECT value FROM quill_execution_test.items WHERE id = 1";
        rows(&pool, QueryRequest::new(select), 1).await?;
        let empty = "SELECT value FROM quill_execution_test.items WHERE id = 2";
        rows(&pool, QueryRequest::new(empty), 0).await?;
        failures(
            &pool,
            "SELECT missing FROM quill_execution_test.items",
            select,
        )
        .await
    }
    .await;
    run(&pool, "DROP KEYSPACE quill_execution_test").await?;
    result
}
