use arrow::{
    array::*,
    buffer::{NullBuffer, OffsetBuffer, ScalarBuffer},
    datatypes::{DataType, Field, Fields},
};
use quill_core::error::{Error, Result};
use scylla::{
    frame::response::result::{CollectionType, ColumnType, NativeType},
    value::{CqlValue, Row},
};
use std::sync::Arc;

#[derive(Debug)]
pub(super) enum Kind {
    List(Box<Kind>),
    Set(Box<Kind>),
    Map(Box<Kind>, Box<Kind>),
    Tuple(Vec<Kind>),
    Udt {
        keyspace: String,
        name: String,
        fields: Vec<(String, Kind)>,
    },
    Vector(Box<Kind>, u16),
    Native(NativeType),
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
    pub(super) fn parse(t: &ColumnType<'_>) -> Result<Self> {
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
            ColumnType::Native(native) => Self::Native(native.clone()),
            ColumnType::Collection { typ, .. } => match typ {
                CollectionType::List(item) => Self::List(Box::new(Self::parse(item)?)),
                CollectionType::Set(item) => Self::Set(Box::new(Self::parse(item)?)),
                CollectionType::Map(key, value) => {
                    Self::Map(Box::new(Self::parse(key)?), Box::new(Self::parse(value)?))
                }
                _ => return Err(Self::unsupported(t)),
            },
            ColumnType::Tuple(items) => {
                Self::Tuple(items.iter().map(Self::parse).collect::<Result<_>>()?)
            }
            ColumnType::Vector { typ, dimensions } => {
                Self::Vector(Box::new(Self::parse(typ)?), *dimensions)
            }
            ColumnType::UserDefinedType { definition, .. } => Self::Udt {
                keyspace: definition.keyspace.to_string(),
                name: definition.name.to_string(),
                fields: definition
                    .field_types
                    .iter()
                    .map(|(name, typ)| Ok((name.to_string(), Self::parse(typ)?)))
                    .collect::<Result<_>>()?,
            },
            _ => {
                return Err(Error::QueryExecution(Box::new(
                    ConversionError::Unsupported(format!("{t:?}")),
                )));
            }
        })
    }
    fn data_type(&self) -> DataType {
        match self {
            Self::List(item) | Self::Set(item) => {
                DataType::List(Arc::new(item.field("item").with_nullable(false)))
            }
            Self::Map(key, value) => DataType::Map(Arc::new(Self::map_entries(key, value)), false),
            Self::Tuple(_) | Self::Udt { .. } => DataType::Struct(self.struct_fields()),
            Self::Vector(item, size) => {
                DataType::FixedSizeList(Arc::new(item.field("item")), i32::from(*size))
            }
            Self::Native(native) => match native {
                NativeType::Counter => DataType::Int64,
                NativeType::Date => DataType::Date32,
                NativeType::Time => DataType::Time64(arrow::datatypes::TimeUnit::Nanosecond),
                NativeType::Timestamp => {
                    DataType::Timestamp(arrow::datatypes::TimeUnit::Millisecond, Some("UTC".into()))
                }
                NativeType::Duration => {
                    DataType::Interval(arrow::datatypes::IntervalUnit::MonthDayNano)
                }
                _ => DataType::Utf8,
            },
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
    pub(super) fn field(&self, name: &str) -> Field {
        let field = Field::new(name, self.data_type(), true);
        if let Self::Native(native) = self
            && self.data_type() == DataType::Utf8
        {
            let logical = format!("{native:?}").to_lowercase();
            return field.with_metadata(
                [
                    ("quill.native_type".into(), logical.clone()),
                    ("quill.logical_type".into(), logical),
                    ("quill.encoding".into(), "text".into()),
                ]
                .into(),
            );
        }
        let semantic = match self {
            Self::Set(_) => Some(("set".to_string(), "set", "arrow_list")),
            Self::Tuple(_) => Some(("tuple".to_string(), "tuple", "arrow_struct")),
            Self::Udt { keyspace, name, .. } => {
                Some((format!("{keyspace}.{name}"), "udt", "arrow_struct"))
            }
            _ => None,
        };
        if let Some((native, logical, encoding)) = semantic {
            field.with_metadata(
                [
                    ("quill.native_type".into(), native),
                    ("quill.logical_type".into(), logical.into()),
                    ("quill.encoding".into(), encoding.into()),
                ]
                .into(),
            )
        } else {
            field
        }
    }
    pub(super) fn array(&self, rows: &[Row], index: usize) -> Result<ArrayRef> {
        self.values_array(Values::Rows(rows, index), index)
    }
    fn values_array(&self, values: Values<'_>, index: usize) -> Result<ArrayRef> {
        if matches!(
            self,
            Self::List(_)
                | Self::Set(_)
                | Self::Map(_, _)
                | Self::Tuple(_)
                | Self::Udt { .. }
                | Self::Vector(_, _)
        ) {
            return self
                .nested_array(values, index)
                .map_err(|error| Error::QueryExecution(Box::new(error)));
        }
        for value in values.iter().flatten() {
            let valid = match (self, value) {
                (Self::Text, CqlValue::Text(_) | CqlValue::Ascii(_))
                | (Self::Blob, CqlValue::Blob(_))
                | (Self::Boolean, CqlValue::Boolean(_))
                | (Self::Int, CqlValue::Int(_))
                | (Self::BigInt, CqlValue::BigInt(_))
                | (Self::SmallInt, CqlValue::SmallInt(_))
                | (Self::TinyInt, CqlValue::TinyInt(_))
                | (Self::Float, CqlValue::Float(_))
                | (Self::Double, CqlValue::Double(_)) => true,
                (Self::Native(native), value) => matches!(
                    (native, value),
                    (NativeType::Counter, CqlValue::Counter(_))
                        | (NativeType::Date, CqlValue::Date(_))
                        | (NativeType::Time, CqlValue::Time(_))
                        | (NativeType::Timestamp, CqlValue::Timestamp(_))
                        | (NativeType::Duration, CqlValue::Duration(_))
                        | (NativeType::Inet, CqlValue::Inet(_))
                        | (NativeType::Uuid, CqlValue::Uuid(_))
                        | (NativeType::Timeuuid, CqlValue::Timeuuid(_))
                        | (NativeType::Decimal, CqlValue::Decimal(_))
                        | (NativeType::Varint, CqlValue::Varint(_))
                ),
                _ => false,
            };
            if !valid {
                return Err(Error::QueryExecution(Box::new(ConversionError::Mismatch {
                    column: index,
                    expected: format!("{self:?}"),
                })));
            }
        }
        Ok(match self {
            Self::List(_)
            | Self::Set(_)
            | Self::Map(_, _)
            | Self::Tuple(_)
            | Self::Udt { .. }
            | Self::Vector(_, _) => unreachable!("nested conversion dispatched above"),
            Self::Native(native) => {
                macro_rules! primitive {
                    ($variant:ident, $array:ty, $convert:expr) => {
                        Arc::new(
                            values
                                .iter()
                                .map(|value| match value {
                                    Some(CqlValue::$variant(value)) => Some(($convert)(value)),
                                    _ => None,
                                })
                                .collect::<$array>(),
                        ) as ArrayRef
                    };
                }
                match native {
                    NativeType::Counter => {
                        primitive!(Counter, Int64Array, |v: &scylla::value::Counter| v.0)
                    }
                    NativeType::Date => primitive!(
                        Date,
                        Date32Array,
                        |v: &scylla::value::CqlDate| (i64::from(v.0) - (1i64 << 31)) as i32
                    ),
                    NativeType::Time => {
                        primitive!(Time, Time64NanosecondArray, |v: &scylla::value::CqlTime| v
                            .0)
                    }
                    NativeType::Timestamp => Arc::new(
                        values
                            .iter()
                            .map(|value| match value {
                                Some(CqlValue::Timestamp(value)) => Some(value.0),
                                _ => None,
                            })
                            .collect::<TimestampMillisecondArray>()
                            .with_timezone("UTC"),
                    ),
                    NativeType::Duration => primitive!(
                        Duration,
                        IntervalMonthDayNanoArray,
                        |v: &scylla::value::CqlDuration| {
                            arrow::datatypes::IntervalMonthDayNano::new(
                                v.months,
                                v.days,
                                v.nanoseconds,
                            )
                        }
                    ),
                    _ => {
                        let values = values
                            .iter()
                            .map(|value| {
                                value.map(|value| match value {
                                    CqlValue::Inet(v) => v.to_string(),
                                    CqlValue::Uuid(v) => v.to_string(),
                                    CqlValue::Timeuuid(v) => v.to_string(),
                                    CqlValue::Varint(v) => {
                                        num_bigint::BigInt::from_signed_bytes_be(
                                            v.as_signed_bytes_be_slice(),
                                        )
                                        .to_string()
                                    }
                                    CqlValue::Decimal(v) => {
                                        let (bytes, scale) =
                                            v.as_signed_be_bytes_slice_and_exponent();
                                        sqlx::types::BigDecimal::new(
                                            num_bigint::BigInt::from_signed_bytes_be(bytes),
                                            i64::from(scale),
                                        )
                                        .to_string()
                                    }
                                    _ => unreachable!("validated native value"),
                                })
                            })
                            .collect::<Vec<_>>();
                        Arc::new(StringArray::from(values))
                    }
                }
            }
            Self::Text => Arc::new(
                values
                    .iter()
                    .map(|value| match value {
                        Some(CqlValue::Text(v) | CqlValue::Ascii(v)) => Some(v.as_str()),
                        _ => None,
                    })
                    .collect::<StringArray>(),
            ),
            Self::Blob => Arc::new(
                values
                    .iter()
                    .map(|value| match value {
                        Some(CqlValue::Blob(v)) => Some(v.as_slice()),
                        _ => None,
                    })
                    .collect::<BinaryArray>(),
            ),
            Self::Boolean => Arc::new(
                values
                    .iter()
                    .map(|value| match value {
                        Some(CqlValue::Boolean(v)) => Some(*v),
                        _ => None,
                    })
                    .collect::<BooleanArray>(),
            ),
            Self::Int => Arc::new(
                values
                    .iter()
                    .map(|value| match value {
                        Some(CqlValue::Int(v)) => Some(*v),
                        _ => None,
                    })
                    .collect::<Int32Array>(),
            ),
            Self::BigInt => Arc::new(
                values
                    .iter()
                    .map(|value| match value {
                        Some(CqlValue::BigInt(v)) => Some(*v),
                        _ => None,
                    })
                    .collect::<Int64Array>(),
            ),
            Self::SmallInt => Arc::new(
                values
                    .iter()
                    .map(|value| match value {
                        Some(CqlValue::SmallInt(v)) => Some(*v),
                        _ => None,
                    })
                    .collect::<Int16Array>(),
            ),
            Self::TinyInt => Arc::new(
                values
                    .iter()
                    .map(|value| match value {
                        Some(CqlValue::TinyInt(v)) => Some(*v),
                        _ => None,
                    })
                    .collect::<Int8Array>(),
            ),
            Self::Float => Arc::new(
                values
                    .iter()
                    .map(|value| match value {
                        Some(CqlValue::Float(v)) => Some(*v),
                        _ => None,
                    })
                    .collect::<Float32Array>(),
            ),
            Self::Double => Arc::new(
                values
                    .iter()
                    .map(|value| match value {
                        Some(CqlValue::Double(v)) => Some(*v),
                        _ => None,
                    })
                    .collect::<Float64Array>(),
            ),
        })
    }
}

#[derive(Clone, Copy)]
enum Values<'a> {
    Rows(&'a [Row], usize),
    Nested(&'a [Option<&'a CqlValue>]),
}
impl<'a> Values<'a> {
    fn len(self) -> usize {
        match self {
            Self::Rows(rows, _) => rows.len(),
            Self::Nested(values) => values.len(),
        }
    }
    fn iter(self) -> impl Iterator<Item = Option<&'a CqlValue>> + Clone {
        (0..self.len()).map(move |index| match self {
            Self::Rows(rows, column) => rows[index].columns[column].as_ref(),
            Self::Nested(values) => values[index],
        })
    }
}

