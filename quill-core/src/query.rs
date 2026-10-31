use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// In-process handle for a running query. Stateless to allocate (no shared
/// counter), unique across all engine instances. Not persisted — history
/// records the SQL text and timestamps, not this id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct QueryId(Uuid);

impl QueryId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub const fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for QueryId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for QueryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// A value passed to the driver's native binding API.
///
/// Values are deliberately not serializable or printable: bindings may contain
/// credentials or other sensitive query data.
pub enum ParameterValue {
    Null,
    Boolean(bool),
    Int64(i64),
    UInt64(u64),
    Float64(f64),
    Text(String),
    Binary(Vec<u8>),
    Date(chrono::NaiveDate),
    Time(chrono::NaiveTime),
    Timestamp(chrono::NaiveDateTime),
    TimestampWithTimezone(chrono::DateTime<chrono::FixedOffset>),
    Uuid(Uuid),
}

/// Both binding styles may coexist. Positional values use native driver order;
/// names are passed unchanged to the native client, without placeholder parsing.
/// Empty bindings represent a parameterless query.
#[derive(Default)]
pub struct Bindings {
    pub positional: Vec<ParameterValue>,
    pub named: std::collections::BTreeMap<String, ParameterValue>,
}

impl Bindings {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Owned query input so driver tasks can retain it without cloning.
///
/// ```
/// use quill_core::query::{QueryRequest, ParameterValue};
/// let mut request = QueryRequest::new("SELECT ? + :offset");
/// request.bindings.positional.push(ParameterValue::Int64(1));
/// request.bindings.named.insert("offset".into(), ParameterValue::Int64(2));
/// assert_eq!(request.bindings.positional.len(), 1);
/// assert_eq!(request.bindings.named.len(), 1);
/// ```
pub struct QueryRequest {
    pub sql: String,
    pub bindings: Bindings,
}

impl QueryRequest {
    /// Preserve SQL exactly as supplied, with initially empty bindings.
    pub fn new(sql: impl Into<String>) -> Self {
        Self {
            sql: sql.into(),
            bindings: Bindings::new(),
        }
    }
}

/// Events are ordered: schema, zero or more batches, then result-set completion.
/// Each result set has a zero-based index, allowing multiple sets per query.
/// A rowless statement emits `ResultSetComplete` without a schema or batches.
/// `Complete` terminates successful execution; an error terminates failed execution.
#[derive(Debug)]
pub enum QueryEvent {
    Schema {
        result_set: usize,
        schema: arrow::datatypes::SchemaRef,
    },
    Batch {
        result_set: usize,
        batch: arrow::record_batch::RecordBatch,
    },
    ResultSetComplete {
        result_set: usize,
        affected_rows: Option<u64>,
    },
    Complete,
}

/// Owned, incremental results. Errors preserve the native error as their source.
/// Cancellation may close this stream without a terminal event; consult control.
pub type QueryStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = crate::error::Result<QueryEvent>> + Send + 'static>,
>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// Per-execution counters. Unknown measurements are absent rather than zero.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionMetrics {
    /// Quill wall time, measured using `Instant::now()` and `start.elapsed()`.
    /// Continues through streaming and freezes on any terminal status.
    pub elapsed: std::time::Duration,
    pub delivered_rows: u64,
    pub delivered_batches: u64,
    pub affected_rows: Option<u64>,
    pub server_elapsed: Option<std::time::Duration>,
    pub scanned_bytes: Option<u64>,
    pub memory_bytes: Option<u64>,
    pub warnings: Vec<String>,
}

/// Optional server measurements retrieved for one execution after submission.
/// Absence may mean the server log has not been flushed yet.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerMetrics {
    pub elapsed: Option<std::time::Duration>,
    pub scanned_bytes: Option<u64>,
    pub memory_bytes: Option<u64>,
}

/// An atomic observation of an execution's status and counters.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionSnapshot {
    pub status: ExecutionStatus,
    pub metrics: ExecutionMetrics,
}

/// Driver control for work spanning submission and streaming.
/// Implementations retain the Tokio task handle and abort local work on cancel.
/// They may additionally request native server cancellation. Cancellation is
/// idempotent; it cannot overwrite an already terminal status. Final snapshots
/// remain readable after cancellation and after the execution is dropped.
pub trait ExecutionControl: Send + Sync {
    /// Stop local work and finalize elapsed time before returning.
    /// This does not guarantee that an already submitted server query stops.
    fn cancel(&self);

    fn snapshot(&self) -> ExecutionSnapshot;
}

/// A driver-owned query lifetime. Dropping it cancels outstanding local work.
/// Clone `control` to observe final status after dropping the execution.
/// Drivers must keep the task alive through streaming, not just submission.
pub struct QueryExecution {
    pub id: QueryId,
    pub events: QueryStream,
    pub control: std::sync::Arc<dyn ExecutionControl>,
}

impl Drop for QueryExecution {
    fn drop(&mut self) {
        self.control.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct Control(AtomicUsize);

    impl ExecutionControl for Control {
        fn cancel(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
        fn snapshot(&self) -> ExecutionSnapshot {
            unreachable!("this test only observes drop ownership")
        }
    }

    #[test]
    fn dropping_execution_cancels_even_when_control_is_retained() {
        let control = Arc::new(Control(AtomicUsize::new(0)));
        let execution = QueryExecution {
            id: QueryId::new(),
            events: Box::pin(futures::stream::pending()),
            control: control.clone(),
        };
        drop(execution);
        assert_eq!(control.0.load(Ordering::SeqCst), 1);
    }
}
