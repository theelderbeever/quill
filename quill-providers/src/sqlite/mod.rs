//! Native SQLite through sqlx.
mod settings;
pub use settings::SqliteSettings;

use crate::settings::SettingsError;
use quill_core::error::BoxError;
use std::{path::Path, str::FromStr, sync::Arc};

use quill_core::{
    error::{Error, Result},
    metadata::{
        CatalogRef, ColumnMetadata, IndexKey, IndexKeySource, IndexMetadata, IndexOrdering,
        SchemaRef, TableMetadata, TableRef,
    },
    query::{ParameterValue, QueryExecution, QueryId, QueryRequest},
    value::{Column, Row, Value},
};
use sqlx::{
    AssertSqlSafe, Column as _, ConnectOptions, Executor, Row as _, SqlSafeStr, Sqlite, SqlitePool,
    Statement as _, TypeInfo, ValueRef,
    query::Query,
    sqlite::{SqliteArguments, SqliteColumn, SqliteConnectOptions, SqlitePoolOptions, SqliteRow},
};

use crate::{
    Capabilities, ConnectionPool, PoolFuture,
    execution::{self, Emitter},
    sql::{self, RowDecoder, query_error},
};

/// SQLite has one connection-local catalog.
const CATALOG: &str = "sqlite";

/// SQLite's name for a private in-memory database.
const MEMORY: &str = ":memory:";

/// Native SQLite connection pool.
pub struct SqliteConnectionPool {
    inner: SqlitePool,
}

fn driver(error: sqlx::Error) -> Error {
    Error::DriverExecute(error.into())
}

/// SQL string literal for `text`.
fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// SQL identifier for `name`.
fn identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

impl SqliteSettings {
    /// `ATTACH` statements for `databases`, each named after its file stem.
    fn attachments(&self) -> Result<Vec<String>, SettingsError> {
        self.databases
            .iter()
            .map(|path| {
                let stem = Path::new(path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| SettingsError::NoFileName(path.clone()))?;
                Ok(format!(
                    "ATTACH DATABASE {} AS {}",
                    literal(path),
                    identifier(stem)
                ))
            })
            .collect()
    }

    async fn pool(&self) -> Result<SqlitePool, BoxError> {
        if !self.params.is_empty() {
            return Err(SettingsError::UnsupportedParams("SQLite").into());
        }
        let path = self.path.to_str().ok_or(SettingsError::NonUtf8Path)?;
        let memory = path == MEMORY;
        let options = if memory {
            SqliteConnectOptions::from_str("sqlite::memory:")?
        } else {
            SqliteConnectOptions::new().filename(path)
        }
        .busy_timeout(self.busy_timeout())
        .disable_statement_logging();
        let attachments = Arc::new(self.attachments()?);
        let mut pool = SqlitePoolOptions::new().after_connect(move |conn, _| {
            let attachments = attachments.clone();
            Box::pin(async move {
                for attach in attachments.iter() {
                    conn.execute(sqlx::raw_sql(AssertSqlSafe(attach.clone())))
                        .await?;
                }
                Ok(())
            })
        });
        if memory {
            // Each connection would otherwise open its own empty database.
            pool = pool
                .max_connections(1)
                .min_connections(1)
                .idle_timeout(None)
                .max_lifetime(None);
        }
        Ok(pool.connect_with(options).await?)
    }
}

impl crate::Connect for SqliteConnectionPool {
    type Settings = SqliteSettings;

    async fn connect(settings: &Self::Settings) -> Result<Self> {
        let inner = settings
            .pool()
            .await
            .map_err(|source| Error::DriverConnect {
                connection: "sqlite".into(),
                source,
            })?;
        Ok(Self { inner })
    }
}

#[derive(Default)]
struct SqliteDecoder;

impl RowDecoder<Sqlite> for SqliteDecoder {
    fn start(&mut self, columns: &[SqliteColumn]) -> Arc<[Column]> {
        Self::columns(columns)
    }

    /// Cells decode by their storage class; SQLite types values, not columns.
    fn decode(&self, row: &SqliteRow) -> Row {
        (0..row.len()).map(|i| decode(row, i)).collect()
    }
}

