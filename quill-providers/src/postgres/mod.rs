//! Native Postgres through sqlx.
mod decode;
mod settings;
pub use settings::{PostgresSettings, SslMode};

use quill_core::{
    error::{Error, Result},
    metadata::{
        CatalogRef, ColumnMetadata, IndexKey, IndexKeySource, IndexMetadata, IndexOrdering,
        SchemaRef, TableMetadata, TableRef,
    },
    query::{ParameterValue, QueryExecution, QueryId, QueryRequest},
};
use secrecy::ExposeSecret;
use sqlx::{
    AssertSqlSafe, ConnectOptions, Executor, PgPool, SqlSafeStr, Statement as _,
    postgres::{PgArguments, PgConnectOptions, PgSslMode},
    query::Query,
};

use crate::{
    Capabilities, ConnectionPool, PoolFuture,
    execution::{self, Emitter},
    sql::{self, query_error},
};
use decode::PgDecoder;

/// Native Postgres connection pool.
pub struct PostgresConnectionPool {
    inner: PgPool,
}

fn driver(error: sqlx::Error) -> Error {
    Error::DriverExecute(error.into())
}

impl SslMode {
    const fn native(self) -> PgSslMode {
        match self {
            Self::Disable => PgSslMode::Disable,
            Self::Prefer => PgSslMode::Prefer,
            Self::Require => PgSslMode::Require,
            Self::VerifyCa => PgSslMode::VerifyCa,
            Self::VerifyFull => PgSslMode::VerifyFull,
        }
    }
}

impl PostgresSettings {
    fn options(&self) -> PgConnectOptions {
        let mut options = PgConnectOptions::new()
            .host(&self.host)
            .port(self.port)
            .database(&self.database)
            .username(&self.user)
            .ssl_mode(self.sslmode.native())
            .options(self.params.iter())
            .disable_statement_logging();
        if let Some(password) = &self.password {
            options = options.password(password.expose_secret());
        }
        if let Some(tls) = &self.tls {
            if let Some(ca) = &tls.ca_cert {
                options = options.ssl_root_cert(ca);
            }
            if let Some(identity) = &tls.identity {
                options = options
                    .ssl_client_cert(&identity.cert)
                    .ssl_client_key(&identity.key);
            }
        }
        options
    }
}

impl crate::Connect for PostgresConnectionPool {
    type Settings = PostgresSettings;

    async fn connect(settings: &Self::Settings) -> Result<Self> {
        let inner = PgPool::connect_with(settings.options())
            .await
            .map_err(|source| Error::DriverConnect {
                connection: "postgres".into(),
                source: source.into(),
            })?;
        Ok(Self { inner })
    }
}

fn bind(
    mut query: Query<'static, sqlx::Postgres, PgArguments>,
    value: ParameterValue,
) -> Query<'static, sqlx::Postgres, PgArguments> {
    query = match value {
        ParameterValue::Null => query.bind(Option::<String>::None),
        ParameterValue::Boolean(v) => query.bind(v),
        ParameterValue::Int64(v) => query.bind(v),
        ParameterValue::UInt64(v) => query.bind(sqlx::types::BigDecimal::from(v)),
        ParameterValue::Float64(v) => query.bind(v),
        ParameterValue::Text(v) => query.bind(v),
        ParameterValue::Binary(v) => query.bind(v),
        ParameterValue::Date(v) => query.bind(v),
        ParameterValue::Time(v) => query.bind(v),
        ParameterValue::Timestamp(v) => query.bind(v),
        ParameterValue::TimestampWithTimezone(v) => query.bind(v),
        ParameterValue::Uuid(v) => query.bind(v),
    };
    query
}

impl PostgresConnectionPool {
    /// Unbound SQL runs as a script over the simple protocol: every statement,
    /// each value as server text. Bound SQL is one extended-protocol statement.
    fn start(&self, request: QueryRequest) -> QueryExecution {
        let pool = self.inner.clone();
        execution::start_with_id(QueryId::new(), move |sender, control| async move {
            if !request.bindings.named.is_empty() {
                return Err(Error::UnsupportedOperation("Postgres named parameters"));
            }
            let mut conn = pool.acquire().await.map_err(query_error)?;
            let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(&mut *conn)
                .await
                .map_err(query_error)?;
            control.on_server_cancel(move || {
                tokio::spawn(async move {
                    let _ = sqlx::query("SELECT pg_cancel_backend($1)")
                        .bind(pid)
                        .execute(&pool)
                        .await;
                });
            });
            let sql = AssertSqlSafe(request.sql).into_sql_str();
            let bound = !request.bindings.positional.is_empty();
            let mut out = Emitter::new(&sender, &control);
            let mut decoder = PgDecoder::default();
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

    /// Columns, or an error when the table is not visible to this role.
    async fn columns(&self, table: &TableRef) -> Result<Vec<ColumnMetadata>> {
        let rows: Vec<(String, String, bool)> = sqlx::query_as(
            "SELECT a.attname::text, pg_catalog.format_type(a.atttypid, a.atttypmod), \
                    NOT (a.attnotnull OR t.typnotnull) \
             FROM pg_catalog.pg_attribute a \
             JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             JOIN pg_catalog.pg_type t ON t.oid = a.atttypid \
             WHERE current_database() = $1 AND n.nspname = $2 AND c.relname = $3 \
               AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum",
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
        Ok(rows
            .into_iter()
            .map(|(name, native_type, nullable)| ColumnMetadata {
                name,
                native_type,
                nullable: Some(nullable),
            })
            .collect())
    }
}

/// One key or included column of an index, in index order.
type IndexRow = (
    String,
    String,
    bool,
    bool,
    bool,
    Option<String>,
    bool,
    Option<String>,
    String,
    Option<bool>,
    Option<bool>,
);

impl ConnectionPool for PostgresConnectionPool {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            named_parameters: false,
            positional_parameters: true,
        }
    }

    fn execute(&self, request: QueryRequest) -> PoolFuture<'_, QueryExecution> {
        Box::pin(async move { Ok(self.start(request)) })
    }

    // Postgres connections can inspect only their current database.
    fn list_catalogs(&self) -> PoolFuture<'_, Vec<CatalogRef>> {
        Box::pin(async {
            let name: String = sqlx::query_scalar("SELECT current_database()::text")
                .fetch_one(&self.inner)
                .await
                .map_err(driver)?;
            Ok(vec![CatalogRef { name }])
        })
    }

