//! FeatherStore round trips through the DataFusion driver and hand-built streams.
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use arrow::{
    array::{AsArray, Int64Array},
    datatypes::{DataType, Field, Int64Type, Schema, SchemaRef},
    record_batch::RecordBatch,
};
use quill_core::query::*;
use quill_drivers::{
    ConnectionPool,
    datafusion::{DatafusionConnectionPool, DatafusionSettings},
};
use quill_io::feather::*;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// Control whose status is set by the test; cancel flips it to `Cancelled`.
struct Stub {
    status: Mutex<ExecutionStatus>,
    cancelled: AtomicBool,
}

impl Stub {
    fn new(status: ExecutionStatus) -> Arc<Self> {
        Arc::new(Self {
            status: Mutex::new(status),
            cancelled: AtomicBool::new(false),
        })
    }
}

impl ExecutionControl for Stub {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Ok(mut status) = self.status.lock()
            && *status == ExecutionStatus::Running
        {
            *status = ExecutionStatus::Cancelled;
        }
    }

    fn snapshot(&self) -> ExecutionSnapshot {
        ExecutionSnapshot {
            status: self
                .status
                .lock()
                .map(|s| *s)
                .unwrap_or(ExecutionStatus::Failed),
            metrics: ClientMetrics::started(),
        }
    }
}

fn execution(
    events: Vec<quill_core::error::Result<QueryEvent>>,
    control: Arc<Stub>,
) -> QueryExecution {
    QueryExecution {
        id: QueryId::new(),
        events: Box::pin(futures::stream::iter(events)),
        control,
    }
}

fn int_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![Field::new("n", DataType::Int64, false)]))
}

fn ints(schema: &SchemaRef, values: std::ops::Range<i64>) -> TestResult<RecordBatch> {
    Ok(RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(Int64Array::from_iter_values(values))],
    )?)
}

async fn pool() -> TestResult<DatafusionConnectionPool> {
    Ok(DatafusionConnectionPool::connect(&DatafusionSettings {
        object_stores: vec![],
        retained_metrics: 2,
    })
    .await?)
}

async fn store_sql(
    sql: &str,
    limit: Option<ByteLimit>,
) -> TestResult<(tempfile::TempDir, StoredQuery)> {
    let dir = tempfile::tempdir()?;
    let execution = pool().await?.execute(QueryRequest::new(sql)).await?;
    let stored = FeatherStore::new(dir.path(), limit)
        .drain(execution, &SinkProgress::default())
        .await?;
    Ok((dir, stored))
}

fn only(stored: StoredQuery) -> TestResult<StoredResult> {
    Ok(stored
        .result_sets
        .into_iter()
        .next()
        .and_then(|set| set.data)
        .ok_or("no stored rows")?)
}

fn values(
    result: &StoredResult,
    range: std::ops::Range<u64>,
    column: usize,
) -> TestResult<Vec<Option<i64>>> {
    let mut out = Vec::new();
    for batch in result.read(range) {
        out.extend(batch?.column(column).as_primitive::<Int64Type>().iter());
    }
    Ok(out)
}

#[tokio::test]
async fn round_trips_values_and_nulls() -> TestResult {
    let (_dir, stored) =
        store_sql("SELECT CAST(value AS BIGINT) AS n FROM range(2500)", None).await?;
    assert!(stored.truncation.is_none());
    let result = only(stored)?;
    assert_eq!(result.rows(), 2500);
    assert!(!result.truncated());
    let expected: Vec<_> = (0..2500).map(Some).collect();
    assert_eq!(values(&result, 0..result.rows(), 0)?, expected);

    let (_dir, stored) = store_sql("SELECT CAST(NULL AS BIGINT) AS n", None).await?;
    assert_eq!(values(&only(stored)?, 0..1, 0)?, [None]);
    Ok(())
}

#[tokio::test]
async fn reads_across_batch_boundaries() -> TestResult {
    let (_dir, stored) =
        store_sql("SELECT CAST(value AS BIGINT) AS n FROM range(20000)", None).await?;
    let result = only(stored)?;
    let batches: Vec<_> = result.read(8190..8200).collect::<Result<_, _>>()?;
    assert!(
        batches.len() >= 2,
        "range should straddle DataFusion's 8192-row batches"
    );
    assert_eq!(
        values(&result, 8190..8200, 0)?,
        (8190..8200).map(Some).collect::<Vec<_>>()
    );
    assert_eq!(values(&result, 19995..30000, 0)?.len(), 5);
    Ok(())
}

