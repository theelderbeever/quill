//! Fetch and format one page of a result through DataFusion.
use std::ops::Range;

use arrow::{
    array::{Array, BinaryArray, BinaryViewArray, FixedSizeBinaryArray, LargeBinaryArray},
    datatypes::{DataType, Field},
    util::display::{ArrayFormatter, FormatOptions},
};
use datafusion::prelude::SessionContext;
use tokio::sync::mpsc::UnboundedSender;

use super::{RunId, RunMsg};
use crate::event::Msg;

/// Borrow the bytes of a non-null binary cell.
fn binary_bytes(column: &dyn Array, row: usize) -> Option<&[u8]> {
    if column.is_null(row) {
        return None;
    }
    let values = column.as_any();
    let bytes = if let Some(array) = values.downcast_ref::<BinaryArray>() {
        array.value(row)
    } else if let Some(array) = values.downcast_ref::<LargeBinaryArray>() {
        array.value(row)
    } else if let Some(array) = values.downcast_ref::<BinaryViewArray>() {
        array.value(row)
    } else {
        let array = values.downcast_ref::<FixedSizeBinaryArray>()?;
        array.value(row)
    };
    Some(bytes)
}

/// Formatted cells for a run of consecutive rows.
pub struct Page {
    pub rows: Vec<PageRow>,
}

pub struct PageRow {
    pub row: u64,
    pub cells: Vec<Cell>,
}

/// Display text, or a binary value retained as hex for inspection and copying.
#[derive(Debug, PartialEq)]
pub enum Cell {
    Text(String),
    Binary(String),
}

#[derive(Clone, Copy)]
enum DisplayType {
    Value,
    Uuid,
}

impl DisplayType {
    fn for_field(field: &Field) -> Self {
        if field.data_type() == &DataType::FixedSizeBinary(16)
            && field
                .metadata()
                .get("ARROW:extension:name")
                .is_some_and(|name| name == "arrow.uuid")
        {
            Self::Uuid
        } else {
            Self::Value
        }
    }
}

impl Cell {
    fn format(
        column: &dyn Array,
        row: usize,
        kind: DisplayType,
        fallback: impl FnOnce() -> String,
    ) -> Self {
        if let DisplayType::Uuid = kind
            && let Some(bytes) = binary_bytes(column, row)
            && let Ok(uuid) = uuid::Uuid::from_slice(bytes)
        {
            return Self::Text(uuid.to_string());
        }
        match binary_bytes(column, row).map(std::str::from_utf8) {
            Some(Ok(text)) => Self::Text(text.to_owned()),
            Some(Err(_)) => Self::Binary(fallback()),
            None => Self::Text(fallback()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Text(text) | Self::Binary(text) => text,
        }
    }

    pub fn display(&self) -> &str {
        match self {
            Self::Text(text) => text,
            Self::Binary(_) => "BINARY",
        }
    }
}

/// `ctx` must keep row order; see [`crate::result::ResultTable::ordered`].
pub fn spawn(
    ctx: SessionContext,
    table: String,
    run: RunId,
    index: u64,
    rows: Range<u64>,
    messages: UnboundedSender<Msg>,
) {
    tokio::spawn(async move {
        let result = fetch(&ctx, &table, rows).await;
        let _ = messages.send(Msg::Run(RunMsg::Page { run, index, result }));
    });
}

async fn fetch(ctx: &SessionContext, table: &str, rows: Range<u64>) -> anyhow::Result<Page> {
    let sql = format!(
        "SELECT * FROM {table} LIMIT {} OFFSET {}",
        rows.end - rows.start,
        rows.start
    );
    let batches = ctx.sql(&sql).await?.collect().await?;
    let options = FormatOptions::default().with_null("NULL");
    let mut page = Vec::new();
    let mut row = rows.start;
    for batch in &batches {
        let schema = batch.schema();
        let formatters = batch
            .columns()
            .iter()
            .enumerate()
            .map(|(index, column)| {
                ArrayFormatter::try_new(column.as_ref(), &options).map(|formatter| {
                    (
                        column.as_ref(),
                        DisplayType::for_field(schema.field(index)),
                        formatter,
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        for i in 0..batch.num_rows() {
            page.push(PageRow {
                row,
                cells: formatters
                    .iter()
                    .map(|(column, kind, formatter)| {
                        Cell::format(*column, i, *kind, || formatter.value(i).to_string())
                    })
                    .collect(),
            });
            row += 1;
        }
    }
    Ok(Page { rows: page })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn uuid_metadata_survives_paging_and_formats_only_tagged_values() {
        use arrow::{array::FixedSizeBinaryBuilder, datatypes::Schema, record_batch::RecordBatch};
        use std::sync::Arc;
        let uuid = uuid::Uuid::parse_str("00112233-4455-6677-8899-aabbccddeeff").unwrap();
        let mut builder = FixedSizeBinaryBuilder::new(16);
        builder.append_value(uuid.as_bytes()).unwrap();
        builder.append_null();
        builder.append_value([0; 16]).unwrap();
        let array = Arc::new(builder.finish());
        let field = Field::new("id", DataType::FixedSizeBinary(16), true).with_metadata(
            [
                ("ARROW:extension:name".into(), "arrow.uuid".into()),
                ("ARROW:extension:metadata".into(), "".into()),
            ]
            .into(),
        );
        let schema = Arc::new(Schema::new(vec![
            field,
            Field::new("bytes", DataType::FixedSizeBinary(16), true),
        ]));
        let batch = RecordBatch::try_new(schema, vec![array.clone(), array]).unwrap();
        let ctx = SessionContext::new();
        ctx.register_batch("r", batch).unwrap();
        let page = fetch(&ctx, "r", 0..3).await.unwrap();
        assert_eq!(page.rows[0].cells[0], Cell::Text(uuid.to_string()));
        assert!(matches!(page.rows[0].cells[1], Cell::Binary(_)));
        assert_eq!(page.rows[1].cells[0], Cell::Text("NULL".into()));
        assert_eq!(
            page.rows[2].cells[0],
            Cell::Text(uuid::Uuid::nil().to_string())
        );
    }

    #[test]
    fn binary_cells_display_utf8_without_losing_invalid_bytes() {
        let array = BinaryArray::from(vec![
            Some("hello ☃".as_bytes()),
            Some(&[0xff, 0x00, 0x80][..]),
            Some(&b""[..]),
            None,
        ]);
        let options = FormatOptions::default().with_null("NULL");
        let formatter = ArrayFormatter::try_new(&array, &options).unwrap();
        let cells: Vec<_> = (0..array.len())
            .map(|i| {
                Cell::format(&array, i, DisplayType::Value, || {
                    formatter.value(i).to_string()
                })
            })
            .collect();
        assert_eq!(cells[0], Cell::Text("hello ☃".into()));
        assert_eq!(cells[1], Cell::Binary("ff0080".into()));
        assert_eq!(cells[1].display(), "BINARY");
        assert_eq!(cells[1].as_str(), "ff0080");
        assert_eq!(cells[2], Cell::Text("".into()));
        assert_eq!(cells[3], Cell::Text("NULL".into()));
        assert_eq!(Cell::Text("BINARY".into()).display(), "BINARY");
        assert_eq!(array.value(1), &[0xff, 0x00, 0x80]);
    }
}
