//! Query task lifetime, live metrics and batched event delivery shared by drivers.
use quill_core::{
    error::{Error, Result},
    query::*,
    value::{Column, Row, RowBatch},
};
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio::{sync::mpsc, task::JoinHandle};

/// Rows per delivered batch; the event channel holds at most one more batch.
const BATCH_ROWS: usize = 1024;

#[derive(Debug, thiserror::Error)]
#[error("query result consumer closed the stream")]
struct ConsumerClosed;

/// Asks the server to stop the statement this execution submitted.
type ServerCancel = Box<dyn FnOnce() + Send>;

pub(crate) struct Control {
    start: Instant,
    state: Mutex<ExecutionSnapshot>,
    task: Mutex<Option<JoinHandle<()>>>,
    server_cancel: Mutex<Option<ServerCancel>>,
}

impl Control {
    /// Returns whether this call ended the execution.
    fn finish(&self, status: ExecutionStatus) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.status != ExecutionStatus::Running {
            return false;
        }
        state.status = status;
        state.metrics.elapsed = self.start.elapsed();
        true
    }

    /// Register how to stop the submitted statement on the server. Runs at
    /// most once, and only if the execution is cancelled while running.
    pub(crate) fn on_server_cancel(&self, cancel: impl FnOnce() + Send + 'static) {
        *self.server_cancel.lock().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(cancel));
    }
}

impl ExecutionControl for Control {
    fn cancel(&self) {
        if self.finish(ExecutionStatus::Cancelled)
            && let Some(cancel) = self
                .server_cancel
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
        {
            cancel();
        }
        if let Some(task) = self.task.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            task.abort();
        }
    }
    fn snapshot(&self) -> ExecutionSnapshot {
        let mut snapshot = self.state.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if snapshot.status == ExecutionStatus::Running {
            snapshot.metrics.elapsed = self.start.elapsed();
        }
        snapshot
    }
}

// Also finalize status when a task panics or is aborted outside cancellation.
struct TaskLifetime(Arc<Control>);
impl Drop for TaskLifetime {
    fn drop(&mut self) {
        self.0.finish(ExecutionStatus::Failed);
    }
}

#[cfg(test)]
pub(crate) fn start<F, Fut>(work: F) -> QueryExecution
where
    F: FnOnce(mpsc::Sender<Result<QueryEvent>>, Arc<Control>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    start_with_id(QueryId::new(), work)
}

/// Spawn `work` and return its execution. Timing starts now.
pub(crate) fn start_with_id<F, Fut>(id: QueryId, work: F) -> QueryExecution
where
    F: FnOnce(mpsc::Sender<Result<QueryEvent>>, Arc<Control>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let control = Arc::new(Control {
        start: Instant::now(),
        state: Mutex::new(ExecutionSnapshot {
            status: ExecutionStatus::Running,
            metrics: ClientMetrics::started(),
        }),
        task: Mutex::new(None),
        server_cancel: Mutex::new(None),
    });
    // One pending event bounds memory and keeps the producer tied to consumption.
    let (sender, receiver) = mpsc::channel(1);
    let task_control = control.clone();
    let task = tokio::spawn(async move {
        let _lifetime = TaskLifetime(task_control.clone());
        let result = work(sender.clone(), task_control.clone()).await;
        match result {
            Ok(()) => {
                task_control.finish(ExecutionStatus::Completed);
                let _ = sender.send(Ok(QueryEvent::Complete)).await;
            }
            Err(error) => {
                task_control.finish(ExecutionStatus::Failed);
                let _ = sender.send(Err(error)).await;
            }
        }
    });
    *control.task.lock().unwrap_or_else(|e| e.into_inner()) = Some(task);
    let events = futures::stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|event| (event, receiver))
    });
    QueryExecution {
        id,
        events: Box::pin(events),
        control,
    }
}

impl Control {
    /// Deliver one event, counting it only while the execution is still running.
    pub(crate) async fn send(
        &self,
        sender: &mpsc::Sender<Result<QueryEvent>>,
        event: QueryEvent,
    ) -> Result<()> {
        let permit = sender
            .reserve()
            .await
            .map_err(|_| Error::QueryExecution(Box::new(ConsumerClosed)))?;
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.status != ExecutionStatus::Running {
                return Ok(());
            }
            let metrics = &mut state.metrics;
            match &event {
                QueryEvent::Rows { batch, .. } => {
                    if !batch.is_empty() && metrics.first_row.is_none() {
                        metrics.first_row = Some(self.start.elapsed());
                    }
                    metrics.delivered_rows += batch.len() as u64;
                    metrics.delivered_batches += 1;
                }
                QueryEvent::ResultSetComplete {
                    affected_rows: Some(affected),
                    ..
                } => {
                    *metrics.affected_rows.get_or_insert(0) += affected;
                }
                _ => {}
            }
        }
        permit.send(Ok(event));
        Ok(())
    }
}

/// Emits result sets in order: columns, batched rows, completion.
pub(crate) struct Emitter<'a> {
    sender: &'a mpsc::Sender<Result<QueryEvent>>,
    control: &'a Control,
    result_set: usize,
    columns: Option<Arc<[Column]>>,
    rows: Vec<Row>,
}

impl<'a> Emitter<'a> {
    pub(crate) const fn new(
        sender: &'a mpsc::Sender<Result<QueryEvent>>,
        control: &'a Control,
    ) -> Self {
        Self {
            sender,
            control,
            result_set: 0,
            columns: None,
            rows: Vec::new(),
        }
    }