impl SqliteDecoder {
    fn columns(columns: &[SqliteColumn]) -> Arc<[Column]> {
        columns
            .iter()
            .map(|c| Column::new(c.name(), c.type_info().name().to_lowercase()))
            .collect()
    }
}

fn decode(row: &SqliteRow, i: usize) -> Value {
    let Ok(raw) = row.try_get_raw(i) else {
        return Value::Null;
    };
    if raw.is_null() {
        return Value::Null;
    }
    let class = raw.type_info().name().to_owned();
    let value = match class.as_str() {
        "INTEGER" => row.try_get_unchecked::<i64, _>(i).map(Value::Int),
        "REAL" => row.try_get_unchecked::<f64, _>(i).map(Value::Float),
        "TEXT" => row.try_get_unchecked::<String, _>(i).map(Value::Text),
        _ => row.try_get_unchecked::<Vec<u8>, _>(i).map(Value::Bytes),
    };
    value.unwrap_or_else(|_| {
        row.try_get_unchecked::<Vec<u8>, _>(i)
            .map_or(Value::Null, Value::text_or_bytes)
    })
}

fn bind(
    query: Query<'static, Sqlite, SqliteArguments>,
    value: ParameterValue,
) -> Query<'static, Sqlite, SqliteArguments> {
    match value {
        ParameterValue::Null => query.bind(Option::<String>::None),
        ParameterValue::Boolean(v) => query.bind(v),
        ParameterValue::Int64(v) => query.bind(v),
        ParameterValue::UInt64(v) => match i64::try_from(v) {
            Ok(v) => query.bind(v),
            Err(_) => query.bind(v.to_string()),
        },
        ParameterValue::Float64(v) => query.bind(v),
        ParameterValue::Text(v) => query.bind(v),
        ParameterValue::Binary(v) => query.bind(v),
        ParameterValue::Date(v) => query.bind(v),
        ParameterValue::Time(v) => query.bind(v),
        ParameterValue::Timestamp(v) => query.bind(v),
        ParameterValue::TimestampWithTimezone(v) => query.bind(v),
        ParameterValue::Uuid(v) => query.bind(v.to_string()),
    }
}

impl SqliteConnectionPool {
    /// Unbound SQL runs as a script; bound SQL is one statement. SQLite has
    /// no server, so cancelling stops only Quill's work.
    fn start(&self, request: QueryRequest) -> QueryExecution {
        let pool = self.inner.clone();
        execution::start_with_id(QueryId::new(), move |sender, control| async move {
            if !request.bindings.named.is_empty() {
                return Err(Error::UnsupportedOperation("SQLite named parameters"));
            }
            let mut conn = pool.acquire().await.map_err(query_error)?;
            let sql = AssertSqlSafe(request.sql).into_sql_str();
            let bound = !request.bindings.positional.is_empty();
            let mut out = Emitter::new(&sender, &control);
            let rowless = {
                let stream = if bound {
                    let query = request
                        .bindings
                        .positional
                        .into_iter()
                        .fold(sqlx::query(sql.clone()), bind);
                    (&mut *conn).fetch_many(query)
                } else {
                    (&mut *conn).fetch_many(sqlx::raw_sql(sql.clone()))
                };
                sql::drive(stream, &mut out, &mut SqliteDecoder, |r| r.rows_affected()).await?
            };
            if let Some(rowless) = rowless {
                let columns = if rowless.only {
                    (&mut *conn)
                        .prepare(sql)
                        .await
                        .ok()
                        .map(|statement| SqliteDecoder::columns(statement.columns()))
                } else {
                    None
                };
                sql::finish_rowless(&mut out, rowless, columns).await?;
            }
            Ok(())
        })
    }
}

