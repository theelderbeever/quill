//! Sorted or filtered views of a result, and exports of it, run through
//! DataFusion against the tables already in memory.
use std::path::PathBuf;

use datafusion::prelude::SessionContext;
use tokio::sync::mpsc::UnboundedSender;

use super::{RunId, RunMsg};
use crate::{event::Msg, result::ResultTable};

/// Table name for a view derived from another result.
pub fn view_name(view: RunId) -> String {
    format!("view_{view}")
}

/// Collect `sql` over `ctx` into a new table, reporting as a view of `base`.
/// `ctx` should keep row order; see [`ResultTable::ordered`].
pub fn spawn_view(
    ctx: SessionContext,
    sql: String,
    base: RunId,
    view: RunId,
    messages: UnboundedSender<Msg>,
) {
    tokio::spawn(async move {
        let result = async {
            let frame = ctx.sql(&sql).await?;
            let schema = frame.schema().inner().clone();
            let batches = frame.collect().await?;
            let table = ResultTable::register(&ctx, view_name(view), schema, batches)?;
            anyhow::Ok(table)
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
