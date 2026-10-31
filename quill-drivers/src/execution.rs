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
            metrics: ExecutionMetrics {
                elapsed: std::time::Duration::ZERO,
                delivered_rows: 0,
                delivered_batches: 0,
                affected_rows: None,
                server_elapsed: None,
                scanned_bytes: None,
                memory_bytes: None,
                warnings: Vec::new(),
            },
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
    pub(crate) async fn send(
        &self,
        sender: &mpsc::Sender<Result<QueryEvent>>,
        event: QueryEvent,
    ) -> Result<()> {
        let permit = sender
            .reserve()
            .await
            .map_err(|_| Error::QueryExecution(Box::new(ConsumerClosed)))?;
        if let QueryEvent::Batch { ref batch, .. } = event {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.metrics.delivered_rows += batch.num_rows() as u64;
            state.metrics.delivered_batches += 1;
        }
        if let QueryEvent::ResultSetComplete {
            affected_rows: Some(affected),
            ..
        } = &event
        {
            self.state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .metrics
                .affected_rows = Some(*affected);
        }
        permit.send(Ok(event));
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
