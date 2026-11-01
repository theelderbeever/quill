//! Single-statement Mysql execution using native extended-protocol bindings.
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
    Column, Executor, Row, SqlSafeStr, Statement, TypeInfo,
    mysql::{MySqlArguments, MySqlRow},
};
use tokio::sync::mpsc;

use super::MysqlConnectionPool;

// Bounded conversion buffer; the event channel holds at most one additional batch.
const TARGET_BATCH_ROWS: usize = 1024;

/// Actionable failures from Mysql query execution and Arrow conversion.
#[derive(Debug, thiserror::Error)]
pub enum ExecutionError {
    #[error("Mysql query or value decoding failed")]
    Database(#[from] sqlx::Error),
    #[error("Arrow result conversion failed")]
    Arrow(#[from] arrow::error::ArrowError),
    #[error("unsupported Mysql result type: {0}")]
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

impl MysqlConnectionPool {
    pub(super) fn start_execution(&self, request: QueryRequest) -> QueryExecution {
        let pool = self.inner.clone();
        crate::execution::start(move |sender, control| async move {
            Self::run(&pool, request, &sender, &control).await
        })
    }

    async fn run(
        pool: &sqlx::MySqlPool,
        request: QueryRequest,
        sender: &mpsc::Sender<Result<QueryEvent>>,
        control: &Control,
    ) -> Result<()> {
        if !request.bindings.named.is_empty() {
            return Err(Error::UnsupportedOperation("Mysql named parameters"));
        }
        // Requests contain SQL supplied by the caller; values remain separately bound.
        let sql = sqlx::AssertSqlSafe(Arc::new(request.sql)).into_sql_str();
        let mut query: sqlx::query::Query<'_, sqlx::MySql, MySqlArguments> =
            sqlx::query(sql.clone());
        for value in request.bindings.positional {
            query = match value {
                ParameterValue::Null => query.bind(Option::<String>::None),
                ParameterValue::Boolean(v) => query.bind(v),
                ParameterValue::Int64(v) => query.bind(v),
                ParameterValue::UInt64(v) => query.bind(v),
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
            let description = (&mut *connection).prepare(sql).await?;
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
                control
                    .send(
                        sender,
                        QueryEvent::Schema {
                            result_set: 0,
                            schema: schema.clone(),
                        },
                    )
                    .await
                    .map_err(|_| ExecutionError::ConsumerClosed)?;
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
        rows: &mut Vec<MySqlRow>,
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
    Geometry,
    Set,
    Bit,
    Decimal,
    Json,
    Null,
    Int8,
    UInt8,
    UInt16,
    UInt32,
    UInt64,
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
    Uuid,
}
impl ColumnKind {
    fn parse(name: &str) -> ExecutionResult<Self> {
        Ok(match name {
            "GEOMETRY" => Self::Geometry,
            "BIT" => Self::Bit,
            "SET" => Self::Set,
            "DECIMAL" => Self::Decimal,
            "JSON" => Self::Json,
            "NULL" => Self::Null,
            "TINYINT" => Self::Int8,
            "YEAR" => Self::UInt16,
            "TINYINT UNSIGNED" => Self::UInt8,
            "SMALLINT UNSIGNED" => Self::UInt16,
            "INT UNSIGNED" | "MEDIUMINT UNSIGNED" => Self::UInt32,
            "BIGINT UNSIGNED" => Self::UInt64,
            "BOOLEAN" => Self::Bool,
            "SMALLINT" => Self::Int16,
            "INT" | "MEDIUMINT" => Self::Int32,
            "BIGINT" => Self::Int64,
            "FLOAT" => Self::Float32,
            "DOUBLE" => Self::Float64,
            "TEXT" | "VARCHAR" | "CHAR" | "TINYTEXT" | "MEDIUMTEXT" | "LONGTEXT" | "ENUM" => {
                Self::Text
            }
            "BLOB" | "TINYBLOB" | "MEDIUMBLOB" | "LONGBLOB" | "BINARY" | "VARBINARY" => {
                Self::Binary
            }
            "DATE" => Self::Date,
            "TIME" => Self::Time,
            "TIMESTAMP" | "DATETIME" => Self::Timestamp,
            "UUID" => Self::Uuid,

            _ => return Err(ExecutionError::UnsupportedType(name.into())),
        })
    }
    fn field(self, name: &str) -> Field {
        let field = Field::new(name, self.data_type(), true);
        if matches!(
            self,
            Self::Decimal | Self::Json | Self::Geometry | Self::Set | Self::Bit
        ) {
            let logical = if matches!(self, Self::Geometry) {
                "geometry"
            } else if matches!(self, Self::Set) {
                "set"
            } else if matches!(self, Self::Bit) {
                "bit"
            } else if matches!(self, Self::Decimal) {
                "decimal"
            } else {
                "json"
            };
            field.with_metadata(
                [
                    ("quill.native_type".into(), logical.into()),
                    ("quill.logical_type".into(), logical.into()),
                    (
                        "quill.encoding".into(),
                        if matches!(self, Self::Geometry) {
                            "mysql_srid_wkb"
                        } else if matches!(self, Self::Bit) {
                            "unsigned_integer"
                        } else {
                            "text"
                        }
                        .into(),
                    ),
                ]
                .into(),
            )
        } else {
            field
        }
    }

    fn data_type(self) -> DataType {
        match self {
            Self::Geometry => DataType::Binary,
            Self::Set => DataType::Utf8,
            Self::Bit => DataType::UInt64,
            Self::Decimal | Self::Json => DataType::Utf8,
            Self::Null => DataType::Null,
            Self::Int8 => DataType::Int8,
            Self::UInt8 => DataType::UInt8,
            Self::UInt16 => DataType::UInt16,
            Self::UInt32 => DataType::UInt32,
            Self::UInt64 => DataType::UInt64,
            Self::Bool => DataType::Boolean,
            Self::Int16 => DataType::Int16,
            Self::Int32 => DataType::Int32,
            Self::Int64 => DataType::Int64,
            Self::Float32 => DataType::Float32,
            Self::Float64 => DataType::Float64,
            Self::Text => DataType::Utf8,
            Self::Binary => DataType::Binary,
            Self::Date => DataType::Date32,
            Self::Time => DataType::Duration(arrow::datatypes::TimeUnit::Microsecond),
            Self::Timestamp => DataType::Timestamp(arrow::datatypes::TimeUnit::Microsecond, None),
            Self::Uuid => DataType::Utf8,
        }
    }
    fn array(self, rows: &[MySqlRow], index: usize) -> ExecutionResult<ArrayRef> {
        Ok(match self {
            Self::Geometry => Arc::new(
                rows.iter()
                    .map(|row| row.try_get_unchecked::<Option<&[u8]>, _>(index))
                    .collect::<std::result::Result<BinaryArray, _>>()?,
            ),
            Self::Set => Arc::new(
                rows.iter()
                    .map(|row| row.try_get_unchecked::<Option<&str>, _>(index))
                    .collect::<std::result::Result<StringArray, _>>()?,
            ),
            Self::Bit => Arc::new(
                rows.iter()
                    .map(|row| row.try_get_unchecked::<Option<u64>, _>(index))
                    .collect::<std::result::Result<UInt64Array, _>>()?,
            ),
            Self::Decimal => {
                let values = rows
                    .iter()
                    .map(|row| {
                        row.try_get::<Option<sqlx::types::BigDecimal>, _>(index)
                            .map(|value| value.map(|value| value.to_string()))
                    })
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Arc::new(StringArray::from(values))
            }
            Self::Json => {
                let values = rows.iter().map(|row| row.try_get::<Option<sqlx::types::Json<Box<serde_json::value::RawValue>>>, _>(index).map(|value| value.map(|value| value.0.to_string()))).collect::<std::result::Result<Vec<_>, _>>()?;
                Arc::new(StringArray::from(values))
            }
            Self::Null => Arc::new(NullArray::new(rows.len())),
            Self::Int8 => crate::macros::primitive!(rows, index, i8, Int8Array),
            Self::UInt8 => crate::macros::primitive!(rows, index, u8, UInt8Array),
            Self::UInt16 => crate::macros::primitive!(rows, index, u16, UInt16Array),
            Self::UInt32 => crate::macros::primitive!(rows, index, u32, UInt32Array),
            Self::UInt64 => crate::macros::primitive!(rows, index, u64, UInt64Array),
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
            Self::Time => Arc::new(
                rows.iter()
                    .map(|row| {
                        row.try_get::<Option<sqlx::mysql::types::MySqlTime>, _>(index)
                            .map(|value| {
                                value.map(|value| {
                                    let micros = ((i64::from(value.hours()) * 60
                                        + i64::from(value.minutes()))
                                        * 60
                                        + i64::from(value.seconds()))
                                        * 1_000_000
                                        + i64::from(value.microseconds());
                                    if value.is_negative() { -micros } else { micros }
                                })
                            })
                    })
                    .collect::<std::result::Result<DurationMicrosecondArray, _>>()?,
            ),
            Self::Timestamp => Arc::new(
                rows.iter()
                    .map(|r| {
                        r.try_get::<Option<chrono::NaiveDateTime>, _>(index)
                            .map(|v| v.map(|t| t.and_utc().timestamp_micros()))
                    })
                    .collect::<std::result::Result<TimestampMicrosecondArray, _>>()?,
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
mod type_tests {
    use super::*;

    #[test]
    fn native_names_have_stable_arrow_schemas() -> ExecutionResult<()> {
        for name in [
            "TINYINT",
            "MEDIUMINT",
            "NULL",
            "YEAR",
            "BIT",
            "ENUM",
            "SET",
            "DECIMAL",
            "GEOMETRY",
            "JSON",
            "CHAR",
            "BINARY",
            "VARBINARY",
            "TINYBLOB",
            "MEDIUMBLOB",
            "LONGBLOB",
            "TINYTEXT",
            "MEDIUMTEXT",
            "LONGTEXT",
        ] {
            let kind = ColumnKind::parse(name)?;
            let array = kind.array(&[], 0)?;
            assert_eq!(array.data_type(), kind.field("value").data_type(), "{name}");
        }
        assert_eq!(
            ColumnKind::parse("TIME")?.data_type(),
            DataType::Duration(arrow::datatypes::TimeUnit::Microsecond)
        );
        let decimal = ColumnKind::parse("DECIMAL")?.field("amount");
        assert_eq!(decimal.metadata()["quill.logical_type"], "decimal");
        assert!(
            ColumnKind::parse("TEXT")?
                .field("value")
                .metadata()
                .is_empty()
        );
        Ok(())
    }
}