#[tokio::test]
async fn keeps_field_metadata() -> TestResult {
    let metadata = HashMap::from([
        ("quill.native_type".to_string(), "int8".to_string()),
        ("quill.logical_type".to_string(), "integer".to_string()),
        ("quill.encoding".to_string(), "binary".to_string()),
    ]);
    let schema = Arc::new(Schema::new(vec![
        Field::new("n", DataType::Int64, false).with_metadata(metadata.clone()),
    ]));
    let events = vec![
        Ok(QueryEvent::Schema {
            result_set: 0,
            schema: schema.clone(),
        }),
        Ok(QueryEvent::Batch {
            result_set: 0,
            batch: ints(&schema, 0..5)?,
        }),
        Ok(QueryEvent::ResultSetComplete {
            result_set: 0,
            affected_rows: None,
        }),
        Ok(QueryEvent::Complete),
    ];
    let dir = tempfile::tempdir()?;
    let stored = FeatherStore::new(dir.path(), None)
        .drain(
            execution(events, Stub::new(ExecutionStatus::Completed)),
            &SinkProgress::default(),
        )
        .await?;
    let result = only(stored)?;
    assert_eq!(result.schema().field(0).metadata(), &metadata);
    for batch in result.read(1..3) {
        assert_eq!(batch?.schema().field(0).metadata(), &metadata);
    }
    Ok(())
}

#[tokio::test]
async fn cancelled_stream_is_truncated_and_readable() -> TestResult {
    let schema = int_schema();
    let events = vec![
        Ok(QueryEvent::Schema {
            result_set: 0,
            schema: schema.clone(),
        }),
        Ok(QueryEvent::Batch {
            result_set: 0,
            batch: ints(&schema, 0..10)?,
        }),
    ];
    let dir = tempfile::tempdir()?;
    let stored = FeatherStore::new(dir.path(), None)
        .drain(
            execution(events, Stub::new(ExecutionStatus::Cancelled)),
            &SinkProgress::default(),
        )
        .await?;
    assert!(matches!(stored.truncation, Some(Truncation::Cancelled)));
    let result = only(stored)?;
    assert!(result.truncated());
    assert_eq!(values(&result, 5..7, 0)?, [Some(5), Some(6)]);
    Ok(())
}

#[tokio::test]
async fn byte_limit_cancels_and_truncates() -> TestResult {
    let dir = tempfile::tempdir()?;
    let execution = pool()
        .await?
        .execute(QueryRequest::new(
            "SELECT CAST(value AS BIGINT) AS n FROM range(1000000)",
        ))
        .await?;
    let control = execution.control.clone();
    let progress = SinkProgress::default();
    let stored = FeatherStore::new(dir.path(), ByteLimit::new(64 * 1024))
        .drain(execution, &progress)
        .await?;
    assert!(matches!(stored.truncation, Some(Truncation::ByteLimit)));
    assert_eq!(control.snapshot().status, ExecutionStatus::Cancelled);
    let result = only(stored)?;
    assert!(result.truncated());
    assert!(result.rows() > 0 && result.rows() < 1_000_000);
    assert_eq!(progress.snapshot().rows_written, result.rows());
    assert!(progress.snapshot().bytes_written >= 64 * 1024);
    Ok(())
}

#[tokio::test]
async fn stream_error_keeps_earlier_rows() -> TestResult {
    let schema = int_schema();
    let events = vec![
        Ok(QueryEvent::Schema {
            result_set: 0,
            schema: schema.clone(),
        }),
        Ok(QueryEvent::Batch {
            result_set: 0,
            batch: ints(&schema, 0..4)?,
        }),
        Err(quill_core::error::Error::QueryExecution("boom".into())),
    ];
    let dir = tempfile::tempdir()?;
    let stored = FeatherStore::new(dir.path(), None)
        .drain(
            execution(events, Stub::new(ExecutionStatus::Failed)),
            &SinkProgress::default(),
        )
        .await?;
    assert!(matches!(stored.truncation, Some(Truncation::Failed(_))));
    assert_eq!(
        values(&only(stored)?, 0..10, 0)?,
        [Some(0), Some(1), Some(2), Some(3)]
    );
    Ok(())
}

