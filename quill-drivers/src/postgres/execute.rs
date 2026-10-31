//! Single-statement Postgres execution using native extended-protocol bindings.
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

use arrow::{
    array::*,
    datatypes::{DataType, Field, Schema, SchemaRef},
    record_batch::RecordBatch,
};
use futures::TryStreamExt;
use quill_core::{
    error::{Error, Result},
    query::*,
};
use sqlx::{
    Column, Executor, Row, Statement, TypeInfo,
    postgres::{PgArguments, PgRow},
};
use tokio::{sync::mpsc, task::JoinHandle};

use super::PostgresConnectionPool;

// Bounded conversion buffer; the event channel holds at most one additional batch.
const TARGET_BATCH_ROWS: usize = 1024;

/// Actionable failures from Postgres query execution and Arrow conversion.
#[derive(Debug, thiserror::Error)]
pub enum ExecutionError {
    #[error("Postgres query or value decoding failed")]
    Database(#[from] sqlx::Error),
    #[error("Arrow result conversion failed")]
    Arrow(#[from] arrow::error::ArrowError),
    #[error("unsupported Postgres result type: {0}")]
    UnsupportedType(String),
    #[error("invalid numeric type modifier: {0}")]
    InvalidNumericModifier(i32),
    #[error("numeric precision {precision} and scale {scale} exceed Arrow's supported range")]
    DecimalRange { precision: i32, scale: i32 },
    #[error("numeric coefficient exceeds the Arrow decimal storage width")]
    DecimalOverflow,
    #[error("numeric cannot be represented at scale {scale} without rounding")]
    DecimalRounding { scale: i8 },
    #[error("query result consumer closed the stream")]
    ConsumerClosed,
}

impl<T> From<mpsc::error::SendError<T>> for ExecutionError {
    fn from(_: mpsc::error::SendError<T>) -> Self {
        Self::ConsumerClosed
    }
}

type ExecutionResult<T> = std::result::Result<T, ExecutionError>;

struct Control {
    start: Instant,
    state: Mutex<ExecutionSnapshot>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl Control {
    fn finish(&self, status: ExecutionStatus) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.status == ExecutionStatus::Running {
            state.status = status;
            state.metrics.elapsed = self.start.elapsed();
        }
    }
}

impl ExecutionControl for Control {
    fn cancel(&self) {
        self.finish(ExecutionStatus::Cancelled);
        if let Some(task) = self.task.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            task.abort();
        }
    }
    fn snapshot(&self) -> ExecutionSnapshot {
        let mut snapshot = self.state.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if snapshot.status == ExecutionStatus::Running {
            snapshot.metrics.elapsed = self.start.elapsed();
        }
        snapshot
    }
}

// Also finalize status when a task panics or is aborted outside cancellation.
struct TaskLifetime(Arc<Control>);
impl Drop for TaskLifetime {
    fn drop(&mut self) {
        self.0.finish(ExecutionStatus::Failed);
    }
}

impl PostgresConnectionPool {
    pub(super) fn start_execution(&self, request: QueryRequest) -> QueryExecution {
        let control = Arc::new(Control {
            start: Instant::now(),
            state: Mutex::new(ExecutionSnapshot {
                status: ExecutionStatus::Running,
                metrics: ExecutionMetrics {
                    elapsed: std::time::Duration::ZERO,
                    delivered_rows: 0,
                    delivered_batches: 0,
                    affected_rows: None,
                    server_elapsed: None,
                    scanned_bytes: None,
                    memory_bytes: None,
                    warnings: Vec::new(),
                },
            }),
            task: Mutex::new(None),
        });
        // One pending event bounds memory and keeps the producer tied to consumption.
        let (sender, receiver) = mpsc::channel(1);
        let pool = self.inner.clone();
        let task_control = control.clone();
        let task = tokio::spawn(async move {
            let _lifetime = TaskLifetime(task_control.clone());
            let result = Self::run(&pool, request, &sender, &task_control).await;
            match result {
                Ok(()) => {
                    task_control.finish(ExecutionStatus::Completed);
                    let _ = sender.send(Ok(QueryEvent::Complete)).await;
                }
                Err(error) => {
                    task_control.finish(ExecutionStatus::Failed);
                    let _ = sender.send(Err(error)).await;
                }
            }
        });
        *control.task.lock().unwrap_or_else(|e| e.into_inner()) = Some(task);
        let events = futures::stream::unfold(receiver, |mut receiver| async move {
            receiver.recv().await.map(|event| (event, receiver))
        });
        QueryExecution {
            id: QueryId::new(),
            events: Box::pin(events),
            control,
        }
    }

