//! Result values as each database's client decodes them.
//!
//! Conversion into [`Value`] is total: a cell Quill cannot decode is kept as
//! the server's text in [`Value::Other`], never reported as an error.
use std::{cmp::Ordering, fmt, sync::Arc};

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime};
use uuid::Uuid;

/// One result column as the server describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    /// The server's spelling of the type, e.g. `numeric(60,8)` or `LowCardinality(String)`.
    pub native_type: String,
}

impl Column {
    pub fn new(name: impl Into<String>, native_type: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            native_type: native_type.into(),
        }
    }
}

/// An exact decimal of any precision, kept as its canonical text.
///
/// ```
/// use quill_core::value::Decimal;
/// let d = Decimal::parse("-0012.500").unwrap();
/// assert_eq!(d.as_str(), "-12.500");
/// assert!(Decimal::parse("1e5").is_none());
/// assert!(Decimal::parse("NaN").is_none());
/// assert!(Decimal::parse("10").unwrap() > Decimal::parse("9.99").unwrap());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decimal(String);

impl Decimal {
    /// Accepts `[+-]digits[.digits]`; leading integer zeros are dropped.
    pub fn parse(text: &str) -> Option<Self> {
        let (negative, unsigned) = match text.as_bytes().first()? {
            b'-' => (true, &text[1..]),
            b'+' => (false, &text[1..]),
            _ => (false, text),
        };
        let (integer, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
        let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
        if integer.is_empty() && fraction.is_empty()
            || !digits(integer)
            || !digits(fraction)
            || unsigned.ends_with('.')
        {
            return None;
        }
        let integer = integer.trim_start_matches('0');
        let integer = if integer.is_empty() { "0" } else { integer };
        let zero = integer == "0" && fraction.bytes().all(|b| b == b'0');
        let mut canonical = String::with_capacity(text.len() + 1);
        if negative && !zero {
            canonical.push('-');
        }
        canonical.push_str(integer);
        if !fraction.is_empty() {
            canonical.push('.');
            canonical.push_str(fraction);
        }
        Some(Self(canonical))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn parts(&self) -> (bool, &str, &str) {
        let (negative, unsigned) = match self.0.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, self.0.as_str()),
        };
        let (integer, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
        (negative, integer, fraction.trim_end_matches('0'))
    }
}

impl Ord for Decimal {
    fn cmp(&self, other: &Self) -> Ordering {
        let (a_neg, a_int, a_frac) = self.parts();
        let (b_neg, b_int, b_frac) = other.parts();
        let magnitude = a_int
            .len()
            .cmp(&b_int.len())
            .then_with(|| a_int.cmp(b_int))
            .then_with(|| a_frac.cmp(b_frac));
        match (a_neg, b_neg) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => magnitude,
            (true, true) => magnitude.reverse(),
        }
    }
}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One decoded cell.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f64),
    Decimal(Decimal),
    Text(String),
    Bytes(Vec<u8>),
    Date(NaiveDate),
    Time(NaiveTime),
    Timestamp(NaiveDateTime),
    TimestampTz(DateTime<FixedOffset>),
    Uuid(Uuid),
    Json(String),
    List(Vec<Value>),
    /// The server's text for a value Quill does not decode.
    Other(String),
}

impl Value {
    /// An exact decimal from text, else the text itself as [`Value::Other`].
    ///
    /// ```
    /// use quill_core::value::Value;
    /// assert!(matches!(Value::decimal_or_other("1.50"), Value::Decimal(_)));
    /// assert_eq!(Value::decimal_or_other("NaN"), Value::Other("NaN".into()));
    /// ```
    pub fn decimal_or_other(text: &str) -> Self {
        Decimal::parse(text).map_or_else(|| Self::Other(text.to_owned()), Self::Decimal)
    }

    /// Text when the bytes are UTF-8, else the bytes.
    pub fn text_or_bytes(bytes: Vec<u8>) -> Self {
        String::from_utf8(bytes).map_or_else(|e| Self::Bytes(e.into_bytes()), Self::Text)
    }

    pub const fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// A total order for sorting results: nulls last, numbers by value,
    /// otherwise by variant then value.
    ///
    /// ```
    /// use std::cmp::Ordering;
    /// use quill_core::value::Value;
    /// assert_eq!(Value::Int(2).sort_cmp(&Value::Float(1.5)), Ordering::Greater);
    /// assert_eq!(Value::Null.sort_cmp(&Value::Int(0)), Ordering::Greater);
    /// ```
    pub fn sort_cmp(&self, other: &Self) -> Ordering {
        use Value::*;
        match (self, other) {
            (Null, Null) => Ordering::Equal,
            (Null, _) => Ordering::Greater,
            (_, Null) => Ordering::Less,
            (Bool(a), Bool(b)) => a.cmp(b),
            (Int(a), Int(b)) => a.cmp(b),
            (UInt(a), UInt(b)) => a.cmp(b),
            (Int(a), UInt(b)) => i128::from(*a).cmp(&i128::from(*b)),
            (UInt(a), Int(b)) => i128::from(*a).cmp(&i128::from(*b)),
            (Decimal(a), Decimal(b)) => a.cmp(b),
            (a, b) if a.as_f64().is_some() && b.as_f64().is_some() => a
                .as_f64()
                .unwrap_or_default()
                .total_cmp(&b.as_f64().unwrap_or_default()),
            (Text(a), Text(b)) | (Json(a), Json(b)) | (Other(a), Other(b)) => a.cmp(b),
            (Bytes(a), Bytes(b)) => a.cmp(b),
            (Date(a), Date(b)) => a.cmp(b),
            (Time(a), Time(b)) => a.cmp(b),
            (Timestamp(a), Timestamp(b)) => a.cmp(b),
            (TimestampTz(a), TimestampTz(b)) => a.cmp(b),
            (Uuid(a), Uuid(b)) => a.cmp(b),
            (List(a), List(b)) => a
                .iter()
                .zip(b)
                .map(|(x, y)| x.sort_cmp(y))
                .find(|o| o.is_ne())
                .unwrap_or_else(|| a.len().cmp(&b.len())),
            (a, b) => a.rank().cmp(&b.rank()),
        }
    }

    fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Int(v) => Some(*v as f64),
            Self::UInt(v) => Some(*v as f64),
            Self::Float(v) => Some(*v),
            Self::Decimal(d) => d.as_str().parse().ok(),
            _ => None,
        }
    }

    const fn rank(&self) -> u8 {
        match self {
            Self::Null => 0,
            Self::Bool(_) => 1,
            Self::Int(_) | Self::UInt(_) | Self::Float(_) | Self::Decimal(_) => 2,
            Self::Text(_) => 3,
            Self::Bytes(_) => 4,
            Self::Date(_) => 5,
            Self::Time(_) => 6,
            Self::Timestamp(_) => 7,
            Self::TimestampTz(_) => 8,
            Self::Uuid(_) => 9,
            Self::Json(_) => 10,
            Self::List(_) => 11,
            Self::Other(_) => 12,
        }
    }

    /// Approximate memory held, for progress reporting.
    pub fn approx_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + match self {
                Self::Text(s) | Self::Json(s) | Self::Other(s) => s.len(),
                Self::Decimal(d) => d.0.len(),
                Self::Bytes(b) => b.len(),
                Self::List(items) => items.iter().map(Self::approx_bytes).sum(),
                _ => 0,
            }
    }
}

/// `NULL` for nulls, lowercase hex for bytes, `[a, b]` for lists.
///
/// ```
/// use quill_core::value::Value;
/// assert_eq!(Value::Bytes(vec![0xff, 0x01]).to_string(), "\\xff01");
/// assert_eq!(Value::List(vec![Value::Int(1), Value::Null]).to_string(), "[1, NULL]");
/// ```
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => f.write_str("NULL"),
            Self::Bool(v) => write!(f, "{v}"),
            Self::Int(v) => write!(f, "{v}"),
            Self::UInt(v) => write!(f, "{v}"),
            Self::Float(v) => write!(f, "{v}"),
            Self::Decimal(v) => write!(f, "{v}"),
            Self::Text(v) | Self::Json(v) | Self::Other(v) => f.write_str(v),
            Self::Bytes(v) => {
                f.write_str("\\x")?;
                v.iter().try_for_each(|b| write!(f, "{b:02x}"))
            }
            Self::Date(v) => write!(f, "{v}"),
            Self::Time(v) => write!(f, "{v}"),
            Self::Timestamp(v) => write!(f, "{v}"),
            Self::TimestampTz(v) => write!(f, "{v}"),
            Self::Uuid(v) => write!(f, "{v}"),
            Self::List(items) => {
                f.write_str("[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{item}")?;
                }
                f.write_str("]")
            }
        }
    }
}

/// One result row, as wide as its batch's columns.
pub type Row = Box<[Value]>;

/// A row's width differs from the batch's column count.
#[derive(Debug, thiserror::Error)]
#[error("row has {actual} values for {expected} columns")]
pub struct RowWidthError {
    pub expected: usize,
    pub actual: usize,
}

/// Rows sharing one column list. Every row has exactly one value per column.
///
/// ```
/// use std::sync::Arc;
/// use quill_core::value::{Column, RowBatch, Value};
/// let columns: Arc<[Column]> = Arc::from([Column::new("n", "int8")]);
/// let batch = RowBatch::new(columns.clone(), vec![Box::from([Value::Int(1)])]).unwrap();
/// assert_eq!(batch.len(), 1);
/// assert!(RowBatch::new(columns, vec![Box::from([])]).is_err());
/// ```
#[derive(Debug, Clone)]
pub struct RowBatch {
    columns: Arc<[Column]>,
    rows: Vec<Row>,
}

impl RowBatch {
    pub fn new(columns: Arc<[Column]>, rows: Vec<Row>) -> Result<Self, RowWidthError> {
        let expected = columns.len();
        if let Some(row) = rows.iter().find(|row| row.len() != expected) {
            return Err(RowWidthError {
                expected,
                actual: row.len(),
            });
        }
        Ok(Self { columns, rows })
    }

    pub fn columns(&self) -> &Arc<[Column]> {
        &self.columns
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn into_rows(self) -> Vec<Row> {
        self.rows
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Approximate memory held, for progress reporting.
    pub fn approx_bytes(&self) -> usize {
        self.rows
            .iter()
            .flat_map(|row| row.iter())
            .map(Value::approx_bytes)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimals_compare_numerically() {
        let d = |s| Decimal::parse(s).unwrap();
        assert!(d("-1.5") < d("-1.25"));
        assert_eq!(d("0.10").cmp(&d("0.1")), Ordering::Equal);
        assert!(d("100") > d("99.999"));
        assert_eq!(d("-0.00").as_str(), "0.00");
        assert!(Decimal::parse(".5").is_some());
        assert!(Decimal::parse("5.").is_none());
        assert!(Decimal::parse("-").is_none());
    }

    #[test]
    fn huge_numerics_are_exact() {
        let text = "12345678901234567890123456789012345678901234567890.12345678";
        assert_eq!(Value::decimal_or_other(text).to_string(), text);
    }
}