#[tokio::test]
async fn result_sets_get_separate_files() -> TestResult {
    let schema = int_schema();
    let events = vec![
        Ok(QueryEvent::Schema {
            result_set: 0,
            schema: schema.clone(),
        }),
        Ok(QueryEvent::Batch {
            result_set: 0,
            batch: ints(&schema, 0..3)?,
        }),
        Ok(QueryEvent::ResultSetComplete {
            result_set: 0,
            affected_rows: None,
        }),
        Ok(QueryEvent::ResultSetComplete {
            result_set: 1,
            affected_rows: Some(7),
        }),
        Ok(QueryEvent::Schema {
            result_set: 2,
            schema: schema.clone(),
        }),
        Ok(QueryEvent::Batch {
            result_set: 2,
            batch: ints(&schema, 10..15)?,
        }),
        Ok(QueryEvent::ResultSetComplete {
            result_set: 2,
            affected_rows: None,
        }),
        Ok(QueryEvent::Complete),
    ];
    let dir = tempfile::tempdir()?;
    let stored = FeatherStore::new(dir.path(), None)
        .drain(
            execution(events, Stub::new(ExecutionStatus::Completed)),
            &SinkProgress::default(),
        )
        .await?;
    let [first, rowless, last] = &stored.result_sets[..] else {
        return Err("expected three result sets".into());
    };
    assert_eq!(rowless.affected_rows, Some(7));
    assert!(rowless.data.is_none());
    let (first, last) = (
        first.data.as_ref().ok_or("rows")?,
        last.data.as_ref().ok_or("rows")?,
    );
    assert_ne!(first.path(), last.path());
    assert_eq!((first.rows(), last.rows()), (3, 5));
    assert_eq!(values(last, 0..1, 0)?, [Some(10)]);
    Ok(())
}

#[cfg(feature = "datafusion")]
mod table {
    use super::*;
    use datafusion::{physical_plan::displayable, prelude::SessionContext};

    async fn registered(sql: &str) -> TestResult<(tempfile::TempDir, SessionContext)> {
        let (dir, stored) = store_sql(sql, None).await?;
        let ctx = SessionContext::new();
        ctx.register_table("r", Arc::new(FeatherTable::new(Arc::new(only(stored)?))?))?;
        Ok((dir, ctx))
    }

    async fn column(ctx: &SessionContext, sql: &str, name: &str) -> TestResult<Vec<i64>> {
        let mut out = Vec::new();
        for batch in ctx.sql(sql).await?.collect().await? {
            let array = arrow::compute::cast(
                batch.column_by_name(name).ok_or("column")?,
                &DataType::Int64,
            )?;
            out.extend(array.as_primitive::<Int64Type>().values().iter().copied());
        }
        Ok(out)
    }

    const RANGE: &str = "SELECT CAST(value AS BIGINT) AS n FROM range(20000)";

    #[tokio::test]
    async fn between_pages_rows() -> TestResult {
        let (_dir, ctx) = registered(RANGE).await?;
        let sql = "SELECT n, __row FROM r WHERE __row BETWEEN 8190 AND 8239";
        let expected: Vec<i64> = (8190..8240).collect();
        assert_eq!(column(&ctx, sql, "n").await?, expected);
        assert_eq!(column(&ctx, sql, "__row").await?, expected);
        assert_eq!(
            column(&ctx, "SELECT n FROM r WHERE __row >= 19998", "n").await?,
            [19998, 19999]
        );
        assert_eq!(
            column(&ctx, "SELECT n FROM r WHERE 3 > __row", "n").await?,
            [0, 1, 2]
        );
        Ok(())
    }

    #[tokio::test]
    async fn row_filter_is_pushed_down() -> TestResult {
        let (_dir, ctx) = registered(RANGE).await?;
        let plan = ctx
            .sql("SELECT * FROM r WHERE __row BETWEEN 8190 AND 8239")
            .await?
            .create_physical_plan()
            .await?;
        let shown = displayable(plan.as_ref()).indent(true).to_string();
        assert!(!shown.contains("FilterExec"), "{shown}");
        Ok(())
    }

    #[tokio::test]
    async fn data_filter_and_limit() -> TestResult {
        let (_dir, ctx) = registered(RANGE).await?;
        let evens = column(&ctx, "SELECT n FROM r WHERE n % 2 = 0 AND __row < 10", "n").await?;
        assert_eq!(evens, [0, 2, 4, 6, 8]);
        assert_eq!(
            column(&ctx, "SELECT n FROM r WHERE __row > 100 LIMIT 3", "n").await?,
            [101, 102, 103]
        );
        Ok(())
    }

    #[tokio::test]
    async fn rejects_row_column_collision() -> TestResult {
        let (_dir, stored) = store_sql("SELECT CAST(1 AS BIGINT) AS __row", None).await?;
        assert!(matches!(
            FeatherTable::new(Arc::new(only(stored)?)),
            Err(quill_io::error::Error::ReservedColumn(ROW_COLUMN))
        ));
        Ok(())
    }
}