    fn list_schemas<'a>(&'a self, catalog: &'a CatalogRef) -> PoolFuture<'a, Vec<SchemaRef>> {
        Box::pin(async move {
            let names: Vec<String> = sqlx::query_scalar(
                "SELECT schema_name::text FROM information_schema.schemata \
                 WHERE catalog_name = $1 ORDER BY schema_name",
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
                "SELECT table_name::text FROM information_schema.tables \
                 WHERE table_catalog = $1 AND table_schema = $2 ORDER BY table_name",
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

    fn list_indexes<'a>(&'a self, table: &'a TableRef) -> PoolFuture<'a, Vec<IndexMetadata>> {
        Box::pin(async move {
            self.columns(table).await?;
            let rows: Vec<IndexRow> = sqlx::query_as(
                "SELECT ic.relname::text, am.amname::text,
                        i.indisunique, i.indisprimary, i.indisvalid,
                        pg_catalog.pg_get_expr(i.indpred, i.indrelid),
                        k.ordinality <= i.indnkeyatts,
                        a.attname::text,
                        pg_catalog.pg_get_indexdef(i.indexrelid, k.ordinality::int, false),
                        CASE WHEN am.amname = 'btree' THEN
                            (i.indoption[(k.ordinality - 1)::int] & 1) <> 0 END,
                        CASE WHEN am.amname = 'btree' THEN
                            (i.indoption[(k.ordinality - 1)::int] & 2) <> 0 END
                 FROM pg_catalog.pg_namespace n
                 JOIN pg_catalog.pg_class c ON c.relnamespace = n.oid
                 JOIN pg_catalog.pg_index i ON i.indrelid = c.oid
                 JOIN pg_catalog.pg_class ic ON ic.oid = i.indexrelid
                 JOIN pg_catalog.pg_am am ON am.oid = ic.relam
                 JOIN LATERAL unnest(i.indkey) WITH ORDINALITY k(attnum, ordinality) ON true
                 LEFT JOIN pg_catalog.pg_attribute a ON a.attrelid = c.oid AND a.attnum = k.attnum
                 WHERE current_database() = $1 AND n.nspname = $2 AND c.relname = $3
                 ORDER BY ic.relname, k.ordinality",
            )
            .bind(&table.schema.catalog.name)
            .bind(&table.schema.name)
            .bind(&table.name)
            .fetch_all(&self.inner)
            .await
            .map_err(driver)?;
            let mut indexes: Vec<IndexMetadata> = Vec::new();
            for (
                name,
                method,
                unique,
                primary,
                valid,
                predicate,
                key,
                column,
                definition,
                desc,
                nulls_first,
            ) in rows
            {
                if indexes.last().is_none_or(|index| index.name != name) {
                    indexes.push(IndexMetadata {
                        name,
                        access_method: method,
                        unique,
                        primary,
                        valid: Some(valid),
                        keys: Vec::new(),
                        included_columns: Vec::new(),
                        predicate,
                    });
                }
                let Some(index) = indexes.last_mut() else {
                    continue;
                };
                if key {
                    index.keys.push(IndexKey {
                        source: column.map_or_else(
                            || IndexKeySource::Expression(definition.clone()),
                            IndexKeySource::Column,
                        ),
                        definition,
                        ordering: desc.zip(nulls_first).map(|(descending, nulls_first)| {
                            IndexOrdering {
                                descending,
                                nulls_first,
                            }
                        }),
                    });
                } else if let Some(column) = column {
                    index.included_columns.push(column);
                }
            }
            Ok(indexes)
        })
    }

    fn describe_table<'a>(&'a self, table: &'a TableRef) -> PoolFuture<'a, TableMetadata> {
        Box::pin(async move {
            Ok(TableMetadata {
                table: table.clone(),
                columns: self.columns(table).await?,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONNECTION: &str = "host = 'localhost'\ndatabase = 'app'\nuser = 'user'\n";

    #[test]
    fn defaults_to_require() {
        let settings: PostgresSettings = toml::from_str(CONNECTION).unwrap();
        assert_eq!(settings.sslmode, SslMode::Require);
        assert!(matches!(
            settings.options().get_ssl_mode(),
            PgSslMode::Require
        ));
    }

    #[test]
    fn rejects_unknown_sslmode() {
        assert!(
            toml::from_str::<PostgresSettings>(&format!("{CONNECTION}sslmode = 'invalid'"))
                .is_err()
        );
    }
}
