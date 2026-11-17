//! Query results held in memory as the drivers decoded them.
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use futures::StreamExt;
use quill_core::{
    query::{ExecutionStatus, QueryEvent, QueryExecution},
    value::{Column, Row, RowBatch},
};

/// Live counters, shared with whoever displays progress.
#[derive(Debug, Default)]
pub struct Progress {
    rows: AtomicU64,
    bytes: AtomicU64,
}

/// A point-in-time read of [`Progress`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Received {
    pub rows: u64,
    /// Approximate memory held for the received rows.
    pub bytes: u64,
}

impl Progress {
    pub fn snapshot(&self) -> Received {
        Received {
            rows: self.rows.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
        }
    }

    fn record(&self, batch: &RowBatch) {
        self.rows.fetch_add(batch.len() as u64, Ordering::Relaxed);
        self.bytes
            .fetch_add(batch.approx_bytes() as u64, Ordering::Relaxed);
    }
}

/// Why a collected query holds fewer rows than the query produced.
#[derive(Debug)]
pub enum Truncation {
    Cancelled,
    Failed(quill_core::error::Error),
}

/// Everything collected from one execution.
#[derive(Debug)]
pub struct Collected {
    /// Ordered by result set index.
    pub result_sets: Vec<CollectedSet>,
    pub truncation: Option<Truncation>,
}

/// One result set; rowless statements hold no table.
#[derive(Debug)]
pub struct CollectedSet {
    pub index: usize,
    pub affected_rows: Option<u64>,
    pub data: Option<ResultTable>,
}

/// Rows in memory, shown in `order` when the view is sorted. Sorted views
/// share their rows with the result they were drawn from.
#[derive(Debug, Clone)]
pub struct ResultTable {
    columns: Arc<[Column]>,
    rows: Arc<[Row]>,
    order: Option<Arc<[usize]>>,
}

impl ResultTable {
    pub fn new(columns: Arc<[Column]>, rows: Vec<Row>) -> Self {
        Self {
            columns,
            rows: rows.into(),
            order: None,
        }
    }

    pub fn columns(&self) -> &Arc<[Column]> {
        &self.columns
    }

    pub fn rows(&self) -> u64 {
        self.rows.len() as u64
    }

    /// The `index`th shown row.
    pub fn row(&self, index: u64) -> Option<&Row> {
        let index = usize::try_from(index).ok()?;
        let index = match &self.order {
            Some(order) => *order.get(index)?,
            None => index,
        };
        self.rows.get(index)
    }

    /// Every shown row, in shown order.
    pub fn iter(&self) -> impl Iterator<Item = &Row> {
        (0..self.rows()).filter_map(|i| self.row(i))
    }

    /// The base rows ordered by `column`, ties kept in their original order.
    ///
    /// ```
    /// # use std::sync::Arc;
    /// # use quill_core::value::{Column, Value};
    /// # use quill_tui::result::ResultTable;
    /// let table = ResultTable::new(
    ///     Arc::from([Column::new("n", "int8")]),
    ///     vec![Box::from([Value::Int(2)]), Box::from([Value::Null]), Box::from([Value::Int(1)])],
    /// );
    /// let sorted = table.sorted(0, false);
    /// let values: Vec<_> = sorted.iter().map(|r| r[0].clone()).collect();
    /// assert_eq!(values, [Value::Int(1), Value::Int(2), Value::Null]);
    /// ```
    pub fn sorted(&self, column: usize, descending: bool) -> Self {
        let mut order: Vec<usize> = (0..self.rows.len()).collect();
        order.sort_by(|&a, &b| {
            let ordering = self.rows[a][column].sort_cmp(&self.rows[b][column]);
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Self {
            columns: self.columns.clone(),
            rows: self.rows.clone(),
            order: Some(order.into()),
        }
    }
}

/// A result set still receiving rows.
struct OpenSet {
    columns: Arc<[Column]>,
    rows: Vec<Row>,
}

/// Collect `execution` in memory. Owning the execution means dropping this
/// future cancels the query. Failures and cancellation are reported as
/// [`Truncation`], keeping rows received so far.
///
/// ```
/// # use std::sync::Arc;
/// # use quill_core::{query::*, value::*};
/// # use quill_tui::result::{Progress, collect};
/// # struct Done;
/// # impl ExecutionControl for Done {
/// #     fn cancel(&self) {}
/// #     fn snapshot(&self) -> ExecutionSnapshot {
/// #         ExecutionSnapshot { status: ExecutionStatus::Completed, metrics: ClientMetrics::started() }
/// #     }
/// # }
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let columns: Arc<[Column]> = Arc::from([Column::new("n", "int8")]);
/// # let rows = (0..3).map(|n| Box::from([Value::Int(n)]) as Row).collect();
/// # let batch = RowBatch::new(columns.clone(), rows)?;
/// # let events = vec![
/// #     Ok(QueryEvent::Columns { result_set: 0, columns }),
/// #     Ok(QueryEvent::Rows { result_set: 0, batch }),
/// #     Ok(QueryEvent::ResultSetComplete { result_set: 0, affected_rows: None }),
/// #     Ok(QueryEvent::Complete),
/// # ];
/// # let execution = QueryExecution { id: QueryId::new(), events: Box::pin(futures::stream::iter(events)), control: Arc::new(Done) };
/// let progress = Progress::default();
/// let collected = collect(execution, &progress).await;
/// let table = collected.result_sets[0].data.as_ref().ok_or("rows")?;
/// assert_eq!(table.rows(), 3);
/// assert_eq!(progress.snapshot().rows, 3);
/// # Ok(())
/// # }
/// ```
pub async fn collect(mut execution: QueryExecution, progress: &Progress) -> Collected {
    let mut open: BTreeMap<usize, OpenSet> = BTreeMap::new();
    let mut done = Vec::new();
    let finish = |set: OpenSet| ResultTable::new(set.columns, set.rows);

    let truncation = loop {
        let Some(event) = execution.events.next().await else {
            break match execution.control.snapshot().status {
                ExecutionStatus::Cancelled => Some(Truncation::Cancelled),
                _ => None,
            };
        };
        match event {
            Ok(QueryEvent::Columns {
                result_set,
                columns,
            }) => {
                open.insert(
                    result_set,
                    OpenSet {
                        columns,
                        rows: Vec::new(),
                    },
                );
            }
            Ok(QueryEvent::Rows { result_set, batch }) => {
                progress.record(&batch);
                let set = open.entry(result_set).or_insert_with(|| OpenSet {
                    columns: batch.columns().clone(),
                    rows: Vec::new(),
                });
                set.rows.extend(batch.into_rows());
            }
            Ok(QueryEvent::ResultSetComplete {
                result_set,
                affected_rows,
            }) => done.push(CollectedSet {
                index: result_set,
                affected_rows,
                data: open.remove(&result_set).map(finish),
            }),
            Ok(QueryEvent::Complete) => break None,
            Err(e) => break Some(Truncation::Failed(e)),
        }
    };

    for (index, set) in open {
        done.push(CollectedSet {
            index,
            affected_rows: None,
            data: Some(finish(set)),
        });
    }
    done.sort_by_key(|set| set.index);
    Collected {
        result_sets: done,
        truncation,
    }
}
