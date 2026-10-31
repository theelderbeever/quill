//! Database object references preserve native identifier components.

/// A catalog name as returned by a driver.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CatalogRef {
    pub name: String,
}

/// A schema within a catalog.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SchemaRef {
    pub catalog: CatalogRef,
    pub name: String,
}

/// A table within a schema. Components are never SQL-quoted or concatenated.
///
/// ```
/// use quill_core::metadata::{CatalogRef, SchemaRef, TableRef};
/// let table = TableRef {
///     schema: SchemaRef {
///         catalog: CatalogRef { name: "warehouse".into() },
///         name: "public".into(),
///     },
///     name: "order.items".into(),
/// };
/// assert_eq!(table.name, "order.items");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TableRef {
    pub schema: SchemaRef,
    pub name: String,
}

/// Column metadata in the database's declared column order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnMetadata {
    pub name: String,
    /// Native database type spelling, including driver-specific details.
    pub native_type: String,
    /// None when the database cannot report nullability.
    pub nullable: Option<bool>,
}

/// Description of a table, including columns in declaration order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableMetadata {
    pub table: TableRef,
    pub columns: Vec<ColumnMetadata>,
}

/// An index key's source; expressions preserve native database syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexKeySource {
    Column(String),
    Expression(String),
}

/// Ordering reported by an ordered index access method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexOrdering {
    pub descending: bool,
    pub nulls_first: bool,
}

/// One index key in declaration order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexKey {
    pub source: IndexKeySource,
    /// Native key definition, including operator classes and collation.
    pub definition: String,
    /// Absent for access methods without ordering.
    pub ordering: Option<IndexOrdering>,
}

/// An index on a table, with keys and included columns in declaration order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexMetadata {
    pub name: String,
    pub access_method: String,
    pub unique: bool,
    pub primary: bool,
    pub valid: bool,
    pub keys: Vec<IndexKey>,
    pub included_columns: Vec<String>,
    /// Native SQL expression for a partial index.
    pub predicate: Option<String>,
}
