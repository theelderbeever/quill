use quill_core::metadata::*;

#[derive(sqlx::FromRow)]
pub(super) struct ColumnRow {
    pub(super) name: String,
    pub(super) native_type: String,
    pub(super) nullable: bool,
}
impl From<ColumnRow> for ColumnMetadata {
    fn from(row: ColumnRow) -> Self {
        let ColumnRow {
            name,
            native_type,
            nullable,
        } = row;
        Self {
            name,
            native_type,
            nullable: Some(nullable),
        }
    }
}

#[derive(sqlx::FromRow)]
pub(super) struct IndexRow {
    pub(super) name: String,
    pub(super) access_method: String,
    pub(super) indisunique: bool,
    pub(super) indisprimary: bool,
    pub(super) indisvalid: bool,
    pub(super) predicate: Option<String>,
    pub(super) is_key: bool,
    pub(super) column_name: Option<String>,
    pub(super) definition: String,
    pub(super) descending: Option<bool>,
    pub(super) nulls_first: Option<bool>,
}
impl From<&IndexRow> for IndexMetadata {
    fn from(row: &IndexRow) -> Self {
        Self {
            name: row.name.clone(),
            access_method: row.access_method.clone(),
            unique: row.indisunique,
            primary: row.indisprimary,
            valid: row.indisvalid,
            predicate: row.predicate.clone(),
            keys: Vec::new(),
            included_columns: Vec::new(),
        }
    }
}
impl From<IndexRow> for IndexKey {
    fn from(row: IndexRow) -> Self {
        Self {
            source: row
                .column_name
                .map(IndexKeySource::Column)
                .unwrap_or_else(|| IndexKeySource::Expression(row.definition.clone())),
            definition: row.definition,
            ordering: row
                .descending
                .zip(row.nulls_first)
                .map(|(descending, nulls_first)| IndexOrdering {
                    descending,
                    nulls_first,
                }),
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

impl From<NameRow> for CatalogRef {
    fn from(row: NameRow) -> Self {
        Self { name: row.name }
    }
}