    async fn run(
        pool: &sqlx::PgPool,
        request: QueryRequest,
        sender: &mpsc::Sender<Result<QueryEvent>>,
        control: &Control,
    ) -> Result<()> {
        if !request.bindings.named.is_empty() {
            return Err(Error::UnsupportedOperation("Postgres named parameters"));
        }
        let mut query: sqlx::query::Query<'_, sqlx::Postgres, PgArguments> =
            sqlx::query(&request.sql);
        let mut parameter_types = Vec::with_capacity(request.bindings.positional.len());
        for value in request.bindings.positional {
            let type_name = match &value {
                ParameterValue::Null | ParameterValue::Text(_) => "text",
                ParameterValue::Boolean(_) => "bool",
                ParameterValue::Int64(_) => "int8",
                ParameterValue::UInt64(_) => "numeric",
                ParameterValue::Float64(_) => "float8",
                ParameterValue::Binary(_) => "bytea",
                ParameterValue::Date(_) => "date",
                ParameterValue::Time(_) => "time",
                ParameterValue::Timestamp(_) => "timestamp",
                ParameterValue::TimestampWithTimezone(_) => "timestamptz",
                ParameterValue::Uuid(_) => "uuid",
            };
            parameter_types.push(sqlx::postgres::PgTypeInfo::with_name(type_name));
            query = match value {
                ParameterValue::Null => query.bind(Option::<String>::None),
                ParameterValue::Boolean(v) => query.bind(v),
                ParameterValue::Int64(v) => query.bind(v),
                ParameterValue::UInt64(v) => query.bind(sqlx::types::BigDecimal::from(v)),
                ParameterValue::Float64(v) => query.bind(v),
                ParameterValue::Text(v) => query.bind(v),
                ParameterValue::Binary(v) => query.bind(v),
                ParameterValue::Date(v) => query.bind(v),
                ParameterValue::Time(v) => query.bind(v),
                ParameterValue::Timestamp(v) => query.bind(v),
                ParameterValue::TimestampWithTimezone(v) => query.bind(v),
                ParameterValue::Uuid(v) => query.bind(v),
            };
        }
        let work = async {
            let mut connection = pool.acquire().await?;
            let description = (&mut *connection)
                .prepare_with(&request.sql, &parameter_types)
                .await?;
            let mut kinds = Vec::with_capacity(description.columns().len());
            for column in description.columns() {
                kinds.push(if column.type_info().name() == "NUMERIC" {
                    match super::decimal::DecimalType::for_column(column, &mut connection).await? {
                        Some(decimal) => ColumnKind::Numeric(decimal),
                        None => ColumnKind::NumericText,
                    }
                } else {
                    ColumnKind::parse(column.type_info().name())?
                });
            }
            let schema = Arc::new(Schema::new(
                description
                    .columns()
                    .iter()
                    .zip(&kinds)
                    .map(|(column, kind)| kind.field(column.name()))
                    .collect::<Vec<_>>(),
            ));
            if !kinds.is_empty() {
                sender
                    .send(Ok(QueryEvent::Schema {
                        result_set: 0,
                        schema: schema.clone(),
                    }))
                    .await?;
            }
            let mut rows = Vec::new();
            // SQLx's executor stream carries both rows and command completion.
            let mut stream = (&mut *connection).fetch_many(query);
            while let Some(item) = stream.try_next().await? {
                match item {
                    sqlx::Either::Right(row) => {
                        rows.push(row);
                        if rows.len() == TARGET_BATCH_ROWS {
                            Self::send_batch(&mut rows, &kinds, &schema, sender, control).await?;
                        }
                    }
                    sqlx::Either::Left(result) => {
                        if !rows.is_empty() {
                            Self::send_batch(&mut rows, &kinds, &schema, sender, control).await?;
                        }
                        let affected_rows = if kinds.is_empty() {
                            Some(result.rows_affected())
                        } else {
                            None
                        };
                        if let Some(affected) = affected_rows {
                            let mut state = control.state.lock().unwrap_or_else(|e| e.into_inner());
                            if state.status == ExecutionStatus::Running {
                                state.metrics.affected_rows = Some(affected);
                            }
                        }
                        sender
                            .send(Ok(QueryEvent::ResultSetComplete {
                                result_set: 0,
                                affected_rows,
                            }))
                            .await?;
                    }
                }
            }
            Ok::<_, ExecutionError>(())
        };
        work.await
            .map_err(|error| Error::QueryExecution(Box::new(error)))
    }

