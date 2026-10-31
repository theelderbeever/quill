//! Single-statement Sqlite execution using native extended-protocol bindings.
use crate::execution::Control;
use std::sync::Arc;

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
    Column, Executor, Row, TypeInfo,
    sqlite::{SqliteArguments, SqliteRow},
};
use tokio::sync::mpsc;

use super::SqliteConnectionPool;

// Bounded conversion buffer; the event channel holds at most one additional batch.
const TARGET_BATCH_ROWS: usize = 1024;

/// Actionable failures from Sqlite query execution and Arrow conversion.
#[derive(Debug, thiserror::Error)]
pub enum ExecutionError {
    #[error("Sqlite query or value decoding failed")]
    Database(#[from] sqlx::Error),
    #[error("Arrow result conversion failed")]
    Arrow(#[from] arrow::error::ArrowError),
    #[error("unsupported Sqlite result type: {0}")]
    UnsupportedType(String),
    #[error("query result consumer closed the stream")]
    ConsumerClosed,
}

impl<T> From<mpsc::error::SendError<T>> for ExecutionError {
    fn from(_: mpsc::error::SendError<T>) -> Self {
        Self::ConsumerClosed
    }
}

type ExecutionResult<T> = std::result::Result<T, ExecutionError>;

impl SqliteConnectionPool {
    pub(super) fn start_execution(&self, request: QueryRequest) -> QueryExecution {
        let pool = self.inner.clone();
        crate::execution::start(move |sender, control| async move {
            Self::run(&pool, request, &sender, &control).await
        })
    }

    async fn run(
        pool: &sqlx::SqlitePool,
        request: QueryRequest,
        sender: &mpsc::Sender<Result<QueryEvent>>,
        control: &Control,
    ) -> Result<()> {
        if !request.bindings.named.is_empty() {
            return Err(Error::UnsupportedOperation("Sqlite named parameters"));
        }
        let mut query: sqlx::query::Query<'_, sqlx::Sqlite, SqliteArguments> =
            sqlx::query(&request.sql);
        for value in request.bindings.positional {
            query = match value {
                ParameterValue::Null => query.bind(Option::<String>::None),
                ParameterValue::Boolean(v) => query.bind(v),
                ParameterValue::Int64(v) => query.bind(v),
                ParameterValue::UInt64(v) => query.bind(i64::try_from(v).map_err(|_| {
                    Error::UnsupportedOperation("SQLite unsigned integer exceeds i64")
                })?),
                ParameterValue::Float64(v) => query.bind(v),
                ParameterValue::Text(v) => query.bind(v),
                ParameterValue::Binary(v) => query.bind(v),
                ParameterValue::Date(v) => query.bind(v),
                ParameterValue::Time(v) => query.bind(v),
                ParameterValue::Timestamp(v) => query.bind(v),
                ParameterValue::TimestampWithTimezone(v) => query.bind(v.naive_utc()),
                ParameterValue::Uuid(v) => query.bind(v.to_string()),
            };
        }
        let work = async {
            let mut connection = pool.acquire().await?;
            let description = (&mut *connection).describe(&request.sql).await?;
            let kinds = description
                .columns()
                .iter()
                .map(|column| ColumnKind::parse(column.type_info().name()))
                .collect::<ExecutionResult<Vec<_>>>()?;
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
                        control
                            .send(
                                sender,
                                QueryEvent::ResultSetComplete {
                                    result_set: 0,
                                    affected_rows,
                                },
                            )
                            .await
                            .map_err(|_| ExecutionError::ConsumerClosed)?;
                    }
                }
            }
            Ok::<_, ExecutionError>(())
        };
        work.await
            .map_err(|error| Error::QueryExecution(Box::new(error)))
    }

    async fn send_batch(
        rows: &mut Vec<SqliteRow>,
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
        control
            .send(
                sender,
                QueryEvent::Batch {
                    result_set: 0,
                    batch,
                },
            )
            .await
            .map_err(|_| ExecutionError::ConsumerClosed)?;
        rows.clear();
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum ColumnKind {
    Null,
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
}
impl ColumnKind {
    fn parse(name: &str) -> ExecutionResult<Self> {
        Ok(match name {
            "NULL" => Self::Null,
            "BOOLEAN" => Self::Bool,
            "INT2" => Self::Int16,
            "INT4" => Self::Int32,
            "INTEGER" => Self::Int64,
            "FLOAT4" => Self::Float32,
            "REAL" => Self::Float64,
            "TEXT" | "VARCHAR" | "BPCHAR" | "NAME" => Self::Text,
            "BLOB" => Self::Binary,
            "DATE" => Self::Date,
            "TIME" => Self::Time,
            "TIMESTAMP" => Self::Timestamp,
            "TIMESTAMPTZ" => Self::TimestampTz,
            "UUID" => Self::Uuid,

            _ => return Err(ExecutionError::UnsupportedType(name.into())),
        })
    }
    fn field(self, name: &str) -> Field {
        Field::new(name, self.data_type(), true)
    }

    fn data_type(self) -> DataType {
        match self {
            Self::Null => DataType::Null,
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
            Self::Uuid => DataType::Utf8,
        }
    }
    fn array(self, rows: &[SqliteRow], index: usize) -> ExecutionResult<ArrayRef> {
        Ok(match self {
            Self::Null => {
                use sqlx::ValueRef;
                for row in rows {
                    if !row.try_get_raw(index)?.is_null() {
                        return Err(ExecutionError::UnsupportedType(
                            "SQLite expression has no declared result type".into(),
                        ));
                    }
                }
                Arc::new(NullArray::new(rows.len()))
            }
            Self::Bool => crate::macros::primitive!(rows, index, bool, BooleanArray),
            Self::Int16 => crate::macros::primitive!(rows, index, i16, Int16Array),
            Self::Int32 => crate::macros::primitive!(rows, index, i32, Int32Array),
            Self::Int64 => crate::macros::primitive!(rows, index, i64, Int64Array),
            Self::Float32 => crate::macros::primitive!(rows, index, f32, Float32Array),
            Self::Float64 => crate::macros::primitive!(rows, index, f64, Float64Array),
            Self::Text => crate::macros::primitive!(rows, index, &str, StringArray),
            Self::Binary => crate::macros::primitive!(rows, index, &[u8], BinaryArray),
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
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ConnectionPool, sqlite::SqliteSettings};

    #[tokio::test]
    async fn binds_values_and_reports_affected_rows()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let pool = SqliteConnectionPool::connect(&SqliteSettings {
            path: ":memory:".into(),
            params: Default::default(),
        })
        .await?;
        sqlx::query("CREATE TABLE items (value INTEGER)")
            .execute(pool.inner())
            .await?;
        let mut request = QueryRequest::new("INSERT INTO items VALUES (?)");
        request.bindings.positional.push(ParameterValue::Int64(42));
        let mut execution = pool.execute(request).await?;
        while execution.events.try_next().await?.is_some() {}
        assert_eq!(execution.control.snapshot().metrics.affected_rows, Some(1));
        let value: i64 = sqlx::query_scalar("SELECT value FROM items")
            .fetch_one(pool.inner())
            .await?;
        assert_eq!(value, 42);
        Ok(())
    }

    #[tokio::test]
    async fn streams_rows_and_reports_completion()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let pool = SqliteConnectionPool::connect(&SqliteSettings {
            path: ":memory:".into(),
            params: Default::default(),
        })
        .await?;
        let mut execution = pool
            .execute(QueryRequest::new("SELECT 42 AS answer"))
            .await?;
        let mut rows = 0;
        let mut complete = false;
        while let Some(event) = execution.events.try_next().await? {
            match event {
                QueryEvent::Batch { batch, .. } => rows += batch.num_rows(),
                QueryEvent::Complete => complete = true,
                _ => {}
            }
        }
        assert_eq!(rows, 1);
        assert!(complete);
        assert_eq!(
            execution.control.snapshot().status,
            ExecutionStatus::Completed
        );
        assert_eq!(execution.control.snapshot().metrics.delivered_rows, 1);
        Ok(())
    }
}
