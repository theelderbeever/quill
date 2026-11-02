//! Execute a query and collect it in memory, reporting live progress.
use std::{sync::Arc, time::Instant};

use datafusion::prelude::SessionContext;
use quill_core::query::{ClientMetrics, ExecutionControl, QueryId, QueryRequest};
use quill_providers::ConnectionPool;
use tokio::{sync::mpsc::UnboundedSender, task::JoinHandle};

use super::{RunId, RunMsg};
use crate::{
    event::Msg,
    result::{self, Progress, Received},
};

/// A query in flight. Dropping it stops the work.
pub struct Run {
    id: RunId,
    progress: Arc<Progress>,
    control: Option<Arc<dyn ExecutionControl>>,
    query: Option<QueryId>,
    started: Instant,
    task: JoinHandle<()>,
}

impl Run {
    /// Collect into tables on `results`, the connection's session context.
    pub fn start(
        pool: Arc<dyn ConnectionPool>,
        results: SessionContext,
        sql: String,
        messages: UnboundedSender<Msg>,
    ) -> Self {
        let id = RunId::next();
        let progress = Arc::new(Progress::default());
        let task = tokio::spawn({
            let progress = progress.clone();
            async move {
                let result = async {
                    let execution = pool.execute(QueryRequest::new(sql)).await?;
                    let started = RunMsg::Started {
                        run: id,
                        query: execution.id,
                        control: execution.control.clone(),
                    };
                    let _ = messages.send(Msg::Run(started));
                    let prefix = result::run_prefix(id);
                    let collected =
                        result::collect(execution, &results, &prefix, &progress).await?;
                    anyhow::Ok(collected)
                }
                .await;
                let _ = messages.send(Msg::Run(RunMsg::Finished { run: id, result }));
            }
        });
        Self {
            id,
            progress,
            control: None,
            query: None,
            started: Instant::now(),
            task,
        }
    }

    pub const fn id(&self) -> RunId {
        self.id
    }

    pub fn attach(&mut self, query: QueryId, control: Arc<dyn ExecutionControl>) {
        self.query = Some(query);
        self.control = Some(control);
    }

    /// The driver's id for this execution, once it has started.
    pub const fn query(&self) -> Option<QueryId> {
        self.query
    }

    /// Final counters: the driver's view, if it started, and what was collected.
    pub fn metrics(&self) -> (Option<ClientMetrics>, Received) {
        let client = self.control.as_ref().map(|c| c.snapshot().metrics);
        (client, self.progress.snapshot())
    }

    /// Stop the query, keeping rows already collected. Before the driver has
    /// accepted it there is nothing to keep, so the task is aborted.
    pub fn cancel(&self) {
        match &self.control {
            Some(control) => control.cancel(),
            None => self.task.abort(),
        }
    }

    pub fn progress(&self) -> (Received, std::time::Duration) {
        let elapsed = self
            .control
            .as_ref()
            .map_or_else(|| self.started.elapsed(), |c| c.snapshot().metrics.elapsed);
        (self.progress.snapshot(), elapsed)
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        // Dropping the collect future drops the execution, which cancels it.
        self.task.abort();
    }
}