    async fn send_batch(
        rows: &mut Vec<PgRow>,
        kinds: &[ColumnKind],
        schema: &SchemaRef,
        sender: &mpsc::Sender<Result<QueryEvent>>,
        control: &Control,
    ) -> ExecutionResult<()> {
        let arrays = kinds
            .iter()
            .enumerate()
            .map(|(index, kind)| kind.array(rows, index))
            .collect::<ExecutionResult<Vec<_>>>()?;
        let batch = RecordBatch::try_new(schema.clone(), arrays)?;
        let count = batch.num_rows() as u64;
        let permit = sender.reserve().await?;
        {
            let mut state = control.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.status != ExecutionStatus::Running {
                return Ok(());
            }
            state.metrics.delivered_rows += count;
            state.metrics.delivered_batches += 1;
            permit.send(Ok(QueryEvent::Batch {
                result_set: 0,
                batch,
            }));
        }
        rows.clear();
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum ColumnKind {
    Bool,
    Int16,
    Int32,
    Int64,
    Float32,
    Float64,
    Text,
    Binary,
    Date,
    Time,
    Timestamp,
    TimestampTz,
    Uuid,
    Numeric(super::decimal::DecimalType),
    NumericText,
}
impl ColumnKind {
    fn parse(name: &str) -> ExecutionResult<Self> {
        Ok(match name {
            "BOOL" => Self::Bool,
            "INT2" => Self::Int16,
            "INT4" => Self::Int32,
            "INT8" => Self::Int64,
            "FLOAT4" => Self::Float32,
            "FLOAT8" => Self::Float64,
            "TEXT" | "VARCHAR" | "BPCHAR" | "NAME" => Self::Text,
            "BYTEA" => Self::Binary,
            "DATE" => Self::Date,
            "TIME" => Self::Time,
            "TIMESTAMP" => Self::Timestamp,
            "TIMESTAMPTZ" => Self::TimestampTz,
            "UUID" => Self::Uuid,

            _ => return Err(ExecutionError::UnsupportedType(name.into())),
        })
    }
    fn field(self, name: &str) -> Field {
        let field = Field::new(name, self.data_type(), true);
        if matches!(self, Self::NumericText) {
            field.with_metadata(
                [
                    ("quill.native_type".into(), "numeric".into()),
                    ("quill.logical_type".into(), "decimal".into()),
                    ("quill.encoding".into(), "text".into()),
                ]
                .into(),
            )
        } else {
            field
        }
    }

    fn data_type(self) -> DataType {
        match self {
            Self::Bool => DataType::Boolean,
            Self::Int16 => DataType::Int16,
            Self::Int32 => DataType::Int32,
            Self::Int64 => DataType::Int64,
            Self::Float32 => DataType::Float32,
            Self::Float64 => DataType::Float64,
            Self::Text => DataType::Utf8,
            Self::Binary => DataType::Binary,
            Self::Date => DataType::Date32,
            Self::Time => DataType::Time64(arrow::datatypes::TimeUnit::Nanosecond),
            Self::Timestamp => DataType::Timestamp(arrow::datatypes::TimeUnit::Microsecond, None),
            Self::TimestampTz => {
                DataType::Timestamp(arrow::datatypes::TimeUnit::Microsecond, Some("UTC".into()))
            }
            Self::Uuid | Self::NumericText => DataType::Utf8,
            Self::Numeric(decimal) => decimal.data_type(),
        }
    }
    fn array(self, rows: &[PgRow], index: usize) -> ExecutionResult<ArrayRef> {
        macro_rules! primitive {
            ($ty:ty, $array:ty) => {
                Arc::new(
                    rows.iter()
                        .map(|r| r.try_get::<Option<$ty>, _>(index))
                        .collect::<std::result::Result<$array, _>>()?,
                ) as ArrayRef
            };
        }
        Ok(match self {
            Self::Bool => primitive!(bool, BooleanArray),
            Self::Int16 => primitive!(i16, Int16Array),
            Self::Int32 => primitive!(i32, Int32Array),
            Self::Int64 => primitive!(i64, Int64Array),
            Self::Float32 => primitive!(f32, Float32Array),
            Self::Float64 => primitive!(f64, Float64Array),
            Self::Text => primitive!(&str, StringArray),
            Self::Binary => primitive!(&[u8], BinaryArray),
            Self::Date => {
                let epoch = chrono::NaiveDate::default();
                Arc::new(
                    rows.iter()
                        .map(|r| {
                            r.try_get::<Option<chrono::NaiveDate>, _>(index)
                                .map(|v| v.map(|d| (d - epoch).num_days() as i32))
                        })
                        .collect::<std::result::Result<Date32Array, _>>()?,
                )
            }
            Self::Time => {
                use chrono::Timelike;
                Arc::new(
                    rows.iter()
                        .map(|r| {
                            r.try_get::<Option<chrono::NaiveTime>, _>(index).map(|v| {
                                v.map(|t| {
                                    i64::from(t.num_seconds_from_midnight()) * 1_000_000_000
                                        + i64::from(t.nanosecond())
                                })
                            })
                        })
                        .collect::<std::result::Result<Time64NanosecondArray, _>>()?,
                )
            }
            Self::Timestamp => Arc::new(
                rows.iter()
                    .map(|r| {
                        r.try_get::<Option<chrono::NaiveDateTime>, _>(index)
                            .map(|v| v.map(|t| t.and_utc().timestamp_micros()))
                    })
                    .collect::<std::result::Result<TimestampMicrosecondArray, _>>()?,
            ),
            Self::TimestampTz => Arc::new(
                rows.iter()
                    .map(|r| {
                        r.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>(index)
                            .map(|v| v.map(|t| t.timestamp_micros()))
                    })
                    .collect::<std::result::Result<TimestampMicrosecondArray, _>>()?
                    .with_timezone("UTC"),
            ),
            Self::Uuid => {
                let values = rows
                    .iter()
                    .map(|r| {
                        r.try_get::<Option<sqlx::types::Uuid>, _>(index)
                            .map(|v| v.map(|v| v.to_string()))
                    })
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Arc::new(StringArray::from_iter(values.iter().map(|v| v.as_deref())))
            }
            Self::Numeric(decimal) => decimal.array(rows, index)?,
            Self::NumericText => {
                let values = rows
                    .iter()
                    .map(|row| {
                        row.try_get::<Option<sqlx::types::BigDecimal>, _>(index)
                            .map(|value| value.map(|value| value.to_string()))
                    })
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Arc::new(StringArray::from_iter(
                    values.iter().map(|value| value.as_deref()),
                ))
            }
        })
    }
}
