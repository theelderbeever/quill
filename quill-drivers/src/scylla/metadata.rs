use quill_core::metadata::*;

#[derive(scylla::DeserializeRow)]
pub(super) struct ColumnRow {
    pub(super) column_name: String,
    pub(super) native_type: String,
    pub(super) kind: String,
    pub(super) position: i32,
}
impl From<ColumnRow> for ColumnMetadata {
    fn from(row: ColumnRow) -> Self {
        let ColumnRow {
            column_name,
            native_type,
            kind,
            position: _,
        } = row;
        Self {
            name: column_name,
            native_type,
            nullable: Some(kind != "partition_key" && kind != "clustering"),
        }
    }
}

#[derive(scylla::DeserializeRow)]
pub(super) struct NameRow {
    pub(super) name: String,
}
#[derive(scylla::DeserializeRow)]
pub(super) struct IndexRow {
    pub(super) index_name: String,
    pub(super) kind: String,
    pub(super) options: std::collections::HashMap<String, String>,
}

impl TryFrom<IndexRow> for IndexMetadata {
    type Error = quill_core::error::Error;
    fn try_from(row: IndexRow) -> quill_core::error::Result<Self> {
        let IndexRow {
            index_name: name,
            kind,
            options,
        } = row;
        let target = options
            .get("target")
            .ok_or_else(|| quill_core::error::Error::MissingMetadata("index target"))?;
        Ok(IndexMetadata {
            name,
            access_method: options.get("class_name").cloned().unwrap_or(kind),
            unique: false,
            primary: false,
            valid: true,
            keys: vec![IndexKey {
                source: IndexKeySource::Expression(target.clone()),
                definition: target.clone(),
                ordering: None,
            }],
            included_columns: Vec::new(),
            predicate: None,
        })
    }
}

pub(super) struct SchemaRow<'a> {
    pub(super) row: NameRow,
    pub(super) catalog: &'a CatalogRef,
}
impl From<SchemaRow<'_>> for SchemaRef {
    fn from(row: SchemaRow<'_>) -> Self {
        Self {
            catalog: row.catalog.clone(),
            name: row.row.name,
        }
    }
}
pub(super) struct TableRow<'a> {
    pub(super) row: NameRow,
    pub(super) schema: &'a SchemaRef,
}
impl From<TableRow<'_>> for TableRef {
    fn from(row: TableRow<'_>) -> Self {
        Self {
            schema: row.schema.clone(),
            name: row.row.name,
        }
    }
}
