use quill_core::{
    error::{Error, Result},
    metadata::*,
};

#[derive(sqlx::FromRow)]
pub(super) struct ColumnRow {
    pub(super) name: String,
    pub(super) native_type: String,
    pub(super) not_null: bool,
    pub(super) pk: i64,
    pub(super) strict: bool,
}
impl From<ColumnRow> for ColumnMetadata {
    fn from(row: ColumnRow) -> Self {
        Self {
            name: row.name,
            native_type: row.native_type,
            nullable: if !row.not_null && row.pk > 0 && !row.strict {
                None
            } else {
                Some(!(row.not_null || row.pk > 0 && row.strict))
            },
        }
    }
}

#[derive(sqlx::FromRow)]
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

#[derive(sqlx::FromRow)]
pub(super) struct IndexRow {
    pub(super) name: String,
    pub(super) unique: bool,
    pub(super) origin: String,
    pub(super) partial: bool,
}
#[derive(sqlx::FromRow)]
pub(super) struct IndexKeyRow {
    pub(super) cid: i64,
    pub(super) column_name: Option<String>,
    pub(super) descending: bool,
    pub(super) collation: String,
}
impl TryFrom<IndexKeyRow> for IndexKey {
    type Error = Error;
    fn try_from(row: IndexKeyRow) -> Result<Self> {
        let IndexKeyRow {
            column_name,
            descending,
            collation,
            ..
        } = row;
        let column = column_name
            .ok_or_else(|| Error::DriverExecute(anyhow::anyhow!("index key has no column")))?;
        let definition = format!(
            "\"{}\" COLLATE \"{}\" {}",
            column.replace('"', "\"\""),
            collation.replace('"', "\"\""),
            if descending { "DESC" } else { "ASC" }
        );
        Ok(IndexKey {
            source: IndexKeySource::Column(column),
            definition,
            ordering: Some(IndexOrdering {
                descending,
                nulls_first: !descending,
            }),
        })
    }
}

impl From<IndexRow> for IndexMetadata {
    fn from(row: IndexRow) -> Self {
        Self {
            name: row.name,
            access_method: "btree".into(),
            unique: row.unique,
            primary: row.origin == "pk",
            valid: true,
            keys: Vec::new(),
            included_columns: Vec::new(),
            predicate: None,
        }
    }
}
