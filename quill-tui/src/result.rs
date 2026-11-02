//! Query results held in memory on a connection's DataFusion context.
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use arrow::{datatypes::SchemaRef, record_batch::RecordBatch};
use datafusion::{
    datasource::MemTable, execution::session_state::SessionStateBuilder, prelude::SessionContext,
};
use futures::StreamExt;
use quill_core::query::{ExecutionStatus, QueryEvent, QueryExecution};

use crate::{connection::quote, task::RunId};

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
    /// Arrow memory held for the received batches.
    pub bytes: u64,
}

impl Progress {
    pub fn snapshot(&self) -> Received {
        Received {
            rows: self.rows.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
        }
    }

    fn record(&self, batch: &RecordBatch) {
        self.rows
            .fetch_add(batch.num_rows() as u64, Ordering::Relaxed);
        self.bytes
            .fetch_add(batch.get_array_memory_size() as u64, Ordering::Relaxed);
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

/// A result registered on a session context, deregistered on drop.
pub struct ResultTable {
    ctx: SessionContext,
    name: String,
    schema: SchemaRef,
    rows: u64,
}

impl ResultTable {
    /// Register `batches` under `name` as one ordered partition.
    pub(crate) fn register(
        ctx: &SessionContext,
        name: String,
        schema: SchemaRef,
        batches: Vec<RecordBatch>,
    ) -> datafusion::error::Result<Self> {
        let rows = batches.iter().map(|b| b.num_rows() as u64).sum();
        let table = MemTable::try_new(schema.clone(), vec![batches])?;
        ctx.register_table(name.as_str(), Arc::new(table))?;
        Ok(Self {
            ctx: ctx.clone(),
            name,
            schema,
            rows,
        })
    }

    pub const fn schema(&self) -> &SchemaRef {
        &self.schema
    }

    pub const fn rows(&self) -> u64 {
        self.rows
    }

    /// The table name, quoted for SQL.
    pub fn sql_name(&self) -> String {
        quote(&self.name, '"')
    }

    /// A context sharing this table's catalog that keeps row order. Results
    /// have one partition; repartitioning a filter would shuffle it.
    pub fn ordered(&self) -> SessionContext {
        // Recreating the default catalog would replace the shared one and
        // drop every registered result.
        let config = self
            .ctx
            .copied_config()
            .with_target_partitions(1)
            .with_create_default_catalog_and_schema(false);
        let state = SessionStateBuilder::new_from_existing(self.ctx.state())
            .with_config(config)
            .build();
        SessionContext::new_with_state(state)
    }
}

impl std::fmt::Debug for ResultTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResultTable")
            .field("name", &self.name)
            .field("rows", &self.rows)
            .finish_non_exhaustive()
    }
}

impl Drop for ResultTable {
    fn drop(&mut self) {
        let _ = self.ctx.deregister_table(self.name.as_str());
    }
}

/// A result set still receiving batches.
struct OpenSet {
    schema: SchemaRef,
    batches: Vec<RecordBatch>,
}

/// Collect `execution` into tables on `ctx` named after `run`. Owning the
/// execution means dropping this future cancels the query. Failures and
/// cancellation are reported as [`Truncation`], keeping rows received so far.
///
/// ```
/// # use std::sync::Arc;
/// # use arrow::{array::Int64Array, datatypes::*, record_batch::RecordBatch};
/// # use datafusion::prelude::SessionContext;
/// # use quill_core::query::*;
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
/// # let schema = Arc::new(Schema::new(vec![Field::new("n", DataType::Int64, false)]));
/// # let batch = RecordBatch::try_new(schema.clone(), vec![Arc::new(Int64Array::from_iter_values(0..3))])?;
/// # let events = vec![
/// #     Ok(QueryEvent::Schema { result_set: 0, schema }),
/// #     Ok(QueryEvent::Batch { result_set: 0, batch }),
/// #     Ok(QueryEvent::ResultSetComplete { result_set: 0, affected_rows: None }),
/// #     Ok(QueryEvent::Complete),
/// # ];
/// # let execution = QueryExecution { id: QueryId::new(), events: Box::pin(futures::stream::iter(events)), control: Arc::new(Done) };
/// let ctx = SessionContext::new();
/// let progress = Progress::default();
/// let collected = collect(execution, &ctx, "run_0", &progress).await?;
/// let table = collected.result_sets[0].data.as_ref().ok_or("rows")?;
/// assert_eq!(table.rows(), 3);
/// assert_eq!(progress.snapshot().rows, 3);
/// # Ok(())
/// # }
/// ```
pub async fn collect(
    mut execution: QueryExecution,
    ctx: &SessionContext,
    prefix: &str,
    progress: &Progress,
) -> datafusion::error::Result<Collected> {
    let mut open: BTreeMap<usize, OpenSet> = BTreeMap::new();
    let mut done = Vec::new();
    let finish = |index: usize, set: OpenSet| {
        let name = format!("{prefix}_{index}");
        ResultTable::register(ctx, name, set.schema, set.batches)
    };

    let truncation = loop {
        let Some(event) = execution.events.next().await else {
            break match execution.control.snapshot().status {
                ExecutionStatus::Cancelled => Some(Truncation::Cancelled),
                _ => None,
            };
        };
        match event {
            Ok(QueryEvent::Schema { result_set, schema }) => {
                let set = OpenSet {
                    schema,
                    batches: Vec::new(),
                };
                open.insert(result_set, set);
            }
            Ok(QueryEvent::Batch { result_set, batch }) => {
                if batch.num_rows() == 0 {
                    continue;
                }
                progress.record(&batch);
                match open.get_mut(&result_set) {
                    Some(set) => set.batches.push(batch),
                    // Providers may stream without announcing a schema first.
                    None => {
                        let set = OpenSet {
                            schema: batch.schema(),
                            batches: vec![batch],
                        };
                        open.insert(result_set, set);
                    }
                }
            }
            Ok(QueryEvent::ResultSetComplete {
                result_set,
                affected_rows,
            }) => {
                let data = open
                    .remove(&result_set)
                    .map(|set| finish(result_set, set))
                    .transpose()?;
                done.push(CollectedSet {
                    index: result_set,
                    affected_rows,
                    data,
                });
            }
            Ok(QueryEvent::Complete) => break None,
            Err(e) => break Some(Truncation::Failed(e)),
        }
    };

    for (index, set) in open {
        done.push(CollectedSet {
            index,
            affected_rows: None,
            data: Some(finish(index, set)?),
        });
    }
    done.sort_by_key(|set| set.index);
    Ok(Collected {
        result_sets: done,
        truncation,
    })
}

