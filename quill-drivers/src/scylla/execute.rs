use super::*;
use arrow::{
    array::*,
    datatypes::{DataType, Field, Schema},
    record_batch::RecordBatch,
};
use futures::TryStreamExt;
use quill_core::query::*;
use scylla::{
    frame::response::result::{ColumnType, NativeType},
    value::{CqlValue, Row},
};
use std::sync::Arc;
impl ScyllaConnectionPool {
    pub(super) async fn run(
        session: Arc<Session>,
        request: QueryRequest,
        sender: tokio::sync::mpsc::Sender<Result<QueryEvent>>,
        control: Arc<crate::execution::Control>,
    ) -> Result<()> {
        let statement = session
            .prepare(request.sql)
            .await
            .map_err(|e| Error::QueryExecution(Box::new(e)))?;
        let named = request
            .bindings
            .named
            .into_iter()
            .map(|(name, value)| Ok((name, Self::parameter(value)?)))
            .collect::<Result<std::collections::BTreeMap<_, _>>>()?;
        let positional = request
            .bindings
            .positional
            .into_iter()
            .map(Self::parameter)
            .collect::<Result<Vec<_>>>()?;
        let values = Parameters { named, positional };
        let pager = session
            .execute_iter(statement, values)
            .await
            .map_err(|e| Error::QueryExecution(Box::new(e)))?;
        let kinds = pager
            .column_specs()
            .iter()
            .map(|s| Kind::parse(s.typ()))
            .collect::<Result<Vec<_>>>()?;
        let schema = Arc::new(Schema::new(
            pager
                .column_specs()
                .iter()
                .zip(&kinds)
                .map(|(s, k)| Field::new(s.name(), k.data_type(), true))
                .collect::<Vec<_>>(),
        ));
        if !kinds.is_empty() {
            control
                .send(
                    &sender,
                    QueryEvent::Schema {
                        result_set: 0,
                        schema: schema.clone(),
                    },
                )
                .await?;
        }
        let mut stream = pager
            .rows_stream::<Row>()
            .map_err(|e| Error::QueryExecution(Box::new(e)))?;
        let mut rows = Vec::with_capacity(1024);
        while let Some(row) = stream
            .try_next()
            .await
            .map_err(|e| Error::QueryExecution(Box::new(e)))?
        {
            rows.push(row);
            if rows.len() == 1024 {
                Self::batch(&rows, &kinds, &schema, &sender, &control).await?;
                rows.clear();
            }
        }
        if !rows.is_empty() {
            Self::batch(&rows, &kinds, &schema, &sender, &control).await?;
        }
        control
            .send(
                &sender,
                QueryEvent::ResultSetComplete {
                    result_set: 0,
                    affected_rows: None,
                },
            )
            .await
    }
    async fn batch(
        rows: &[Row],
        kinds: &[Kind],
        schema: &Arc<Schema>,
        sender: &tokio::sync::mpsc::Sender<Result<QueryEvent>>,
        control: &crate::execution::Control,
    ) -> Result<()> {
        let arrays = kinds
            .iter()
            .enumerate()
            .map(|(i, k)| k.array(rows, i))
            .collect::<Vec<_>>();
        let batch = RecordBatch::try_new(schema.clone(), arrays)
            .map_err(|e| Error::QueryExecution(Box::new(e)))?;
        control
            .send(
                sender,
                QueryEvent::Batch {
                    result_set: 0,
                    batch,
                },
            )
            .await
    }
}
enum Kind {
    Text,
    Blob,
    Boolean,
    Int,
    BigInt,
    SmallInt,
    TinyInt,
    Float,
    Double,
}
impl Kind {
    fn parse(t: &ColumnType<'_>) -> Result<Self> {
        Ok(match t {
            ColumnType::Native(NativeType::Boolean) => Self::Boolean,
            ColumnType::Native(NativeType::Int) => Self::Int,
            ColumnType::Native(NativeType::BigInt) => Self::BigInt,
            ColumnType::Native(NativeType::SmallInt) => Self::SmallInt,
            ColumnType::Native(NativeType::TinyInt) => Self::TinyInt,
            ColumnType::Native(NativeType::Float) => Self::Float,
            ColumnType::Native(NativeType::Double) => Self::Double,
            ColumnType::Native(NativeType::Text | NativeType::Ascii) => Self::Text,
            ColumnType::Native(NativeType::Blob) => Self::Blob,
            _ => return Err(Error::UnsupportedOperation("Scylla result type")),
        })
    }
    fn data_type(&self) -> DataType {
        match self {
            Self::Text => DataType::Utf8,
            Self::Blob => DataType::Binary,
            Self::Boolean => DataType::Boolean,
            Self::Int => DataType::Int32,
            Self::BigInt => DataType::Int64,
            Self::SmallInt => DataType::Int16,
            Self::TinyInt => DataType::Int8,
            Self::Float => DataType::Float32,
            Self::Double => DataType::Float64,
        }
    }
    fn array(&self, rows: &[Row], index: usize) -> ArrayRef {
        match self {
            Self::Text => Arc::new(
                rows.iter()
                    .map(|r| match &r.columns[index] {
                        Some(CqlValue::Text(v) | CqlValue::Ascii(v)) => Some(v.as_str()),
                        _ => None,
                    })
                    .collect::<StringArray>(),
            ),
            Self::Blob => Arc::new(
                rows.iter()
                    .map(|r| match &r.columns[index] {
                        Some(CqlValue::Blob(v)) => Some(v.as_slice()),
                        _ => None,
                    })
                    .collect::<BinaryArray>(),
            ),
            Self::Boolean => Arc::new(
                rows.iter()
                    .map(|r| match &r.columns[index] {
                        Some(CqlValue::Boolean(v)) => Some(*v),
                        _ => None,
                    })
                    .collect::<BooleanArray>(),
            ),
            Self::Int => Arc::new(
                rows.iter()
                    .map(|r| match &r.columns[index] {
                        Some(CqlValue::Int(v)) => Some(*v),
                        _ => None,
                    })
                    .collect::<Int32Array>(),
            ),
            Self::BigInt => Arc::new(
                rows.iter()
                    .map(|r| match &r.columns[index] {
                        Some(CqlValue::BigInt(v)) => Some(*v),
                        _ => None,
                    })
                    .collect::<Int64Array>(),
            ),
            Self::SmallInt => Arc::new(
                rows.iter()
                    .map(|r| match &r.columns[index] {
                        Some(CqlValue::SmallInt(v)) => Some(*v),
                        _ => None,
                    })
                    .collect::<Int16Array>(),
            ),
            Self::TinyInt => Arc::new(
                rows.iter()
                    .map(|r| match &r.columns[index] {
                        Some(CqlValue::TinyInt(v)) => Some(*v),
                        _ => None,
                    })
                    .collect::<Int8Array>(),
            ),
            Self::Float => Arc::new(
                rows.iter()
                    .map(|r| match &r.columns[index] {
                        Some(CqlValue::Float(v)) => Some(*v),
                        _ => None,
                    })
                    .collect::<Float32Array>(),
            ),
            Self::Double => Arc::new(
                rows.iter()
                    .map(|r| match &r.columns[index] {
                        Some(CqlValue::Double(v)) => Some(*v),
                        _ => None,
                    })
                    .collect::<Float64Array>(),
            ),
        }
    }
}

