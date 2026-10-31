use quill_core::metadata::*;

#[derive(clickhouse::Row, serde::Deserialize)]
pub(super) struct ColumnRow {
    pub(super) name: String,
    pub(super) native_type: String,
}
impl From<ColumnRow> for ColumnMetadata {
    fn from(row: ColumnRow) -> Self {
        let nullable = Some(
            row.native_type.starts_with("Nullable(")
                || row.native_type.starts_with("LowCardinality(Nullable("),
        );
        Self {
            name: row.name,
            native_type: row.native_type,
            nullable,
        }
    }
}
#[derive(clickhouse::Row, serde::Deserialize)]
pub(super) struct IndexRow {
    pub(super) name: String,
    pub(super) access_method: String,
    pub(super) expression: String,
}
impl From<IndexRow> for IndexMetadata {
    fn from(row: IndexRow) -> Self {
        Self {
            name: row.name,
            access_method: row.access_method,
            unique: false,
            primary: false,
            valid: true,
            keys: vec![IndexKey {
                source: IndexKeySource::Expression(row.expression.clone()),
                definition: row.expression,
                ordering: None,
            }],
            included_columns: Vec::new(),
            predicate: None,
        }
    }
}

#[derive(clickhouse::Row, serde::Deserialize)]
pub(super) struct NameRow {
    pub(super) name: String,
}
pub(super) struct SchemaRow<'a> {
    pub(super) row: NameRow,
    pub(super) catalog: &'a CatalogRef,
}
impl From<SchemaRow<'_>> for SchemaRef {
    fn from(SchemaRow { row, catalog }: SchemaRow<'_>) -> Self {
        Self {
            catalog: catalog.clone(),
            name: row.name,
        }
    }
}
pub(super) struct TableRow<'a> {
    pub(super) row: NameRow,
    pub(super) schema: &'a SchemaRef,
}
impl From<TableRow<'_>> for TableRef {
    fn from(TableRow { row, schema }: TableRow<'_>) -> Self {
        Self {
            schema: schema.clone(),
            name: row.name,
        }
    }
}
