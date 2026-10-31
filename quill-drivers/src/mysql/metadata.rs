use quill_core::{
    error::{Error, Result},
    metadata::*,
};

#[derive(sqlx::FromRow)]
pub(super) struct ColumnRow {
    pub(super) name: String,
    pub(super) native_type: String,
    pub(super) nullable: String,
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
            nullable: Some(nullable == "YES"),
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
    pub(super) non_unique: i64,
    pub(super) method: String,
    pub(super) column_name: Option<String>,
    pub(super) expression: Option<String>,
    pub(super) direction: Option<String>,
    pub(super) prefix: Option<i64>,
}
impl From<&IndexRow> for IndexMetadata {
    fn from(row: &IndexRow) -> Self {
        Self {
            name: row.name.clone(),
            access_method: row.method.clone(),
            unique: row.non_unique == 0,
            primary: row.name == "PRIMARY",
            valid: true,
            keys: Vec::new(),
            included_columns: Vec::new(),
            predicate: None,
        }
    }
}
impl TryFrom<IndexRow> for IndexKey {
    type Error = Error;
    fn try_from(row: IndexRow) -> Result<Self> {
        let IndexRow {
            column_name: column,
            expression,
            direction,
            prefix,
            ..
        } = row;
        let source = match (column, expression) {
            (Some(column), _) => IndexKeySource::Column(column),
            (_, Some(expression)) => IndexKeySource::Expression(expression),
            _ => {
                return Err(Error::DriverExecute(anyhow::anyhow!(
                    "index key has no column or expression"
                )));
            }
        };
        let mut definition = match &source {
            IndexKeySource::Column(c) => format!("`{}`", c.replace('`', "``")),
            IndexKeySource::Expression(e) => format!("({e})"),
        };
        if let Some(prefix) = prefix {
            definition.push_str(&format!("({prefix})"));
        }
        let ordering = direction.map(|d| {
            let descending = d == "D";
            definition.push_str(if descending { " DESC" } else { " ASC" });
            IndexOrdering {
                descending,
                nulls_first: !descending,
            }
        });
        Ok(Self {
            source,
            definition,
            ordering,
        })
    }
}
