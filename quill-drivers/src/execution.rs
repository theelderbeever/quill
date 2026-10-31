use quill_core::{
    error::{Error, Result},
    query::*,
};
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio::{sync::mpsc, task::JoinHandle};
#[derive(Debug, thiserror::Error)]
#[error("query result consumer closed the stream")]
struct ConsumerClosed;

pub(crate) struct Control {
    start: Instant,
    state: Mutex<ExecutionSnapshot>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl Control {
    fn finish(&self, status: ExecutionStatus) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.status == ExecutionStatus::Running {
            state.status = status;
            state.metrics.elapsed = self.start.elapsed();
        }
    }
}

impl ExecutionControl for Control {
    fn cancel(&self) {
        self.finish(ExecutionStatus::Cancelled);
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

pub(crate) fn start<F, Fut>(work: F) -> QueryExecution
where
    F: FnOnce(mpsc::Sender<Result<QueryEvent>>, Arc<Control>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    start_with_id(QueryId::new(), work)
}

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
                QueryEvent::Batch { batch, .. } => {
                    if batch.num_rows() > 0 && metrics.first_row.is_none() {
                        metrics.first_row = Some(self.start.elapsed());
                    }
                    metrics.delivered_rows += batch.num_rows() as u64;
                    metrics.delivered_batches += 1;
                }
                QueryEvent::ResultSetComplete {
                    affected_rows: Some(affected),
                    ..
                } => metrics.affected_rows = Some(*affected),
                _ => {}
            }
        }
        permit.send(Ok(event));
        Ok(())
    }

    /// Update driver-specific client metrics while the execution is still running.
    pub(crate) fn record(&self, update: impl FnOnce(&mut ClientMetrics)) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.status == ExecutionStatus::Running {
            update(&mut state.metrics);
        }
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
        let values = arrow::array::Int64Array::from_iter_values(0..rows);
        QueryEvent::Batch {
            result_set: 0,
            batch: arrow::record_batch::RecordBatch::try_from_iter([(
                "v",
                Arc::new(values) as arrow::array::ArrayRef,
            )])
            .unwrap(),
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
    async fn reports_worker_errors_through_the_stream() {
        let mut execution = start(|_, _| async { Err(Error::UnsupportedOperation("test")) });
        assert!(execution.events.try_next().await.is_err());
        assert_eq!(execution.control.snapshot().status, ExecutionStatus::Failed);
    }
}
