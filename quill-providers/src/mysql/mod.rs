//! Native MySQL through sqlx.
mod settings;
pub use settings::MysqlSettings;

use crate::settings::SettingsError;
use quill_core::error::BoxError;
use std::sync::Arc;

use quill_core::{
    error::{Error, Result},
    metadata::{
        CatalogRef, ColumnMetadata, IndexKey, IndexKeySource, IndexMetadata, IndexOrdering,
        SchemaRef, TableMetadata, TableRef,
    },
    query::{ParameterValue, QueryExecution, QueryId, QueryRequest},
    value::{Column, Row, Value},
};
use secrecy::ExposeSecret;
use sqlx::{
    AssertSqlSafe, Column as _, ConnectOptions, Executor, MySql, MySqlPool, Row as _, SqlSafeStr,
    Statement as _, TypeInfo, ValueRef,
    mysql::{MySqlArguments, MySqlColumn, MySqlConnectOptions, MySqlRow, MySqlSslMode},
    query::Query,
    types::{BigDecimal, chrono},
};

use crate::{
    Capabilities, ConnectionPool, PoolFuture,
    execution::{self, Emitter},
    sql::{self, RowDecoder, query_error},
};

/// MySQL reports one catalog for every schema.
const CATALOG: &str = "def";

/// Native MySQL connection pool.
pub struct MysqlConnectionPool {
    inner: MySqlPool,
}

fn driver(error: sqlx::Error) -> Error {
    Error::DriverExecute(error.into())
}

impl MysqlSettings {
    fn options(&self) -> Result<MySqlConnectOptions, SettingsError> {
        if !self.params.is_empty() {
            return Err(SettingsError::UnsupportedParams("MySQL"));
        }
        let mut options = MySqlConnectOptions::new()
            .host(&self.host)
            .port(self.port)
            .database(&self.database)
            .username(&self.user)
            .ssl_mode(MySqlSslMode::Disabled)
            .disable_statement_logging();
        if let Some(password) = &self.password {
            options = options.password(password.expose_secret());
        }
        if let Some(tls) = &self.tls {
            options = options.ssl_mode(MySqlSslMode::VerifyIdentity);
            if let Some(ca) = &tls.ca_cert {
                options = options.ssl_ca(ca);
            }
            if let Some(identity) = &tls.identity {
                options = options
                    .ssl_client_cert(&identity.cert)
                    .ssl_client_key(&identity.key);
            }
        }
        Ok(options)
    }
}

impl crate::Connect for MysqlConnectionPool {
    type Settings = MysqlSettings;

    async fn connect(settings: &Self::Settings) -> Result<Self> {
        let connect = async {
            let inner = MySqlPool::connect_with(settings.options()?).await?;
            Ok::<_, BoxError>(Self { inner })
        };
        connect.await.map_err(|source| Error::DriverConnect {
            connection: "mysql".into(),
            source,
        })
    }
}

#[derive(Debug, Clone, Copy)]
enum Kind {
    Bool,
    Int,
    UInt,
    Float,
    Decimal,
    Text,
    Bytes,
    Date,
    Time,
    DateTime,
    Json,
    Other,
}

impl Kind {
    fn of(column: &MySqlColumn) -> Self {
        match column.type_info().name() {
            "BOOLEAN" => Self::Bool,
            "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "BIGINT" | "YEAR" => Self::Int,
            "TINYINT UNSIGNED" | "SMALLINT UNSIGNED" | "MEDIUMINT UNSIGNED" | "INT UNSIGNED"
            | "BIGINT UNSIGNED" => Self::UInt,
            "FLOAT" | "DOUBLE" => Self::Float,
            "DECIMAL" => Self::Decimal,
            "CHAR" | "VARCHAR" | "TINYTEXT" | "TEXT" | "MEDIUMTEXT" | "LONGTEXT" | "ENUM"
            | "SET" => Self::Text,
            "BINARY" | "VARBINARY" | "TINYBLOB" | "BLOB" | "MEDIUMBLOB" | "LONGBLOB"
            | "GEOMETRY" | "BIT" => Self::Bytes,
            "DATE" => Self::Date,
            "TIME" => Self::Time,
            "DATETIME" | "TIMESTAMP" => Self::DateTime,
            "JSON" => Self::Json,
            _ => Self::Other,
        }
    }
}

#[derive(Default)]
struct MysqlDecoder {
    kinds: Vec<Kind>,
}

