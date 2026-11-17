//! DataFusion's Arrow batches as [`Value`] rows.
use std::sync::Arc;

use arrow::{
    array::{Array, AsArray},
    datatypes::*,
    record_batch::RecordBatch,
    util::display::{ArrayFormatter, FormatOptions},
};
use quill_core::value::{Column, Row, Value};

pub(super) fn columns(schema: &Schema) -> Arc<[Column]> {
    schema
        .fields()
        .iter()
        .map(|f| Column::new(f.name(), f.data_type().to_string()))
        .collect()
}

/// Rows of `batch`; types without a [`Value`] keep Arrow's display text.
pub(super) fn rows(batch: &RecordBatch) -> Result<Vec<Row>, arrow::error::ArrowError> {
    let options = FormatOptions::default();
    let formatters = batch
        .columns()
        .iter()
        .map(|c| ArrayFormatter::try_new(c.as_ref(), &options))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((0..batch.num_rows())
        .map(|row| {
            batch
                .columns()
                .iter()
                .zip(&formatters)
                .map(|(array, formatter)| {
                    cell(array.as_ref(), row, || formatter.value(row).to_string())
                })
                .collect()
        })
        .collect())
}

fn cell(array: &dyn Array, row: usize, text: impl FnOnce() -> String) -> Value {
    if array.is_null(row) {
        return Value::Null;
    }
    macro_rules! primitive {
        ($t:ty, $f:expr) => {
            $f(array.as_primitive::<$t>().value(row))
        };
    }
    match array.data_type() {
        DataType::Boolean => Value::Bool(array.as_boolean().value(row)),
        DataType::Int8 => primitive!(Int8Type, |v: i8| Value::Int(v.into())),
        DataType::Int16 => primitive!(Int16Type, |v: i16| Value::Int(v.into())),
        DataType::Int32 => primitive!(Int32Type, |v: i32| Value::Int(v.into())),
        DataType::Int64 => primitive!(Int64Type, Value::Int),
        DataType::UInt8 => primitive!(UInt8Type, |v: u8| Value::UInt(v.into())),
        DataType::UInt16 => primitive!(UInt16Type, |v: u16| Value::UInt(v.into())),
        DataType::UInt32 => primitive!(UInt32Type, |v: u32| Value::UInt(v.into())),
        DataType::UInt64 => primitive!(UInt64Type, Value::UInt),
        DataType::Float32 => primitive!(Float32Type, |v: f32| Value::Float(v.into())),
        DataType::Float64 => primitive!(Float64Type, Value::Float),
        DataType::Utf8 => Value::Text(array.as_string::<i32>().value(row).to_owned()),
        DataType::LargeUtf8 => Value::Text(array.as_string::<i64>().value(row).to_owned()),
        DataType::Utf8View => Value::Text(array.as_string_view().value(row).to_owned()),
        DataType::Binary => Value::Bytes(array.as_binary::<i32>().value(row).to_vec()),
        DataType::LargeBinary => Value::Bytes(array.as_binary::<i64>().value(row).to_vec()),
        DataType::BinaryView => Value::Bytes(array.as_binary_view().value(row).to_vec()),
        DataType::FixedSizeBinary(_) => {
            Value::Bytes(array.as_fixed_size_binary().value(row).to_vec())
        }
        DataType::Decimal32(..)
        | DataType::Decimal64(..)
        | DataType::Decimal128(..)
        | DataType::Decimal256(..) => Value::decimal_or_other(&text()),
        DataType::Date32 => primitive!(Date32Type, |v| {
            Date32Type::to_naive_date_opt(v).map_or_else(|| Value::Other(text()), Value::Date)
        }),
        DataType::Timestamp(_, None) => {
            timestamp(array, row).unwrap_or_else(|| Value::Other(text()))
        }
        DataType::List(_) | DataType::LargeList(_) | DataType::FixedSizeList(..) => {
            list(array, row).unwrap_or_else(|| Value::Other(text()))
        }
        _ => Value::Other(text()),
    }
}

fn timestamp(array: &dyn Array, row: usize) -> Option<Value> {
    use arrow::temporal_conversions::as_datetime;
    let DataType::Timestamp(unit, _) = array.data_type() else {
        return None;
    };
    let naive = match unit {
        TimeUnit::Second => as_datetime::<TimestampSecondType>(
            array.as_primitive::<TimestampSecondType>().value(row),
        ),
        TimeUnit::Millisecond => as_datetime::<TimestampMillisecondType>(
            array.as_primitive::<TimestampMillisecondType>().value(row),
        ),
        TimeUnit::Microsecond => as_datetime::<TimestampMicrosecondType>(
            array.as_primitive::<TimestampMicrosecondType>().value(row),
        ),
        TimeUnit::Nanosecond => as_datetime::<TimestampNanosecondType>(
            array.as_primitive::<TimestampNanosecondType>().value(row),
        ),
    }?;
    Some(Value::Timestamp(naive))
}

fn list(array: &dyn Array, row: usize) -> Option<Value> {
    let items = match array.data_type() {
        DataType::List(_) => array.as_list::<i32>().value(row),
        DataType::LargeList(_) => array.as_list::<i64>().value(row),
        DataType::FixedSizeList(..) => array.as_fixed_size_list().value(row),
        _ => return None,
    };
    let options = FormatOptions::default();
    let formatter = ArrayFormatter::try_new(items.as_ref(), &options).ok()?;
    Some(Value::List(
        (0..items.len())
            .map(|i| cell(items.as_ref(), i, || formatter.value(i).to_string()))
            .collect(),
    ))
}