impl ScyllaConnectionPool {
    fn parameter(value: ParameterValue) -> Result<Option<CqlValue>> {
        use chrono::Timelike;
        use scylla::value::{CqlDate, CqlTime, CqlTimestamp};
        Ok(Some(match value {
            ParameterValue::Null => return Ok(None),
            ParameterValue::Boolean(v) => CqlValue::Boolean(v),
            ParameterValue::Int64(v) => CqlValue::BigInt(v),
            ParameterValue::UInt64(v) => {
                CqlValue::BigInt(i64::try_from(v).map_err(|_| {
                    Error::UnsupportedOperation("Scylla unsigned integer exceeds i64")
                })?)
            }
            ParameterValue::Float64(v) => CqlValue::Double(v),
            ParameterValue::Text(v) => CqlValue::Text(v),
            ParameterValue::Binary(v) => CqlValue::Blob(v),
            #[allow(clippy::unwrap_used)]
            ParameterValue::Date(v) => CqlValue::Date(CqlDate(
                ((v - chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()).num_days()
                    + (1i64 << 31)) as u32,
            )),
            ParameterValue::Time(v) => CqlValue::Time(CqlTime(
                i64::from(v.num_seconds_from_midnight()) * 1_000_000_000
                    + i64::from(v.nanosecond()),
            )),
            ParameterValue::Timestamp(v) => {
                CqlValue::Timestamp(CqlTimestamp(v.and_utc().timestamp_millis()))
            }
            ParameterValue::TimestampWithTimezone(v) => {
                CqlValue::Timestamp(CqlTimestamp(v.timestamp_millis()))
            }
            ParameterValue::Uuid(v) => CqlValue::Uuid(v),
        }))
    }
}

// Delegate serialization and binding errors to Scylla while interleaving styles.
struct Parameters {
    named: std::collections::BTreeMap<String, Option<CqlValue>>,
    positional: Vec<Option<CqlValue>>,
}

impl scylla::serialize::row::SerializeRow for Parameters {
    fn serialize(
        &self,
        ctx: &scylla::serialize::row::RowSerializationContext<'_>,
        writer: &mut scylla::serialize::writers::RowWriter,
    ) -> std::result::Result<(), scylla::serialize::SerializationError> {
        use scylla::serialize::row::RowSerializationContext;
        if self.named.is_empty() {
            return self.positional.serialize(ctx, writer);
        }
        if self.positional.is_empty() {
            return self.named.serialize(ctx, writer);
        }
        let mut positional = self.positional.as_slice();
        for column in ctx.columns() {
            let column_ctx = RowSerializationContext::from_specs(std::slice::from_ref(column));
            if let Some(value) = self.named.get(column.name()) {
                std::slice::from_ref(value).serialize(&column_ctx, writer)?;
            } else {
                let (value, remaining) = positional.split_at(positional.len().min(1));
                value.serialize(&column_ctx, writer)?;
                positional = remaining;
            }
        }
        positional.serialize(&RowSerializationContext::empty(), writer)?;
        let unused = self
            .named
            .iter()
            .filter(|(name, _)| !ctx.columns().iter().any(|c| c.name() == name.as_str()))
            .map(|(name, value)| (name.as_str(), value))
            .collect::<std::collections::BTreeMap<_, _>>();
        unused.serialize(&RowSerializationContext::empty(), writer)
    }
    fn is_empty(&self) -> bool {
        self.named.is_empty() && self.positional.is_empty()
    }
}
