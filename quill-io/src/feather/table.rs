//! DataFusion access to a [`StoredResult`] with row-range pushdown.
use std::{ops::Range, sync::Arc};

use arrow::{
    array::{ArrayRef, UInt64Array},
    datatypes::{DataType, Field, Schema, SchemaRef},
    record_batch::RecordBatch,
};
use datafusion::{
    catalog::{Session, TableProvider},
    common::{DataFusionError, ScalarValue},
    datasource::TableType,
    execution::{SendableRecordBatchStream, TaskContext},
    logical_expr::{Between, BinaryExpr, Expr, Operator, TableProviderFilterPushDown},
    physical_plan::{
        ExecutionPlan,
        stream::RecordBatchStreamAdapter,
        streaming::{PartitionStream, StreamingTableExec},
    },
};

use super::{Batches, StoredResult};
use crate::error::{Error, Result};

/// Virtual zero-based row number. Never stored; filters on it select blocks.
pub const ROW_COLUMN: &str = "__row";

/// A [`TableProvider`] over a stored result plus the virtual [`ROW_COLUMN`].
///
/// Range filters on `__row` are pushed down, so deep pages read only the
/// blocks they touch. Other filters are applied by DataFusion above the scan.
///
/// ```
/// # use std::sync::Arc;
/// # use arrow::{array::Int64Array, datatypes::*, record_batch::RecordBatch};
/// # use datafusion::prelude::SessionContext;
/// # use quill_core::query::*;
/// # use quill_io::feather::{FeatherStore, FeatherTable, SinkProgress};
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
/// # let batch = RecordBatch::try_new(schema.clone(), vec![Arc::new(Int64Array::from_iter_values(0..100))])?;
/// # let events = vec![
/// #     Ok(QueryEvent::Schema { result_set: 0, schema }),
/// #     Ok(QueryEvent::Batch { result_set: 0, batch }),
/// #     Ok(QueryEvent::ResultSetComplete { result_set: 0, affected_rows: None }),
/// #     Ok(QueryEvent::Complete),
/// # ];
/// # let execution = QueryExecution { id: QueryId::new(), events: Box::pin(futures::stream::iter(events)), control: Arc::new(Done) };
/// # let dir = tempfile::tempdir()?;
/// # let mut stored = FeatherStore::new(dir.path(), None).drain(execution, &SinkProgress::default()).await?;
/// let result = stored.result_sets.remove(0).data.ok_or("rows")?;
/// let ctx = SessionContext::new();
/// ctx.register_table("r", Arc::new(FeatherTable::new(Arc::new(result))?))?;
/// let page = ctx.sql("SELECT n FROM r WHERE __row BETWEEN 40 AND 49").await?.collect().await?;
/// assert_eq!(page.iter().map(|b| b.num_rows()).sum::<usize>(), 10);
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct FeatherTable {
    result: Arc<StoredResult>,
    schema: SchemaRef,
}

impl FeatherTable {
    pub fn new(result: Arc<StoredResult>) -> Result<Self> {
        let stored = result.schema();
        if stored.column_with_name(ROW_COLUMN).is_some() {
            return Err(Error::ReservedColumn(ROW_COLUMN));
        }
        let fields = stored.fields().iter().cloned().chain([Arc::new(Field::new(
            ROW_COLUMN,
            DataType::UInt64,
            false,
        ))]);
        let schema = Arc::new(Schema::new_with_metadata(
            fields.collect::<Vec<_>>(),
            stored.metadata().clone(),
        ));
        Ok(Self { result, schema })
    }

    /// Rows selected by a pushed-down `__row` filter, or `None` if unsupported.
    fn row_range(expr: &Expr) -> Option<Range<u64>> {
        match expr {
            Expr::BinaryExpr(BinaryExpr { left, op, right }) => {
                match (Self::row_column(left), Self::row_column(right)) {
                    (true, false) => Self::comparison(*op, right),
                    (false, true) => Self::comparison(op.swap()?, left),
                    _ => None,
                }
            }
            Expr::Between(Between {
                expr,
                negated: false,
                low,
                high,
            }) if Self::row_column(expr) => {
                let low = Self::comparison(Operator::GtEq, low)?;
                let high = Self::comparison(Operator::LtEq, high)?;
                Some(intersect(low, high))
            }
            _ => None,
        }
    }

