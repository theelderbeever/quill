pub use execute::ExecutionError;
mod decimal;
mod execute;
mod metadata;
use metadata::{ColumnRow, IndexRow, NameRow, SchemaRow, TableRow};

mod settings;
pub use settings::PostgresSettings;

use crate::{ConnectionPool, PoolFuture};
use quill_core::{
    error::{Error, Result},
    metadata::{
        CatalogRef, ColumnMetadata, IndexKey, IndexMetadata, SchemaRef, TableMetadata, TableRef,
    },
};
use secrecy::ExposeSecret;
use sqlx::{
    ConnectOptions, PgPool,
    postgres::{PgConnectOptions, PgSslMode},
};

/// Native Postgres connection pool.
pub struct PostgresConnectionPool {
    pub(crate) inner: PgPool,
}

impl PostgresConnectionPool {
    /// Access the native pool for driver-specific operations.
    pub fn inner(&self) -> &PgPool {
        &self.inner
    }
}

impl ConnectionPool for PostgresConnectionPool {
    type Settings = PostgresSettings;

    fn capabilities(&self) -> crate::Capabilities {
        crate::Capabilities {
            named_parameters: false,
            positional_parameters: true,
        }
    }

    fn execute(
        &self,
        request: quill_core::query::QueryRequest,
    ) -> PoolFuture<'_, quill_core::query::QueryExecution> {
        Box::pin(async move { Ok(self.start_execution(request)) })
    }

    // PostgreSQL connections can inspect only their current database.
    fn list_catalogs(&self) -> PoolFuture<'_, Vec<CatalogRef>> {
        Box::pin(async {
            let row: NameRow = sqlx::query_as("SELECT current_database() AS name")
                .fetch_one(&self.inner)
                .await
                .map_err(|error| Error::DriverExecute(error.into()))?;
            Ok(vec![CatalogRef::from(row)])
        })
    }

    fn list_schemas<'a>(&'a self, catalog: &'a CatalogRef) -> PoolFuture<'a, Vec<SchemaRef>> {
        Box::pin(async move {
            let names: Vec<NameRow> = sqlx::query_as(
                "SELECT schema_name AS name FROM information_schema.schemata \
                 WHERE catalog_name = $1 ORDER BY schema_name",
            )
            .bind(&catalog.name)
            .fetch_all(&self.inner)
            .await
            .map_err(|error| Error::DriverExecute(error.into()))?;
            Ok(names
                .into_iter()
                .map(|row| SchemaRef::from(SchemaRow { row, catalog }))
                .collect())
        })
    }

    fn list_tables<'a>(&'a self, schema: &'a SchemaRef) -> PoolFuture<'a, Vec<TableRef>> {
        Box::pin(async move {
            let names: Vec<NameRow> = sqlx::query_as(
                "SELECT table_name AS name FROM information_schema.tables \
                 WHERE table_catalog = $1 AND table_schema = $2 ORDER BY table_name",
            )
            .bind(&schema.catalog.name)
            .bind(&schema.name)
            .fetch_all(&self.inner)
            .await
            .map_err(|error| Error::DriverExecute(error.into()))?;
            Ok(names
                .into_iter()
                .map(|row| TableRef::from(TableRow { row, schema }))
                .collect())
        })
    }

    fn list_indexes<'a>(&'a self, table: &'a TableRef) -> PoolFuture<'a, Vec<IndexMetadata>> {
        Box::pin(async move {
            self.describe_table(table).await?;
            let rows: Vec<IndexRow> = sqlx::query_as(
                "SELECT ic.relname::text AS name, am.amname::text AS access_method,
                        i.indisunique, i.indisprimary, i.indisvalid,
                        pg_catalog.pg_get_expr(i.indpred, i.indrelid) AS predicate,
                        k.ordinality::bigint AS position,
                        k.ordinality <= i.indnkeyatts AS is_key,
                        a.attname::text AS column_name,
                        pg_catalog.pg_get_indexdef(i.indexrelid, k.ordinality::int, false) AS definition,
                        CASE WHEN am.amname = 'btree' THEN
                            (i.indoption[(k.ordinality - 1)::int] & 1) <> 0 END AS descending,
                        CASE WHEN am.amname = 'btree' THEN
                            (i.indoption[(k.ordinality - 1)::int] & 2) <> 0 END AS nulls_first
                 FROM information_schema.tables t
                 JOIN pg_catalog.pg_namespace n ON n.nspname = t.table_schema
                 JOIN pg_catalog.pg_class c ON c.relnamespace = n.oid AND c.relname = t.table_name
                 JOIN pg_catalog.pg_index i ON i.indrelid = c.oid
                 JOIN pg_catalog.pg_class ic ON ic.oid = i.indexrelid
                 JOIN pg_catalog.pg_am am ON am.oid = ic.relam
                 JOIN LATERAL unnest(i.indkey) WITH ORDINALITY k(attnum, ordinality) ON true
                 LEFT JOIN pg_catalog.pg_attribute a ON a.attrelid = c.oid AND a.attnum = k.attnum
                 WHERE t.table_catalog = $1 AND t.table_schema = $2 AND t.table_name = $3
                 ORDER BY ic.relname, k.ordinality"
            ).bind(&table.schema.catalog.name).bind(&table.schema.name).bind(&table.name)
                .fetch_all(&self.inner).await
                .map_err(|error| Error::DriverExecute(error.into()))?;
            let mut indexes: Vec<IndexMetadata> = Vec::new();
            for row in rows {
                if indexes.last().is_none_or(|index| index.name != row.name) {
                    indexes.push(IndexMetadata::from(&row));
                }
                if let Some(index) = indexes.last_mut() {
                    if row.is_key {
                        index.keys.push(IndexKey::from(row));
                    } else if let Some(column) = row.column_name {
                        index.included_columns.push(column);
                    }
                }
            }
            Ok(indexes)
        })
    }

    fn describe_table<'a>(&'a self, table: &'a TableRef) -> PoolFuture<'a, TableMetadata> {
        Box::pin(async move {
            let mut connection = self
                .inner
                .acquire()
                .await
                .map_err(|error| Error::DriverExecute(error.into()))?;
            // Resolve by separate bound identifiers, including privilege filtering.
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT FROM information_schema.tables \
                 WHERE table_catalog = $1 AND table_schema = $2 AND table_name = $3)",
            )
            .bind(&table.schema.catalog.name)
            .bind(&table.schema.name)
            .bind(&table.name)
            .fetch_one(&mut *connection)
            .await
            .map_err(|error| Error::DriverExecute(error.into()))?;
            if !exists {
                return Err(Error::DriverExecute(sqlx::Error::RowNotFound.into()));
            }
            let rows: Vec<ColumnRow> = sqlx::query_as(
                "SELECT a.attname::text AS name, pg_catalog.format_type(a.atttypid, a.atttypmod) AS native_type, \
                        NOT (a.attnotnull OR t.typnotnull) AS nullable \
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
            .fetch_all(&mut *connection)
            .await
            .map_err(|error| Error::DriverExecute(error.into()))?;
            Ok(TableMetadata {
                table: table.clone(),
                columns: rows.into_iter().map(ColumnMetadata::from).collect(),
            })
        })
    }

    async fn connect(settings: &Self::Settings) -> Result<Self> {
        let connect = async {
            if !settings.params.is_empty() {
                anyhow::bail!("Postgres parameters are not yet supported");
            }
            let mut options = PgConnectOptions::new()
                .host(&settings.host)
                .port(settings.port)
                .database(&settings.database)
                .username(&settings.user)
                .ssl_mode(PgSslMode::Disable);
            if let Some(password) = &settings.password {
                options = options.password(password.expose_secret());
            }
            if let Some(tls) = &settings.tls {
                options = options.ssl_mode(PgSslMode::VerifyFull);
                if let Some(ca) = &tls.ca_cert {
                    options = options.ssl_root_cert(ca);
                }
                if let Some(identity) = &tls.identity {
                    options = options
                        .ssl_client_cert(&identity.cert)
                        .ssl_client_key(&identity.key);
                }
            }
            let inner = PgPool::connect_with(options.disable_statement_logging()).await?;
            Ok::<_, anyhow::Error>(Self { inner })
        };
        connect.await.map_err(|source| Error::DriverConnect {
            connection: "postgres".into(),
            source,
        })
    }
}
