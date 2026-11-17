//! Postgres cells to [`Value`]s, in either wire format.
use std::sync::Arc;

use quill_core::value::{Column, Decimal, Row, Value};
use sqlx::{
    Column as _, Postgres, Row as _, TypeInfo, ValueRef,
    postgres::{PgColumn, PgRow, PgValueFormat},
    types::{BigDecimal, chrono, uuid::Uuid},
};

use crate::sql::RowDecoder;

#[derive(Debug, Clone, Copy)]
enum Scalar {
    Bool,
    Int2,
    Int4,
    Int8,
    Oid,
    Float4,
    Float8,
    Numeric,
    Text,
    Bytea,
    Date,
    Time,
    Timestamp,
    TimestampTz,
    Uuid,
    Json,
    Void,
    Other,
}

impl Scalar {
    fn parse(name: &str) -> Self {
        match name {
            "BOOL" => Self::Bool,
            "INT2" => Self::Int2,
            "INT4" => Self::Int4,
            "INT8" => Self::Int8,
            "OID" => Self::Oid,
            "FLOAT4" => Self::Float4,
            "FLOAT8" => Self::Float8,
            "NUMERIC" => Self::Numeric,
            "TEXT" | "VARCHAR" | "BPCHAR" | "NAME" => Self::Text,
            "BYTEA" => Self::Bytea,
            "DATE" => Self::Date,
            "TIME" => Self::Time,
            "TIMESTAMP" => Self::Timestamp,
            "TIMESTAMPTZ" => Self::TimestampTz,
            "UUID" => Self::Uuid,
            "JSON" | "JSONB" => Self::Json,
            "VOID" => Self::Void,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Kind {
    Scalar(Scalar),
    Array(Scalar),
}

impl Kind {
    fn of(column: &PgColumn) -> Self {
        let name = column.type_info().name();
        match name.strip_suffix("[]") {
            Some(element) => Self::Array(Scalar::parse(element)),
            None => Self::Scalar(Scalar::parse(name)),
        }
    }
}

#[derive(Default)]
pub(super) struct PgDecoder {
    kinds: Vec<Kind>,
}

impl PgDecoder {
    pub(super) fn columns(&mut self, columns: &[PgColumn]) -> Arc<[Column]> {
        self.kinds = columns.iter().map(Kind::of).collect();
        columns
            .iter()
            .map(|c| Column::new(c.name(), c.type_info().name().to_lowercase()))
            .collect()
    }
}

impl RowDecoder<Postgres> for PgDecoder {
    fn start(&mut self, columns: &[PgColumn]) -> Arc<[Column]> {
        self.columns(columns)
    }

    fn decode(&self, row: &PgRow) -> Row {
        self.kinds
            .iter()
            .enumerate()
            .map(|(i, kind)| decode(row, i, *kind).unwrap_or_else(|| raw(row, i)))
            .collect()
    }
}

/// The server's text, or bytes for binary values Quill does not decode.
fn raw(row: &PgRow, i: usize) -> Value {
    let Ok(value) = row.try_get_raw(i) else {
        return Value::Null;
    };
    if value.is_null() {
        return Value::Null;
    }
    match value.format() {
        PgValueFormat::Text => value
            .as_str()
            .map_or(Value::Null, |s| Value::Other(s.to_owned())),
        PgValueFormat::Binary => value
            .as_bytes()
            .map_or(Value::Null, |b| Value::Bytes(b.to_vec())),
    }
}

fn text(row: &PgRow, i: usize) -> Option<Option<&str>> {
    let value = row.try_get_raw(i).ok()?;
    if value.is_null() {
        return Some(None);
    }
    match value.format() {
        PgValueFormat::Text => value.as_str().ok().map(Some),
        PgValueFormat::Binary => None,
    }
}

fn decimal(value: &BigDecimal) -> Value {
    Decimal::parse(&value.to_plain_string())
        .map_or_else(|| Value::Other(value.to_string()), Value::Decimal)
}

/// `None` when the typed decode fails; the caller falls back to [`raw`].
fn decode(row: &PgRow, i: usize, kind: Kind) -> Option<Value> {
    macro_rules! get {
        ($t:ty, $f:expr) => {
            row.try_get_unchecked::<Option<$t>, _>(i)
                .ok()
                .map(|v| v.map_or(Value::Null, $f))
        };
    }
    macro_rules! list {
        ($t:ty, $f:expr) => {
            row.try_get_unchecked::<Option<Vec<Option<$t>>>, _>(i)
                .ok()
                .map(|v| {
                    v.map_or(Value::Null, |items| {
                        Value::List(
                            items
                                .into_iter()
                                .map(|item| item.map_or(Value::Null, $f))
                                .collect(),
                        )
                    })
                })
        };
    }
    match kind {
        Kind::Scalar(scalar) => match scalar {
            Scalar::Bool => get!(bool, Value::Bool),
            Scalar::Int2 => get!(i16, |v| Value::Int(v.into())),
            Scalar::Int4 => get!(i32, |v| Value::Int(v.into())),
            Scalar::Int8 => get!(i64, Value::Int),
            Scalar::Oid => get!(sqlx::postgres::types::Oid, |v| Value::UInt(v.0.into())),
            Scalar::Float4 => get!(f32, |v| Value::Float(v.into())),
            Scalar::Float8 => get!(f64, Value::Float),
            Scalar::Numeric => match text(row, i) {
                Some(text) => Some(text.map_or(Value::Null, Value::decimal_or_other)),
                None => get!(BigDecimal, |v| decimal(&v)),
            },
            Scalar::Text => get!(String, Value::Text),
            Scalar::Bytea => get!(Vec<u8>, Value::Bytes),
            Scalar::Date => get!(chrono::NaiveDate, Value::Date),
            Scalar::Time => get!(chrono::NaiveTime, Value::Time),
            Scalar::Timestamp => get!(chrono::NaiveDateTime, Value::Timestamp),
            Scalar::TimestampTz => get!(chrono::DateTime<chrono::Utc>, |v| {
                Value::TimestampTz(v.fixed_offset())
            }),
            Scalar::Uuid => get!(Uuid, Value::Uuid),
            Scalar::Json => match text(row, i) {
                Some(text) => Some(text.map_or(Value::Null, |s| Value::Json(s.to_owned()))),
                None => get!(sqlx::types::JsonValue, |v| Value::Json(v.to_string())),
            },
            Scalar::Void => Some(Value::Null),
            Scalar::Other => None,
        },
        Kind::Array(element) => match element {
            Scalar::Bool => list!(bool, Value::Bool),
            Scalar::Int2 => list!(i16, |v| Value::Int(v.into())),
            Scalar::Int4 => list!(i32, |v| Value::Int(v.into())),
            Scalar::Int8 => list!(i64, Value::Int),
            Scalar::Float4 => list!(f32, |v| Value::Float(v.into())),
            Scalar::Float8 => list!(f64, Value::Float),
            Scalar::Numeric => list!(BigDecimal, |v| decimal(&v)),
            Scalar::Text => list!(String, Value::Text),
            Scalar::Bytea => list!(Vec<u8>, Value::Bytes),
            Scalar::Date => list!(chrono::NaiveDate, Value::Date),
            Scalar::Time => list!(chrono::NaiveTime, Value::Time),
            Scalar::Timestamp => list!(chrono::NaiveDateTime, Value::Timestamp),
            Scalar::TimestampTz => list!(chrono::DateTime<chrono::Utc>, |v| {
                Value::TimestampTz(v.fixed_offset())
            }),
            Scalar::Uuid => list!(Uuid, Value::Uuid),
            Scalar::Json => list!(sqlx::types::JsonValue, |v| Value::Json(v.to_string())),
            Scalar::Oid | Scalar::Void | Scalar::Other => None,
        },
    }
}