/// Table name prefix for a run's own result sets.
pub fn run_prefix(run: RunId) -> String {
    format!("run_{run}")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow::{
        array::Int64Array,
        datatypes::{DataType, Field, Schema},
    };
    use quill_core::query::*;

    use super::*;

    struct Done;
    impl ExecutionControl for Done {
        fn cancel(&self) {}
        fn snapshot(&self) -> ExecutionSnapshot {
            ExecutionSnapshot {
                status: ExecutionStatus::Completed,
                metrics: ClientMetrics::started(),
            }
        }
    }

    fn execution(batches: Vec<RecordBatch>) -> QueryExecution {
        let schema = batches[0].schema();
        let events = std::iter::once(QueryEvent::Schema {
            result_set: 0,
            schema,
        })
        .chain(batches.into_iter().map(|batch| QueryEvent::Batch {
            result_set: 0,
            batch,
        }))
        .chain([
            QueryEvent::ResultSetComplete {
                result_set: 0,
                affected_rows: None,
            },
            QueryEvent::Complete,
        ])
        .map(Ok)
        .collect::<Vec<_>>();
        QueryExecution {
            id: QueryId::new(),
            events: Box::pin(futures::stream::iter(events)),
            control: Arc::new(Done),
        }
    }

    fn numbers(range: std::ops::Range<i64>) -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![Field::new("n", DataType::Int64, false)]));
        RecordBatch::try_new(schema, vec![Arc::new(Int64Array::from_iter_values(range))]).unwrap()
    }

    /// Values of `n` from `sql`, in result order.
    async fn values(ctx: &SessionContext, sql: &str) -> Vec<i64> {
        let batches = ctx.sql(sql).await.unwrap().collect().await.unwrap();
        batches
            .iter()
            .flat_map(|b| {
                let n = b.column(0).as_any().downcast_ref::<Int64Array>().unwrap();
                n.values().to_vec()
            })
            .collect()
    }

    #[tokio::test]
    async fn tables_live_on_the_session_until_dropped() {
        let ctx = SessionContext::new();
        let progress = Progress::default();
        let collected = collect(execution(vec![numbers(0..10)]), &ctx, "run_0", &progress)
            .await
            .unwrap();
        let table = collected
            .result_sets
            .into_iter()
            .next()
            .unwrap()
            .data
            .unwrap();
        assert!(ctx.table_exist("run_0_0").unwrap());
        let ordered = table.ordered();
        assert_eq!(
            values(&ordered, "SELECT n FROM run_0_0 LIMIT 3 OFFSET 4").await,
            [4, 5, 6]
        );
        drop(table);
        assert!(!ctx.table_exist("run_0_0").unwrap());
    }

    #[tokio::test]
    async fn filters_keep_row_order_across_batches() {
        let ctx = SessionContext::new();
        let batches = (0..64).map(|i| numbers(i * 100..(i + 1) * 100)).collect();
        let collected = collect(execution(batches), &ctx, "run_1", &Progress::default())
            .await
            .unwrap();
        let table = collected
            .result_sets
            .into_iter()
            .next()
            .unwrap()
            .data
            .unwrap();
        let kept = values(&table.ordered(), "SELECT n FROM run_1_0 WHERE n % 7 = 0").await;
        assert!(kept.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(kept.len(), 915);
    }
}