impl MysqlDecoder {
    fn columns(&mut self, columns: &[MySqlColumn]) -> Arc<[Column]> {
        self.kinds = columns.iter().map(Kind::of).collect();
        columns
            .iter()
            .map(|c| Column::new(c.name(), c.type_info().name().to_lowercase()))
            .collect()
    }
}

/// The value's bytes: server text over the text protocol.
fn raw(row: &MySqlRow, i: usize) -> Value {
    match row.try_get_unchecked::<Option<Vec<u8>>, _>(i) {
        Ok(Some(bytes)) => match Value::text_or_bytes(bytes) {
            Value::Text(text) => Value::Other(text),
            bytes => bytes,
        },
        _ => Value::Null,
    }
}

fn decode(row: &MySqlRow, i: usize, kind: Kind) -> Option<Value> {
    if row.try_get_raw(i).ok()?.is_null() {
        return Some(Value::Null);
    }
    macro_rules! get {
        ($t:ty, $f:expr) => {
            row.try_get_unchecked::<$t, _>(i).ok().map($f)
        };
    }
    match kind {
        Kind::Bool => get!(bool, Value::Bool),
        Kind::Int => get!(i64, Value::Int),
        Kind::UInt => get!(u64, Value::UInt),
        Kind::Float => get!(f64, Value::Float),
        Kind::Decimal => get!(BigDecimal, |v| Value::decimal_or_other(
            &v.to_plain_string()
        )),
        Kind::Text => get!(String, Value::Text),
        Kind::Bytes => get!(Vec<u8>, Value::Bytes),
        Kind::Date => get!(chrono::NaiveDate, Value::Date),
        Kind::Time => get!(chrono::NaiveTime, Value::Time),
        Kind::DateTime => get!(chrono::NaiveDateTime, Value::Timestamp),
        Kind::Json => get!(String, Value::Json),
        Kind::Other => None,
    }
}

impl RowDecoder<MySql> for MysqlDecoder {
    fn start(&mut self, columns: &[MySqlColumn]) -> Arc<[Column]> {
        self.columns(columns)
    }

    fn decode(&self, row: &MySqlRow) -> Row {
        self.kinds
            .iter()
            .enumerate()
            .map(|(i, kind)| decode(row, i, *kind).unwrap_or_else(|| raw(row, i)))
            .collect()
    }
}

fn bind(
    query: Query<'static, MySql, MySqlArguments>,
    value: ParameterValue,
) -> Query<'static, MySql, MySqlArguments> {
    match value {
        ParameterValue::Null => query.bind(Option::<String>::None),
        ParameterValue::Boolean(v) => query.bind(v),
        ParameterValue::Int64(v) => query.bind(v),
        ParameterValue::UInt64(v) => query.bind(v),
        ParameterValue::Float64(v) => query.bind(v),
        ParameterValue::Text(v) => query.bind(v),
        ParameterValue::Binary(v) => query.bind(v),
        ParameterValue::Date(v) => query.bind(v),
        ParameterValue::Time(v) => query.bind(v),
        ParameterValue::Timestamp(v) => query.bind(v),
        ParameterValue::TimestampWithTimezone(v) => query.bind(v.naive_utc()),
        ParameterValue::Uuid(v) => query.bind(v.to_string()),
    }
}

impl MysqlConnectionPool {
    /// Unbound SQL runs as a script over the text protocol; bound SQL is one
    /// prepared statement.
    fn start(&self, request: QueryRequest) -> QueryExecution {
        let pool = self.inner.clone();
        execution::start_with_id(QueryId::new(), move |sender, control| async move {
            if !request.bindings.named.is_empty() {
                return Err(Error::UnsupportedOperation("MySQL named parameters"));
            }
            let mut conn = pool.acquire().await.map_err(query_error)?;
            let id: u64 = sqlx::query_scalar("SELECT CONNECTION_ID()")
                .fetch_one(&mut *conn)
                .await
                .map_err(query_error)?;
            control.on_server_cancel(move || {
                tokio::spawn(async move {
                    let kill = AssertSqlSafe(format!("KILL QUERY {id}"));
                    let _ = sqlx::raw_sql(kill).execute(&pool).await;
                });
            });
            let sql = AssertSqlSafe(request.sql).into_sql_str();
            let bound = !request.bindings.positional.is_empty();
            let mut out = Emitter::new(&sender, &control);
            let mut decoder = MysqlDecoder::default();
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
                sql::drive(stream, &mut out, &mut decoder, |r| r.rows_affected()).await?
            };
            if let Some(rowless) = rowless {
                let columns = if rowless.only {
                    (&mut *conn)
                        .prepare(sql)
                        .await
                        .ok()
                        .map(|statement| decoder.columns(statement.columns()))
                } else {
                    None
                };
                sql::finish_rowless(&mut out, rowless, columns).await?;
            }
            Ok(())
        })
    }
}

