//! Sorted views of a result, computed off the UI thread.
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use super::{RunId, RunMsg};
use crate::{event::Msg, result::ResultTable};

/// Sort `table` by `column`, reporting as a view of `base`.
pub fn spawn_view(
    table: Arc<ResultTable>,
    column: usize,
    descending: bool,
    base: RunId,
    view: RunId,
    messages: UnboundedSender<Msg>,
) {
    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(move || table.sorted(column, descending))
            .await
            .map_err(anyhow::Error::from);
        let _ = messages.send(Msg::Run(RunMsg::Derived { base, view, result }));
    });
}
