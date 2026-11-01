//! Sorted or filtered views of a stored result, and exports of it, run
//! through DataFusion against the files already on disk.
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use datafusion::{execution::SendableRecordBatchStream, prelude::SessionContext};
use futures::StreamExt;
use quill_core::{
    error::Error,
    query::{
        ClientMetrics, ExecutionControl, ExecutionSnapshot, ExecutionStatus, QueryEvent,
        QueryExecution, QueryId,
    },
};
use quill_io::feather::{FeatherStore, SinkProgress};
use tokio::sync::mpsc::UnboundedSender;

use super::{RunId, RunMsg};
use crate::event::Msg;

/// Status for a local DataFusion stream; dropping the stream stops it.
struct LocalControl(Mutex<ExecutionStatus>);

impl ExecutionControl for LocalControl {
    fn cancel(&self) {
        let mut status = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if *status == ExecutionStatus::Running {
            *status = ExecutionStatus::Cancelled;
        }
    }

    fn snapshot(&self) -> ExecutionSnapshot {
        ExecutionSnapshot {
            status: *self.0.lock().unwrap_or_else(|e| e.into_inner()),
            metrics: ClientMetrics::started(),
        }
    }
}

/// Present a DataFusion stream as a single-result-set execution.
fn execution(stream: SendableRecordBatchStream) -> QueryExecution {
    let schema = QueryEvent::Schema {
        result_set: 0,
        schema: stream.schema(),
    };
    let batches = stream.map(|batch| {
        batch
            .map(|batch| QueryEvent::Batch {
                result_set: 0,
                batch,
            })
            .map_err(|e| Error::QueryExecution(Box::new(e)))
    });
    let tail = [
        QueryEvent::ResultSetComplete {
            result_set: 0,
            affected_rows: None,
        },
        QueryEvent::Complete,
    ];
    let events = futures::stream::once(async { Ok(schema) })
        .chain(batches)
        .chain(futures::stream::iter(tail.map(Ok)));
    QueryExecution {
        id: QueryId::new(),
        events: Box::pin(events),
        control: Arc::new(LocalControl(Mutex::new(ExecutionStatus::Running))),
    }
}

/// Drain `sql` over `ctx` into `dir`, reporting as a view of `base`.
pub fn spawn_view(
    ctx: SessionContext,
    sql: String,
    dir: PathBuf,
    base: RunId,
    view: RunId,
    messages: UnboundedSender<Msg>,
) {
    tokio::spawn(async move {
        let result = async {
            std::fs::create_dir_all(&dir)?;
            let stream = ctx.sql(&sql).await?.execute_stream().await?;
            let stored = FeatherStore::new(&dir, None)
                .drain(execution(stream), &SinkProgress::default())
                .await?;
            anyhow::Ok(stored)
        }
        .await;
        let _ = messages.send(Msg::Run(RunMsg::Derived { base, view, result }));
    });
}

/// Run a `COPY ... TO` statement; the file format follows the extension.
pub fn spawn_save(ctx: SessionContext, sql: String, path: PathBuf, messages: UnboundedSender<Msg>) {
    tokio::spawn(async move {
        let result = async {
            ctx.sql(&sql).await?.collect().await?;
            anyhow::Ok(())
        }
        .await;
        let _ = messages.send(Msg::Run(RunMsg::Saved { path, result }));
    });
}
