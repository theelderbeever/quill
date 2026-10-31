//! Per-execution measurements, split by who observes them.
//!
//! Client metrics are live and read from execution control; server metrics are
//! looked up from the pool after submission and may arrive late. Unknown
//! measurements are absent rather than zero.
use std::{net::SocketAddr, time::Duration};

/// Measurements Quill observes while driving one execution.
#[derive(Debug, Clone, PartialEq)]
pub struct ClientMetrics {
    /// Quill wall time. Continues through streaming and freezes on any terminal status.
    pub elapsed: Duration,
    /// Time from submission to the first non-empty batch. Absent for rowless results.
    pub first_row: Option<Duration>,
    /// Rows handed to the consumer.
    pub delivered_rows: u64,
    pub delivered_batches: u64,
    pub affected_rows: Option<u64>,
    /// Retry and routing history, for drivers that expose it.
    pub driver: Option<DriverHistory>,
}

impl ClientMetrics {
    /// Counters for an execution that has just been submitted.
    ///
    /// ```
    /// use quill_core::query::ClientMetrics;
    /// assert_eq!(ClientMetrics::started().first_row, None);
    /// ```
    pub const fn started() -> Self {
        Self {
            elapsed: Duration::ZERO,
            first_row: None,
            delivered_rows: 0,
            delivered_batches: 0,
            affected_rows: None,
            driver: None,
        }
    }
}

/// Driver-side request history: pages, attempts, and the nodes that served them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverHistory {
    /// Requests sent; one per page for paged drivers.
    pub requests: u32,
    /// Attempts across all requests, including retries and speculative executions.
    pub attempts: u32,
    pub speculative_executions: u32,
    /// Distinct nodes that received an attempt, in first-contact order.
    pub nodes: Vec<SocketAddr>,
}

/// Engine measurements retrieved for one execution after submission.
/// Absence of the whole record may mean the server log has not been flushed yet.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerMetrics {
    pub elapsed: Option<Duration>,
    pub read_rows: Option<u64>,
    pub read_bytes: Option<u64>,
    pub written_rows: Option<u64>,
    pub written_bytes: Option<u64>,
    pub result_rows: Option<u64>,
    pub result_bytes: Option<u64>,
    pub peak_memory_bytes: Option<u64>,
    pub spilled_bytes: Option<u64>,
    /// Operator compute time summed across partitions; not wall time.
    pub compute_time: Option<Duration>,
}

impl ServerMetrics {
    /// A record with every measurement absent, for drivers to fill selectively.
    ///
    /// ```
    /// use quill_core::query::ServerMetrics;
    /// let metrics = ServerMetrics { read_rows: Some(10), ..ServerMetrics::unknown() };
    /// assert_eq!(metrics.elapsed, None);
    /// ```
    pub const fn unknown() -> Self {
        Self {
            elapsed: None,
            read_rows: None,
            read_bytes: None,
            written_rows: None,
            written_bytes: None,
            result_rows: None,
            result_bytes: None,
            peak_memory_bytes: None,
            spilled_bytes: None,
            compute_time: None,
        }
    }
}

/// Merged view of one execution, assembled by consumers from
/// `ExecutionControl::snapshot` and `ConnectionPool::execution_metrics`.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryMetrics {
    pub client: ClientMetrics,
    pub server: Option<ServerMetrics>,
}

/// Why server metrics could not be retrieved.
#[derive(Debug, thiserror::Error)]
pub enum ServerMetricsError {
    #[error("not permitted to read server metrics")]
    PermissionDenied(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("server metrics are unavailable")]
    Unavailable(#[source] Box<dyn std::error::Error + Send + Sync>),
}
