//! Result streaming shared by the sqlx drivers.
use std::sync::Arc;

use futures::{TryStreamExt, stream::BoxStream};
use quill_core::{
    error::{Error, Result},
    value::{Column, Row},
};
use sqlx::{Database, Either};

use crate::execution::Emitter;

/// Decodes one driver's rows. Column kinds are fixed per result set.
pub(crate) trait RowDecoder<DB: Database> {
    /// Start a result set from its first row; returns its columns.
    fn start(&mut self, columns: &[DB::Column]) -> Arc<[Column]>;
    /// Decode every cell; never fails, falling back to the server's text or bytes.
    fn decode(&self, row: &DB::Row) -> Row;
}

/// A driver's stream of rows and per-statement completions.
pub(crate) type ResultStream<'a, DB> = BoxStream<
    'a,
    std::result::Result<Either<<DB as Database>::QueryResult, <DB as Database>::Row>, sqlx::Error>,
>;

pub(crate) fn query_error(error: sqlx::Error) -> Error {
    Error::QueryExecution(Box::new(error))
}

/// How a script's final result set ended when it returned no rows.
pub(crate) struct Rowless {
    /// Whether it was the script's only result set.
    pub only: bool,
    pub affected_rows: u64,
}

/// Stream every result set of `stream`. A trailing rowless set is returned
/// uncompleted so the caller can describe its columns: drivers report
/// columns only alongside rows.
pub(crate) async fn drive<DB: Database>(
    mut stream: ResultStream<'_, DB>,
    out: &mut Emitter<'_>,
    decoder: &mut impl RowDecoder<DB>,
    affected: impl Fn(&DB::QueryResult) -> u64,
) -> Result<Option<Rowless>> {
    use sqlx::Row as _;
    let mut sets = 0usize;
    let mut pending: Option<u64> = None;
    while let Some(item) = stream.try_next().await.map_err(query_error)? {
        match item {
            Either::Right(row) => {
                if let Some(count) = pending.take() {
                    out.complete(Some(count)).await?;
                }
                if !out.has_columns() {
                    let columns = decoder.start(row.columns());
                    out.columns(columns).await?;
                }
                out.row(decoder.decode(&row)).await?;
            }
            Either::Left(result) => {
                if let Some(count) = pending.take() {
                    out.complete(Some(count)).await?;
                }
                if out.has_columns() {
                    out.complete(None).await?;
                } else {
                    pending = Some(affected(&result));
                }
                sets += 1;
            }
        }
    }
    Ok(pending.map(|affected_rows| Rowless {
        only: sets == 1,
        affected_rows,
    }))
}

/// Complete a trailing rowless set, as a table when `columns` is non-empty.
pub(crate) async fn finish_rowless(
    out: &mut Emitter<'_>,
    rowless: Rowless,
    columns: Option<Arc<[Column]>>,
) -> Result<()> {
    match columns.filter(|c| !c.is_empty()) {
        Some(columns) => {
            out.columns(columns).await?;
            out.complete(None).await
        }
        None => out.complete(Some(rowless.affected_rows)).await,
    }
}