    fn row_column(expr: &Expr) -> bool {
        matches!(expr, Expr::Column(c) if c.name == ROW_COLUMN)
    }

    /// Rows satisfying `__row <op> value`.
    fn comparison(op: Operator, value: &Expr) -> Option<Range<u64>> {
        let Expr::Literal(value, _) = value else {
            return None;
        };
        let v: i128 = match value {
            ScalarValue::Int8(Some(v)) => (*v).into(),
            ScalarValue::Int16(Some(v)) => (*v).into(),
            ScalarValue::Int32(Some(v)) => (*v).into(),
            ScalarValue::Int64(Some(v)) => (*v).into(),
            ScalarValue::UInt8(Some(v)) => (*v).into(),
            ScalarValue::UInt16(Some(v)) => (*v).into(),
            ScalarValue::UInt32(Some(v)) => (*v).into(),
            ScalarValue::UInt64(Some(v)) => (*v).into(),
            _ => return None,
        };
        let clamp = |v: i128| v.clamp(0, u64::MAX.into()) as u64;
        Some(match op {
            Operator::Eq => clamp(v)..clamp(v + 1),
            Operator::GtEq => clamp(v)..u64::MAX,
            Operator::Gt => clamp(v + 1)..u64::MAX,
            Operator::LtEq => 0..clamp(v + 1),
            Operator::Lt => 0..clamp(v),
            _ => return None,
        })
    }
}

fn intersect(a: Range<u64>, b: Range<u64>) -> Range<u64> {
    let start = a.start.max(b.start);
    start..a.end.min(b.end).max(start)
}

#[async_trait::async_trait]
impl TableProvider for FeatherTable {
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> datafusion::common::Result<Vec<TableProviderFilterPushDown>> {
        Ok(filters
            .iter()
            .map(|f| match Self::row_range(f) {
                Some(_) => TableProviderFilterPushDown::Exact,
                None => TableProviderFilterPushDown::Unsupported,
            })
            .collect())
    }

    async fn scan(
        &self,
        _state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        limit: Option<usize>,
    ) -> datafusion::common::Result<Arc<dyn ExecutionPlan>> {
        let mut range = filters
            .iter()
            .filter_map(Self::row_range)
            .fold(0..self.result.rows(), intersect);
        if let Some(limit) = limit {
            range.end = range.end.min(range.start.saturating_add(limit as u64));
        }
        let projection = match projection {
            Some(p) => p.clone(),
            None => (0..self.schema.fields().len()).collect(),
        };
        let schema = Arc::new(self.schema.project(&projection)?);
        let partition = RowPartition {
            result: self.result.clone(),
            range,
            projection,
            schema: schema.clone(),
        };
        Ok(Arc::new(StreamingTableExec::try_new(
            schema,
            vec![Arc::new(partition)],
            None,
            [],
            false,
            limit,
        )?))
    }
}

/// One streaming partition over a row range, adding `__row` when projected.
#[derive(Debug)]
struct RowPartition {
    result: Arc<StoredResult>,
    range: Range<u64>,
    /// Indices into the table schema; the last table column is `__row`.
    projection: Vec<usize>,
    schema: SchemaRef,
}

impl PartitionStream for RowPartition {
    fn schema(&self) -> &SchemaRef {
        &self.schema
    }

    fn execute(&self, _ctx: Arc<TaskContext>) -> SendableRecordBatchStream {
        let row_idx = self.result.schema().fields().len();
        let projection = self.projection.clone();
        let schema = self.schema.clone();
        let mut start = self.range.start;
        let batches = Batches::new(self.result.clone(), self.range.clone()).map(move |batch| {
            let batch = batch.map_err(|e| DataFusionError::External(Box::new(e)))?;
            let rows = batch.num_rows() as u64;
            let columns: Vec<ArrayRef> = projection
                .iter()
                .map(|&i| match i == row_idx {
                    true => {
                        Arc::new(UInt64Array::from_iter_values(start..start + rows)) as ArrayRef
                    }
                    false => batch.column(i).clone(),
                })
                .collect();
            start += rows;
            Ok(RecordBatch::try_new_with_options(
                schema.clone(),
                columns,
                &arrow::record_batch::RecordBatchOptions::new().with_row_count(Some(rows as usize)),
            )?)
        });
        Box::pin(RecordBatchStreamAdapter::new(
            self.schema.clone(),
            futures::stream::iter(batches),
        ))
    }
}
