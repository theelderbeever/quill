//! Streaming `TabSeparatedWithNamesAndTypes` decoding. The server formats
//! every value as text, so every type decodes, at worst as [`Value::Other`].
use std::sync::Arc;

use quill_core::value::{Column, Row, Value};

/// One unescaped field; `None` is `\N`.
pub(super) type Field = Option<Vec<u8>>;

/// Split complete lines out of a byte stream.
#[derive(Default)]
pub(super) struct Lines {
    buffer: Vec<u8>,
}

impl Lines {
    pub(super) fn push(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
    }

    /// The next complete line, without its newline.
    pub(super) fn next(&mut self) -> Option<Vec<u8>> {
        let end = self.buffer.iter().position(|b| *b == b'\n')?;
        let mut line: Vec<u8> = self.buffer.drain(..=end).collect();
        line.pop();
        Some(line)
    }

    /// Whether unterminated bytes remain.
    pub(super) fn has_remainder(&self) -> bool {
        !self.buffer.is_empty()
    }
}

/// Fields of one line, with TSV escapes resolved.
///
/// ```ignore
/// assert_eq!(fields(b"a\\tb\t\\N"), vec![Some(b"a\tb".to_vec()), None]);
/// ```
pub(super) fn fields(line: &[u8]) -> Vec<Field> {
    line.split(|b| *b == b'\t')
        .map(|raw| {
            if raw == b"\\N" {
                return None;
            }
            let mut out = Vec::with_capacity(raw.len());
            let mut bytes = raw.iter();
            while let Some(&b) = bytes.next() {
                if b != b'\\' {
                    out.push(b);
                    continue;
                }
                match bytes.next() {
                    Some(b'n') => out.push(b'\n'),
                    Some(b't') => out.push(b'\t'),
                    Some(b'r') => out.push(b'\r'),
                    Some(b'0') => out.push(0),
                    Some(b'b') => out.push(0x08),
                    Some(b'f') => out.push(0x0c),
                    Some(&other) => out.push(other),
                    None => out.push(b'\\'),
                }
            }
            Some(out)
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Int,
    UInt,
    BigInt,
    Float,
    Decimal,
    Bool,
    Text,
    Date,
    DateTime,
    Uuid,
    Json,
    Other,
}

impl Kind {
    fn of(native: &str) -> Self {
        let mut ty = native.trim();
        loop {
            let inner = ["Nullable(", "LowCardinality("]
                .iter()
                .find_map(|w| ty.strip_prefix(w).and_then(|t| t.strip_suffix(')')));
            match inner {
                Some(inner) => ty = inner,
                None => break,
            }
        }
        let base = ty.split('(').next().unwrap_or(ty);
        match base {
            "Int8" | "Int16" | "Int32" | "Int64" => Self::Int,
            "UInt8" | "UInt16" | "UInt32" | "UInt64" => Self::UInt,
            "Int128" | "Int256" | "UInt128" | "UInt256" => Self::BigInt,
            "Float32" | "Float64" | "BFloat16" => Self::Float,
            "Decimal" | "Decimal32" | "Decimal64" | "Decimal128" | "Decimal256" => Self::Decimal,
            "Bool" => Self::Bool,
            "String" | "FixedString" | "Enum8" | "Enum16" => Self::Text,
            "Date" | "Date32" => Self::Date,
            "DateTime" | "DateTime64" => Self::DateTime,
            "UUID" => Self::Uuid,
            "JSON" | "Object" => Self::Json,
            _ => Self::Other,
        }
    }

    fn decode(self, bytes: Vec<u8>) -> Value {
        if self == Self::Text {
            return Value::text_or_bytes(bytes);
        }
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(e) => return Value::Bytes(e.into_bytes()),
        };
        let parsed = match self {
            Self::Int => text.parse().ok().map(Value::Int),
            Self::UInt => text.parse().ok().map(Value::UInt),
            Self::BigInt | Self::Decimal => Some(Value::decimal_or_other(&text)),
            Self::Float => text.parse().ok().map(Value::Float),
            Self::Bool => match text.as_str() {
                "true" => Some(Value::Bool(true)),
                "false" => Some(Value::Bool(false)),
                _ => None,
            },
            Self::Date => chrono::NaiveDate::parse_from_str(&text, "%Y-%m-%d")
                .ok()
                .map(Value::Date),
            Self::DateTime => chrono::NaiveDateTime::parse_from_str(&text, "%Y-%m-%d %H:%M:%S%.f")
                .ok()
                .map(Value::Timestamp),
            Self::Uuid => uuid::Uuid::parse_str(&text).ok().map(Value::Uuid),
            Self::Json => return Value::Json(text),
            Self::Text | Self::Other => None,
        };
        parsed.unwrap_or(Value::Other(text))
    }
}

/// Decodes rows once both header lines have been read.
pub(super) struct Decoder {
    columns: Arc<[Column]>,
    kinds: Vec<Kind>,
}

impl Decoder {
    pub(super) fn new(names: Vec<Field>, types: Vec<Field>) -> Self {
        let text = |f: Field| String::from_utf8_lossy(&f.unwrap_or_default()).into_owned();
        let columns: Arc<[Column]> = names
            .into_iter()
            .zip(types)
            .map(|(name, ty)| Column::new(text(name), text(ty)))
            .collect();
        let kinds = columns.iter().map(|c| Kind::of(&c.native_type)).collect();
        Self { columns, kinds }
    }

    pub(super) fn columns(&self) -> Arc<[Column]> {
        self.columns.clone()
    }

    pub(super) fn row(&self, line: &[u8]) -> Row {
        let mut fields = fields(line).into_iter();
        self.kinds
            .iter()
            .map(|kind| match fields.next().flatten() {
                Some(bytes) => kind.decode(bytes),
                None => Value::Null,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescapes_fields_and_nulls() {
        assert_eq!(
            fields(b"a\\tb\t\\N\t\\\\"),
            vec![Some(b"a\tb".to_vec()), None, Some(b"\\".to_vec())]
        );
    }

    #[test]
    fn decodes_by_native_type() {
        let header = |s: &str| fields(s.as_bytes());
        let decoder = Decoder::new(
            header("a\tb\tc\td\te"),
            header("Nullable(Int32)\tLowCardinality(String)\tDecimal(76, 2)\tArray(UInt8)\tString"),
        );
        let row = decoder.row(b"\\N\tx\t123456789012345678901234567890.25\t[1,2]\t\xff");
        assert_eq!(row[0], Value::Null);
        assert_eq!(row[1], Value::Text("x".into()));
        assert_eq!(row[2].to_string(), "123456789012345678901234567890.25");
        assert_eq!(row[3], Value::Other("[1,2]".into()));
        assert_eq!(row[4], Value::Bytes(vec![0xff]));
    }

    #[test]
    fn splits_lines_across_chunks() {
        let mut lines = Lines::default();
        lines.push(b"a\tb\nc");
        assert_eq!(lines.next(), Some(b"a\tb".to_vec()));
        assert_eq!(lines.next(), None);
        lines.push(b"d\n");
        assert_eq!(lines.next(), Some(b"cd".to_vec()));
        assert!(!lines.has_remainder());
    }
}