    /// Whether the current result set has announced its columns.
    pub(crate) const fn has_columns(&self) -> bool {
        self.columns.is_some()
    }

    /// Announce the current result set's columns.
    pub(crate) async fn columns(&mut self, columns: Arc<[Column]>) -> Result<()> {
        self.columns = Some(columns.clone());
        let event = QueryEvent::Columns {
            result_set: self.result_set,
            columns,
        };
        self.control.send(self.sender, event).await
    }

    /// Queue a row of the current result set, delivering a full batch.
    pub(crate) async fn row(&mut self, row: Row) -> Result<()> {
        self.rows.push(row);
        if self.rows.len() >= BATCH_ROWS {
            self.flush().await?;
        }
        Ok(())
    }

    async fn flush(&mut self) -> Result<()> {
        if self.rows.is_empty() {
            return Ok(());
        }
        let Some(columns) = self.columns.clone() else {
            return Err(Error::MissingMetadata("result columns"));
        };
        let rows = std::mem::take(&mut self.rows);
        let batch = RowBatch::new(columns, rows).map_err(|e| Error::QueryExecution(Box::new(e)))?;
        let event = QueryEvent::Rows {
            result_set: self.result_set,
            batch,
        };
        self.control.send(self.sender, event).await
    }

    /// Finish the current result set and start the next.
    pub(crate) async fn complete(&mut self, affected_rows: Option<u64>) -> Result<()> {
        self.flush().await?;
        let event = QueryEvent::ResultSetComplete {
            result_set: self.result_set,
            affected_rows,
        };
        self.control.send(self.sender, event).await?;
        self.result_set += 1;
        self.columns = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::TryStreamExt;

    #[tokio::test]
    async fn preserves_id_and_freezes_completed_timing() {
        let id = QueryId::new();
        let mut execution = start_with_id(id, |_, _| async { Ok(()) });
        assert_eq!(execution.id, id);
        while execution.events.try_next().await.unwrap().is_some() {}
        let snapshot = execution.control.snapshot();
        assert_eq!(snapshot.status, ExecutionStatus::Completed);
        execution.control.cancel();
        assert_eq!(execution.control.snapshot(), snapshot);
    }

    fn batch(rows: i64) -> QueryEvent {
        use quill_core::value::Value;
        let columns: Arc<[Column]> = Arc::from([Column::new("v", "int8")]);
        let rows = (0..rows)
            .map(|v| Box::from([Value::Int(v)]) as Row)
            .collect();
        QueryEvent::Rows {
            result_set: 0,
            batch: RowBatch::new(columns, rows).unwrap(),
        }
    }

    #[tokio::test]
    async fn first_row_waits_for_rows_and_is_set_once() {
        let mut execution = start(|sender, control| async move {
            control.send(&sender, batch(0)).await?;
            assert_eq!(control.snapshot().metrics.first_row, None);
            control.send(&sender, batch(2)).await?;
            let first = control.snapshot().metrics.first_row;
            assert!(first.is_some());
            control.send(&sender, batch(3)).await?;
            assert_eq!(control.snapshot().metrics.first_row, first);
            Ok(())
        });
        while execution.events.try_next().await.unwrap().is_some() {}
        let metrics = execution.control.snapshot().metrics;
        assert_eq!(metrics.delivered_rows, 5);
        assert_eq!(metrics.delivered_batches, 3);
        assert!(
            metrics
                .first_row
                .is_some_and(|first| first <= metrics.elapsed)
        );
    }

    #[tokio::test]
    async fn rowless_results_have_no_first_row() {
        let mut execution = start(|sender, control| async move {
            control
                .send(
                    &sender,
                    QueryEvent::ResultSetComplete {
                        result_set: 0,
                        affected_rows: Some(4),
                    },
                )
                .await
        });
        while execution.events.try_next().await.unwrap().is_some() {}
        let metrics = execution.control.snapshot().metrics;
        assert_eq!(metrics.first_row, None);
        assert_eq!(metrics.affected_rows, Some(4));
    }

    #[tokio::test]
    async fn cancellation_stays_terminal() {
        let execution = start(|_, _| std::future::pending());
        execution.control.cancel();
        tokio::task::yield_now().await;
        assert_eq!(
            execution.control.snapshot().status,
            ExecutionStatus::Cancelled
        );
    }

    #[tokio::test]
    async fn server_cancel_runs_once_and_only_while_running() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let register = |calls: Arc<AtomicUsize>, finish: bool| {
            start(move |_, control| async move {
                control.on_server_cancel(move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                });
                if finish {
                    Ok(())
                } else {
                    std::future::pending().await
                }
            })
        };
        let mut finished = register(calls.clone(), true);
        while finished.events.try_next().await.unwrap().is_some() {}
        finished.control.cancel();
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        let running = register(calls.clone(), false);
        for _ in 0..3 {
            tokio::task::yield_now().await;
        }
        running.control.cancel();
        running.control.cancel();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn reports_worker_errors_through_the_stream() {
        let mut execution = start(|_, _| async { Err(Error::UnsupportedOperation("test")) });
        assert!(execution.events.try_next().await.is_err());
        assert_eq!(execution.control.snapshot().status, ExecutionStatus::Failed);
    }
}
