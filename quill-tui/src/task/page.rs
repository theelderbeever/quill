//! Format one page of a result for display.
use std::{ops::Range, sync::Arc};

use quill_core::value::Value;
use tokio::sync::mpsc::UnboundedSender;

use super::{RunId, RunMsg};
use crate::{event::Msg, result::ResultTable};

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

impl Cell {
    /// Bytes that are UTF-8 show as text; others as hex.
    fn format(value: &Value) -> Self {
        match value {
            Value::Bytes(bytes) => match std::str::from_utf8(bytes) {
                Ok(text) => Self::Text(text.to_owned()),
                Err(_) => Self::Binary(bytes.iter().map(|b| format!("{b:02x}")).collect()),
            },
            other => Self::Text(other.to_string()),
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

pub fn spawn(
    table: Arc<ResultTable>,
    run: RunId,
    index: u64,
    rows: Range<u64>,
    messages: UnboundedSender<Msg>,
) {
    tokio::spawn(async move {
        let result = Ok(fetch(&table, rows));
        let _ = messages.send(Msg::Run(RunMsg::Page { run, index, result }));
    });
}

fn fetch(table: &ResultTable, rows: Range<u64>) -> Page {
    Page {
        rows: rows
            .filter_map(|row| {
                let cells = table.row(row)?.iter().map(Cell::format).collect();
                Some(PageRow { row, cells })
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quill_core::value::Column;

    #[test]
    fn pages_follow_the_shown_order_and_keep_binary_hex() {
        let table = ResultTable::new(
            Arc::from([Column::new("v", "bytea")]),
            vec![
                Box::from([Value::Bytes("hello ☃".as_bytes().to_vec())]),
                Box::from([Value::Bytes(vec![0xff, 0x00, 0x80])]),
                Box::from([Value::Null]),
            ],
        );
        let page = fetch(&table, 0..3);
        assert_eq!(page.rows[0].cells[0], Cell::Text("hello ☃".into()));
        assert_eq!(page.rows[1].cells[0], Cell::Binary("ff0080".into()));
        assert_eq!(page.rows[1].cells[0].display(), "BINARY");
        assert_eq!(page.rows[2].cells[0], Cell::Text("NULL".into()));
        // Descending puts nulls first, then the larger bytes.
        let sorted = fetch(&table.sorted(0, true), 0..3);
        assert_eq!(sorted.rows[0].cells[0], Cell::Text("NULL".into()));
        assert_eq!(sorted.rows[1].cells[0], Cell::Binary("ff0080".into()));
    }
}