impl ConnectionPool for SqliteConnectionPool {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            named_parameters: false,
            positional_parameters: true,
        }
    }

    fn execute(&self, request: QueryRequest) -> PoolFuture<'_, QueryExecution> {
        Box::pin(async move { Ok(self.start(request)) })
    }

    fn list_catalogs(&self) -> PoolFuture<'_, Vec<CatalogRef>> {
        Box::pin(async {
            Ok(vec![CatalogRef {
                name: CATALOG.into(),
            }])
        })
    }

    fn list_schemas<'a>(&'a self, catalog: &'a CatalogRef) -> PoolFuture<'a, Vec<SchemaRef>> {
        Box::pin(async move {
            let names: Vec<String> =
                sqlx::query_scalar("SELECT name FROM pragma_database_list ORDER BY seq")
                    .fetch_all(&self.inner)
                    .await
                    .map_err(driver)?;
            Ok(names
                .into_iter()
                .map(|name| SchemaRef {
                    catalog: catalog.clone(),
                    name,
                })
                .collect())
        })
    }

    fn list_tables<'a>(&'a self, schema: &'a SchemaRef) -> PoolFuture<'a, Vec<TableRef>> {
        Box::pin(async move {
            let names: Vec<String> = sqlx::query_scalar(
                "SELECT name FROM pragma_table_list \
                 WHERE schema = ? AND type IN ('table', 'view', 'virtual') \
                   AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name",
            )
            .bind(&schema.name)
            .fetch_all(&self.inner)
            .await
            .map_err(driver)?;
            Ok(names
                .into_iter()
                .map(|name| TableRef {
                    schema: schema.clone(),
                    name,
                })
                .collect())
        })
    }

    fn describe_table<'a>(&'a self, table: &'a TableRef) -> PoolFuture<'a, TableMetadata> {
        Box::pin(async move {
            let rows: Vec<(String, String, bool, i64)> = sqlx::query_as(
                "SELECT name, type, \"notnull\", pk FROM pragma_table_info(?, ?) ORDER BY cid",
            )
            .bind(&table.name)
            .bind(&table.schema.name)
            .fetch_all(&self.inner)
            .await
            .map_err(driver)?;
            if rows.is_empty() {
                return Err(Error::TableNotFound {
                    schema: table.schema.name.clone(),
                    table: table.name.clone(),
                });
            }
            Ok(TableMetadata {
                table: table.clone(),
                columns: rows
                    .into_iter()
                    .map(|(name, native_type, not_null, _)| ColumnMetadata {
                        name,
                        native_type,
                        nullable: Some(!not_null),
                    })
                    .collect(),
            })
        })
    }

    fn list_indexes<'a>(&'a self, table: &'a TableRef) -> PoolFuture<'a, Vec<IndexMetadata>> {
        Box::pin(async move {
            self.describe_table(table).await?;
            let list: Vec<(String, bool, String, bool)> = sqlx::query_as(
                "SELECT name, \"unique\", origin, partial FROM pragma_index_list(?, ?) ORDER BY name",
            )
            .bind(&table.name)
            .bind(&table.schema.name)
            .fetch_all(&self.inner)
            .await
            .map_err(driver)?;
            let mut indexes = Vec::with_capacity(list.len());
            for (name, unique, origin, _partial) in list {
                let columns: Vec<(Option<String>, i64, bool, bool)> = sqlx::query_as(
                    "SELECT name, cid, \"desc\", key FROM pragma_index_xinfo(?, ?) ORDER BY seqno",
                )
                .bind(&name)
                .bind(&table.schema.name)
                .fetch_all(&self.inner)
                .await
                .map_err(driver)?;
                let mut keys = Vec::new();
                for (column, cid, descending, key) in columns {
                    // Non-key entries are the rowid SQLite appends to every index.
                    match (key, column) {
                        (true, Some(column)) => keys.push(IndexKey {
                            source: IndexKeySource::Column(column.clone()),
                            definition: column,
                            ordering: Some(IndexOrdering {
                                descending,
                                nulls_first: !descending,
                            }),
                        }),
                        // cid -2 marks an expression key; SQLite does not report its text.
                        (true, None) if cid == -2 => keys.push(IndexKey {
                            source: IndexKeySource::Expression("<expression>".into()),
                            definition: "<expression>".into(),
                            ordering: None,
                        }),
                        _ => {}
                    }
                }
                indexes.push(IndexMetadata {
                    name,
                    access_method: "btree".into(),
                    unique,
                    primary: origin == "pk",
                    valid: None,
                    keys,
                    included_columns: Vec::new(),
                    predicate: None,
                });
            }
            Ok(indexes)
        })
    }
}
