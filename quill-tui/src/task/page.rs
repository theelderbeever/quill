//! Fetch and format one page of a stored result through DataFusion.
use std::ops::Range;

use arrow::{
    array::AsArray,
    datatypes::UInt64Type,
    util::display::{ArrayFormatter, FormatOptions},
};
use datafusion::prelude::SessionContext;
use quill_io::feather::ROW_COLUMN;
use tokio::sync::mpsc::UnboundedSender;

use super::{RunId, RunMsg};
use crate::event::Msg;

/// Name the stored result is registered under.
pub const TABLE: &str = "r";

/// Formatted cells for a run of consecutive rows.
pub struct Page {
    pub rows: Vec<PageRow>,
}

pub struct PageRow {
    pub row: u64,
    pub cells: Vec<String>,
}

pub fn spawn(
    ctx: SessionContext,
    run: RunId,
    index: u64,
    rows: Range<u64>,
    messages: UnboundedSender<Msg>,
) {
    tokio::spawn(async move {
        let result = fetch(&ctx, rows).await;
        let _ = messages.send(Msg::Run(RunMsg::Page { run, index, result }));
    });
}

async fn fetch(ctx: &SessionContext, rows: Range<u64>) -> anyhow::Result<Page> {
    let sql = format!(
        "SELECT * FROM {TABLE} WHERE {ROW_COLUMN} >= {} AND {ROW_COLUMN} < {}",
        rows.start, rows.end
    );
    let batches = ctx.sql(&sql).await?.collect().await?;
    let options = FormatOptions::default().with_null("NULL");
    let mut page = Vec::new();
    for batch in &batches {
        let schema = batch.schema();
        let row_index = schema.index_of(ROW_COLUMN)?;
        let numbers = batch.column(row_index).as_primitive::<UInt64Type>();
        let formatters = batch
            .columns()
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != row_index)
            .map(|(_, column)| ArrayFormatter::try_new(column.as_ref(), &options))
            .collect::<Result<Vec<_>, _>>()?;
        for i in 0..batch.num_rows() {
            page.push(PageRow {
                row: numbers.value(i),
                cells: formatters.iter().map(|f| f.value(i).to_string()).collect(),
            });
        }
    }
    page.sort_by_key(|r| r.row);
    Ok(Page { rows: page })
}
