//! Execute a query and drain it to disk, reporting live progress.
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use anyhow::Context as _;
use quill_core::query::{ClientMetrics, ExecutionControl, QueryId, QueryRequest};
use quill_drivers::ConnectionPool;
use quill_io::feather::{ByteLimit, FeatherStore, SinkMetrics, SinkProgress};
use tokio::{sync::mpsc::UnboundedSender, task::JoinHandle};

use super::{RunId, RunMsg};
use crate::event::Msg;

/// A query in flight. Dropping it stops the work.
pub struct Run {
    id: RunId,
    dir: PathBuf,
    progress: Arc<SinkProgress>,
    control: Option<Arc<dyn ExecutionControl>>,
    query: Option<QueryId>,
    started: Instant,
    task: JoinHandle<()>,
}

impl Run {
    /// Drain into a new directory under `results_dir`.
    pub fn start(
        pool: Arc<dyn ConnectionPool>,
        sql: String,
        results_dir: &Path,
        limit: Option<ByteLimit>,
        messages: UnboundedSender<Msg>,
    ) -> Self {
        let id = RunId::next();
        let dir = results_dir.join(format!("run-{}", id.0));
        let progress = Arc::new(SinkProgress::default());
        let task = tokio::spawn({
            let dir = dir.clone();
            let progress = progress.clone();
            async move {
                let result = async {
                    std::fs::create_dir_all(&dir)
                        .with_context(|| format!("create {}", dir.display()))?;
                    let execution = pool.execute(QueryRequest::new(sql)).await?;
                    let started = RunMsg::Started {
                        run: id,
                        query: execution.id,
                        control: execution.control.clone(),
                    };
                    let _ = messages.send(Msg::Run(started));
                    let stored = FeatherStore::new(&dir, limit)
                        .drain(execution, &progress)
                        .await?;
                    anyhow::Ok(stored)
                }
                .await;
                let _ = messages.send(Msg::Run(RunMsg::Finished { run: id, result }));
            }
        });
        Self {
            id,
            dir,
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

    /// Where this run's result files are written.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn attach(&mut self, query: QueryId, control: Arc<dyn ExecutionControl>) {
        self.query = Some(query);
        self.control = Some(control);
    }

    /// The driver's id for this execution, once it has started.
    pub const fn query(&self) -> Option<QueryId> {
        self.query
    }

    /// Final counters: the driver's view, if it started, and the sink's.
    pub fn metrics(&self) -> (Option<ClientMetrics>, SinkMetrics) {
        let client = self.control.as_ref().map(|c| c.snapshot().metrics);
        (client, self.progress.snapshot())
    }

    /// Stop the query, keeping rows already drained. Before the driver has
    /// accepted it there is nothing to keep, so the task is aborted.
    pub fn cancel(&self) {
        match &self.control {
            Some(control) => control.cancel(),
            None => self.task.abort(),
        }
    }

    pub fn progress(&self) -> (SinkMetrics, std::time::Duration) {
        let elapsed = self
            .control
            .as_ref()
            .map_or_else(|| self.started.elapsed(), |c| c.snapshot().metrics.elapsed);
        (self.progress.snapshot(), elapsed)
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        // Dropping the drain future drops the execution, which cancels it.
        self.task.abort();
    }
}

/// A run's result directory, removed when the result is replaced or the
/// connection's session ends. The session directory is the backstop.
pub struct ResultDir(PathBuf);

impl ResultDir {
    pub const fn new(dir: PathBuf) -> Self {
        Self(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ResultDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