impl Kind {
    fn unsupported(typ: &ColumnType<'_>) -> Error {
        Error::QueryExecution(Box::new(ConversionError::Unsupported(format!("{typ:?}"))))
    }
    fn map_entries(key: &Self, value: &Self) -> Field {
        Field::new(
            "entries",
            DataType::Struct(
                vec![
                    key.field("key").with_nullable(false),
                    value.field("value").with_nullable(false),
                ]
                .into(),
            ),
            false,
        )
    }
    fn struct_fields(&self) -> Fields {
        match self {
            Self::Tuple(items) => items
                .iter()
                .enumerate()
                .map(|(i, kind)| kind.field(&format!("item_{i}")))
                .collect(),
            Self::Udt { fields, .. } => {
                fields.iter().map(|(name, kind)| kind.field(name)).collect()
            }
            _ => unreachable!("struct fields requested for a scalar"),
        }
    }
    fn nested_array(
        &self,
        values: Values<'_>,
        column: usize,
    ) -> std::result::Result<ArrayRef, ConversionError> {
        let invalid = |reason| ConversionError::Nested { column, reason };
        let validity = NullBuffer::from(values.iter().map(|v| v.is_some()).collect::<Vec<_>>());
        match self {
            Self::List(item) | Self::Set(item) => {
                let mut children = Vec::new();
                let mut offsets = vec![0i32];
                for value in values.iter() {
                    match (self, value) {
                        (_, None) => {}
                        (Self::List(_), Some(CqlValue::List(items)))
                        | (Self::Set(_), Some(CqlValue::Set(items))) => {
                            children.extend(items.iter().map(Some))
                        }
                        _ => {
                            return Err(invalid("expected list or set matching the declared type"));
                        }
                    }
                    offsets.push(
                        i32::try_from(children.len())
                            .map_err(|_| invalid("list exceeds Arrow's i32 offset range"))?,
                    );
                }
                let array = item.values_array(Values::Nested(&children), column)?;
                Ok(Arc::new(ListArray::try_new(
                    Arc::new(item.field("item").with_nullable(false)),
                    OffsetBuffer::new(ScalarBuffer::from(offsets)),
                    array,
                    Some(validity),
                )?))
            }
            Self::Map(key, value) => {
                let mut keys = Vec::new();
                let mut items = Vec::new();
                let mut offsets = vec![0i32];
                for parent in values.iter() {
                    match parent {
                        None => {}
                        Some(CqlValue::Map(entries)) => {
                            for (key, value) in entries {
                                keys.push(Some(key));
                                items.push(Some(value));
                            }
                        }
                        _ => return Err(invalid("expected map")),
                    }
                    offsets.push(
                        i32::try_from(keys.len())
                            .map_err(|_| invalid("map exceeds Arrow's i32 offset range"))?,
                    );
                }
                let entry_field = Self::map_entries(key, value);
                let DataType::Struct(fields) = entry_field.data_type() else {
                    unreachable!()
                };
                let entries = StructArray::try_new(
                    fields.clone(),
                    vec![
                        key.values_array(Values::Nested(&keys), column)?,
                        value.values_array(Values::Nested(&items), column)?,
                    ],
                    None,
                )?;
                Ok(Arc::new(MapArray::try_new(
                    Arc::new(entry_field),
                    OffsetBuffer::new(offsets.into()),
                    entries,
                    Some(validity),
                    false,
                )?))
            }
            Self::Vector(item, size) => {
                let mut children = Vec::new();
                for value in values.iter() {
                    match value {
                        None => children.extend(std::iter::repeat_n(None, usize::from(*size))),
                        Some(CqlValue::Vector(items)) if items.len() == usize::from(*size) => {
                            children.extend(items.iter().map(Some))
                        }
                        _ => return Err(invalid("expected vector with the declared dimensions")),
                    }
                }
                Ok(Arc::new(FixedSizeListArray::try_new_with_length(
                    Arc::new(item.field("item")),
                    i32::from(*size),
                    item.values_array(Values::Nested(&children), column)?,
                    Some(validity),
                    values.len(),
                )?))
            }
            Self::Tuple(kinds) => {
                for value in values.iter().flatten() {
                    if !matches!(value, CqlValue::Tuple(items) if items.len() == kinds.len()) {
                        return Err(invalid("expected tuple with the declared field count"));
                    }
                }
                let arrays = kinds
                    .iter()
                    .enumerate()
                    .map(|(index, kind)| {
                        let children = values
                            .iter()
                            .map(|value| match value {
                                Some(CqlValue::Tuple(items)) => items[index].as_ref(),
                                _ => None,
                            })
                            .collect::<Vec<_>>();
                        kind.values_array(Values::Nested(&children), column)
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(Arc::new(StructArray::try_new_with_length(
                    self.struct_fields(),
                    arrays,
                    Some(validity),
                    values.len(),
                )?))
            }
            Self::Udt {
                keyspace,
                name,
                fields,
            } => {
                for value in values.iter().flatten() {
                    match value {
                        CqlValue::UserDefinedType {
                            keyspace: actual_keyspace,
                            name: actual_name,
                            fields: actual_fields,
                        } if actual_keyspace == keyspace
                            && actual_name == name
                            && actual_fields.len() == fields.len()
                            && actual_fields
                                .iter()
                                .zip(fields)
                                .all(|((a, _), (b, _))| a == b) => {}
                        _ => {
                            return Err(invalid(
                                "UDT identity or fields do not match the declared type",
                            ));
                        }
                    }
                }
                let arrays = fields
                    .iter()
                    .enumerate()
                    .map(|(index, (_, kind))| {
                        let children = values
                            .iter()
                            .map(|value| match value {
                                Some(CqlValue::UserDefinedType { fields, .. }) => {
                                    fields[index].1.as_ref()
                                }
                                _ => None,
                            })
                            .collect::<Vec<_>>();
                        kind.values_array(Values::Nested(&children), column)
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(Arc::new(StructArray::try_new_with_length(
                    self.struct_fields(),
                    arrays,
                    Some(validity),
                    values.len(),
                )?))
            }
            _ => unreachable!("nested conversion requested for scalar"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum ConversionError {
    #[error(transparent)]
    Arrow(#[from] arrow::error::ArrowError),
    #[error("nested Scylla result in column {column}: {reason}")]
    Nested { column: usize, reason: &'static str },
    #[error(transparent)]
    Child(#[from] Error),
    #[error("unsupported Scylla result type: {0}")]
    Unsupported(String),
    #[error("Scylla result column {column} does not match {expected}")]
    Mismatch { column: usize, expected: String },
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod conversion_tests {
    use super::*;

    #[test]
    fn native_scalars_preserve_values_and_nulls() -> Result<()> {
        let rows = vec![
            Row {
                columns: vec![
                    Some(CqlValue::Date(scylla::value::CqlDate(0))),
                    Some(CqlValue::Counter(scylla::value::Counter(i64::MAX))),
                ],
            },
            Row {
                columns: vec![None, None],
            },
        ];
        let dates = Kind::Native(NativeType::Date).array(&rows, 0)?;
        assert_eq!(
            dates
                .as_any()
                .downcast_ref::<Date32Array>()
                .unwrap()
                .value(0),
            i32::MIN
        );
        assert!(dates.is_null(1));
        let counters = Kind::Native(NativeType::Counter).array(&rows, 1)?;
        assert_eq!(
            counters
                .as_any()
                .downcast_ref::<Int64Array>()
                .unwrap()
                .value(0),
            i64::MAX
        );
        assert!(counters.is_null(1));
        Ok(())
    }

    #[test]
    fn decimals_are_exact_annotated_text() -> Result<()> {
        let kind = Kind::Native(NativeType::Decimal);
        let rows = vec![Row {
            columns: vec![Some(CqlValue::Decimal(
                scylla::value::CqlDecimal::from_signed_be_bytes_and_exponent(vec![0x7b], 2),
            ))],
        }];
        let array = kind.array(&rows, 0)?;
        assert_eq!(
            array
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .value(0),
            "1.23"
        );
        assert_eq!(
            kind.field("amount").metadata()["quill.logical_type"],
            "decimal"
        );
        assert_eq!(kind.field("amount").metadata()["quill.encoding"], "text");
        Ok(())
    }

    #[test]
    fn mismatches_and_empty_values_are_errors() {
        for value in [CqlValue::Text("secret".into()), CqlValue::Empty] {
            let rows = vec![Row {
                columns: vec![Some(value)],
            }];
            assert!(Kind::Int.array(&rows, 0).is_err());
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod nested_tests {
    use super::*;
    use scylla::frame::response::result::UserDefinedType;

    fn collection(typ: CollectionType<'static>) -> ColumnType<'static> {
        ColumnType::Collection { frozen: false, typ }
    }
    fn native(typ: NativeType) -> Box<ColumnType<'static>> {
        Box::new(ColumnType::Native(typ))
    }
    fn rows(values: Vec<Option<CqlValue>>) -> Vec<Row> {
        values
            .into_iter()
            .map(|value| Row {
                columns: vec![value],
            })
            .collect()
    }

    #[test]
    fn nested_lists_maps_and_sets_preserve_null_empty_and_values() -> Result<()> {
        let kind = Kind::parse(&collection(CollectionType::List(Box::new(collection(
            CollectionType::Map(native(NativeType::Text), native(NativeType::Int)),
        )))))?;
        let rows = rows(vec![
            Some(CqlValue::List(vec![
                CqlValue::Map(vec![(CqlValue::Text("a".into()), CqlValue::Int(42))]),
                CqlValue::Map(vec![]),
            ])),
            Some(CqlValue::List(vec![])),
            None,
        ]);
        let array = kind.array(&rows, 0)?;
        assert_eq!(array.data_type(), kind.field("nested").data_type());
        let lists = array.as_any().downcast_ref::<ListArray>().unwrap();
        assert_eq!(lists.value_offsets(), &[0, 2, 2, 2]);
        assert!(!lists.is_null(1));
        assert!(lists.is_null(2));
        let child = lists.value(0);
        let maps = child.as_any().downcast_ref::<MapArray>().unwrap();
        assert_eq!(maps.value_offsets(), &[0, 1, 1]);
        assert_eq!(
            maps.keys()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .value(0),
            "a"
        );
        assert_eq!(
            maps.values()
                .as_any()
                .downcast_ref::<Int32Array>()
                .unwrap()
                .value(0),
            42
        );

        let set = Kind::parse(&collection(CollectionType::Set(native(NativeType::Text))))?;
        assert_eq!(set.field("tags").metadata()["quill.logical_type"], "set");
        let set_rows = self::rows(vec![
            Some(CqlValue::Set(vec![CqlValue::Text("x".into())])),
            None,
        ]);
        let set_array = set.array(&set_rows, 0)?;
        assert_eq!(set_array.data_type(), set.field("tags").data_type());
        assert!(set_array.is_null(1));
        Ok(())
    }

    #[test]
    fn tuples_udts_and_vectors_preserve_field_nulls_and_dimensions() -> Result<()> {
        let tuple = Kind::parse(&ColumnType::Tuple(vec![
            ColumnType::Native(NativeType::Int),
            ColumnType::Native(NativeType::Text),
        ]))?;
        let tuple_rows = rows(vec![
            Some(CqlValue::Tuple(vec![Some(CqlValue::Int(7)), None])),
            None,
        ]);
        let tuple_array = tuple.array(&tuple_rows, 0)?;
        let tuples = tuple_array.as_any().downcast_ref::<StructArray>().unwrap();
        assert!(!tuples.is_null(0));
        assert!(tuples.is_null(1));
        assert!(tuples.column(1).is_null(0));
        assert_eq!(
            tuples
                .column(0)
                .as_any()
                .downcast_ref::<Int32Array>()
                .unwrap()
                .value(0),
            7
        );
        assert_eq!(tuples.fields()[0].name(), "item_0");

        let udt = Kind::parse(&ColumnType::UserDefinedType {
            frozen: false,
            definition: Arc::new(UserDefinedType {
                name: "profile".into(),
                keyspace: "ks".into(),
                field_types: vec![("age".into(), ColumnType::Native(NativeType::Int))],
            }),
        })?;
        let udt_rows = rows(vec![
            Some(CqlValue::UserDefinedType {
                keyspace: "ks".into(),
                name: "profile".into(),
                fields: vec![("age".into(), Some(CqlValue::Int(21)))],
            }),
            None,
        ]);
        let udt_array = udt.array(&udt_rows, 0)?;
        assert_eq!(udt_array.data_type(), udt.field("person").data_type());
        assert_eq!(
            udt.field("person").metadata()["quill.native_type"],
            "ks.profile"
        );
        assert!(udt_array.is_null(1));

        let vector = Kind::parse(&ColumnType::Vector {
            typ: native(NativeType::Float),
            dimensions: 2,
        })?;
        let vector_rows = rows(vec![
            Some(CqlValue::Vector(vec![
                CqlValue::Float(1.5),
                CqlValue::Float(-2.5),
            ])),
            None,
        ]);
        let vector_array = vector.array(&vector_rows, 0)?;
        let vectors = vector_array
            .as_any()
            .downcast_ref::<FixedSizeListArray>()
            .unwrap();
        assert_eq!(vectors.value_length(), 2);
        assert!(vectors.is_null(1));
        assert_eq!(
            vectors
                .values()
                .as_any()
                .downcast_ref::<Float32Array>()
                .unwrap()
                .value(1),
            -2.5
        );
        Ok(())
    }

    #[test]
    fn invalid_nested_shapes_and_elements_return_errors() -> Result<()> {
        let list = Kind::parse(&collection(CollectionType::List(native(NativeType::Int))))?;
        for value in [
            CqlValue::Set(vec![]),
            CqlValue::List(vec![CqlValue::Text("private".into())]),
            CqlValue::List(vec![CqlValue::Empty]),
        ] {
            let error = list.array(&rows(vec![Some(value)]), 0).unwrap_err();
            assert!(!format!("{error:?}").contains("private"));
        }
        let vector = Kind::parse(&ColumnType::Vector {
            typ: native(NativeType::Int),
            dimensions: 2,
        })?;
        assert!(
            vector
                .array(
                    &rows(vec![Some(CqlValue::Vector(vec![CqlValue::Int(1)]))]),
                    0
                )
                .is_err()
        );
        let tuple = Kind::parse(&ColumnType::Tuple(vec![ColumnType::Native(
            NativeType::Int,
        )]))?;
        assert!(
            tuple
                .array(&rows(vec![Some(CqlValue::Tuple(vec![]))]), 0)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn nested_decimal_metadata_is_stable_across_batches_and_ipc()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use arrow::{
            datatypes::Schema,
            ipc::{reader::FileReader, writer::FileWriter},
            record_batch::RecordBatch,
        };
        let kind = Kind::parse(&collection(CollectionType::Map(
            native(NativeType::Text),
            Box::new(collection(CollectionType::List(native(
                NativeType::Decimal,
            )))),
        )))?;
        let schema = Arc::new(Schema::new(vec![kind.field("amounts")]));
        let fixtures = [
            rows(vec![]),
            rows(vec![None]),
            rows(vec![Some(CqlValue::Map(vec![(
                CqlValue::Text("exact".into()),
                CqlValue::List(vec![CqlValue::Decimal(
                    scylla::value::CqlDecimal::from_signed_be_bytes_and_exponent(vec![0x7b], 2),
                )]),
            )]))]),
        ];
        let mut bytes = Vec::new();
        {
            let mut writer = FileWriter::try_new(&mut bytes, &schema)?;
            for fixture in &fixtures {
                let batch = RecordBatch::try_new(schema.clone(), vec![kind.array(fixture, 0)?])?;
                writer.write(&batch)?;
            }
            writer.finish()?;
        }
        let reader = FileReader::try_new(std::io::Cursor::new(bytes), None)?;
        assert_eq!(reader.schema(), schema);
        for (batch, fixture) in reader.zip(&fixtures) {
            let batch = batch?;
            assert_eq!(batch.schema(), schema);
            assert_eq!(batch.column(0).to_data(), kind.array(fixture, 0)?.to_data());
        }
        let DataType::Map(entries, _) = schema.field(0).data_type() else {
            panic!("map")
        };
        let DataType::Struct(fields) = entries.data_type() else {
            panic!("entries")
        };
        let DataType::List(item) = fields[1].data_type() else {
            panic!("list")
        };
        assert_eq!(item.metadata()["quill.logical_type"], "decimal");
        assert_eq!(item.metadata()["quill.encoding"], "text");
        Ok(())
    }
}