/// One column of an index, in index order.
type IndexRow = (
    String,
    i64,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
);

impl ConnectionPool for MysqlConnectionPool {
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
            let names: Vec<String> = sqlx::query_scalar(
                "SELECT CAST(SCHEMA_NAME AS CHAR) FROM information_schema.SCHEMATA \
                 WHERE CATALOG_NAME = ? ORDER BY SCHEMA_NAME",
            )
            .bind(&catalog.name)
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
                "SELECT CAST(TABLE_NAME AS CHAR) FROM information_schema.TABLES \
                 WHERE TABLE_CATALOG = ? AND TABLE_SCHEMA = ? ORDER BY TABLE_NAME",
            )
            .bind(&schema.catalog.name)
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
            let rows: Vec<(String, String, String)> = sqlx::query_as(
                "SELECT CAST(COLUMN_NAME AS CHAR), CAST(COLUMN_TYPE AS CHAR), \
                        CAST(IS_NULLABLE AS CHAR) \
                 FROM information_schema.COLUMNS \
                 WHERE TABLE_CATALOG = ? AND TABLE_SCHEMA = ? AND TABLE_NAME = ? \
                 ORDER BY ORDINAL_POSITION",
            )
            .bind(&table.schema.catalog.name)
            .bind(&table.schema.name)
            .bind(&table.name)
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
                    .map(|(name, native_type, nullable)| ColumnMetadata {
                        name,
                        native_type,
                        nullable: Some(nullable == "YES"),
                    })
                    .collect(),
            })
        })
    }

    fn list_indexes<'a>(&'a self, table: &'a TableRef) -> PoolFuture<'a, Vec<IndexMetadata>> {
        Box::pin(async move {
            self.describe_table(table).await?;
            let rows: Vec<IndexRow> = sqlx::query_as(
                "SELECT CAST(INDEX_NAME AS CHAR), CAST(NON_UNIQUE AS SIGNED), \
                        CAST(INDEX_TYPE AS CHAR), CAST(COLUMN_NAME AS CHAR), \
                        CAST(EXPRESSION AS CHAR), CAST(COLLATION AS CHAR), \
                        CAST(SUB_PART AS SIGNED) \
                 FROM information_schema.STATISTICS \
                 WHERE TABLE_CATALOG = ? AND TABLE_SCHEMA = ? AND TABLE_NAME = ? \
                 ORDER BY INDEX_NAME, SEQ_IN_INDEX",
            )
            .bind(&table.schema.catalog.name)
            .bind(&table.schema.name)
            .bind(&table.name)
            .fetch_all(&self.inner)
            .await
            .map_err(driver)?;
            let mut indexes: Vec<IndexMetadata> = Vec::new();
            for (name, non_unique, method, column, expression, collation, prefix) in rows {
                if indexes.last().is_none_or(|index| index.name != name) {
                    indexes.push(IndexMetadata {
                        primary: name == "PRIMARY",
                        name,
                        access_method: method,
                        unique: non_unique == 0,
                        valid: None,
                        keys: Vec::new(),
                        included_columns: Vec::new(),
                        predicate: None,
                    });
                }
                let Some(index) = indexes.last_mut() else {
                    continue;
                };
                let (source, mut definition) = match (column, expression) {
                    (Some(column), _) => (IndexKeySource::Column(column.clone()), column),
                    (None, Some(expression)) => {
                        (IndexKeySource::Expression(expression.clone()), expression)
                    }
                    (None, None) => continue,
                };
                if let Some(prefix) = prefix {
                    definition = format!("{definition}({prefix})");
                }
                index.keys.push(IndexKey {
                    source,
                    definition,
                    ordering: collation.map(|c| IndexOrdering {
                        descending: c == "D",
                        nulls_first: c != "D",
                    }),
                });
            }
            Ok(indexes)
        })
    }
}
