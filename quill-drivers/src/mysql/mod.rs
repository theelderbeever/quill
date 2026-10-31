mod metadata;
use metadata::{ColumnRow, IndexRow, NameRow, SchemaRow, TableRow};

mod settings;
pub use settings::MysqlSettings;

use crate::{ConnectionPool, PoolFuture};
use quill_core::{
    error::{Error, Result},
    metadata::*,
};
use secrecy::ExposeSecret;
use sqlx::{
    ConnectOptions, MySqlPool,
    mysql::{MySqlConnectOptions, MySqlSslMode},
};

/// Native Mysql connection pool.
pub struct MysqlConnectionPool {
    pub(crate) inner: MySqlPool,
}

impl MysqlConnectionPool {
    /// Access the native pool for driver-specific operations.
    pub fn inner(&self) -> &MySqlPool {
        &self.inner
    }
}

impl ConnectionPool for MysqlConnectionPool {
    type Settings = MysqlSettings;

    fn list_catalogs(&self) -> PoolFuture<'_, Vec<CatalogRef>> {
        Box::pin(async { Ok(vec![CatalogRef { name: "def".into() }]) })
    }

    fn list_schemas<'a>(&'a self, catalog: &'a CatalogRef) -> PoolFuture<'a, Vec<SchemaRef>> {
        Box::pin(async move {
            let names: Vec<NameRow> = sqlx::query_as("SELECT CAST(SCHEMA_NAME AS CHAR) AS name FROM information_schema.SCHEMATA WHERE CATALOG_NAME = ? ORDER BY SCHEMA_NAME")
                .bind(&catalog.name).fetch_all(&self.inner).await.map_err(|e| Error::DriverExecute(e.into()))?;
            Ok(names
                .into_iter()
                .map(|row| SchemaRef::from(SchemaRow { row, catalog }))
                .collect())
        })
    }

    fn list_tables<'a>(&'a self, schema: &'a SchemaRef) -> PoolFuture<'a, Vec<TableRef>> {
        Box::pin(async move {
            let names: Vec<NameRow> = sqlx::query_as("SELECT CAST(TABLE_NAME AS CHAR) AS name FROM information_schema.TABLES WHERE TABLE_CATALOG = ? AND TABLE_SCHEMA = ? ORDER BY TABLE_NAME")
                .bind(&schema.catalog.name).bind(&schema.name).fetch_all(&self.inner).await.map_err(|e| Error::DriverExecute(e.into()))?;
            Ok(names
                .into_iter()
                .map(|row| TableRef::from(TableRow { row, schema }))
                .collect())
        })
    }

    fn describe_table<'a>(&'a self, table: &'a TableRef) -> PoolFuture<'a, TableMetadata> {
        Box::pin(async move {
            let rows: Vec<ColumnRow> = sqlx::query_as("SELECT CAST(COLUMN_NAME AS CHAR) AS name, CAST(COLUMN_TYPE AS CHAR) AS native_type, CAST(IS_NULLABLE AS CHAR) AS nullable FROM information_schema.COLUMNS WHERE TABLE_CATALOG = ? AND TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION")
                .bind(&table.schema.catalog.name).bind(&table.schema.name).bind(&table.name).fetch_all(&self.inner).await.map_err(|e| Error::DriverExecute(e.into()))?;
            if rows.is_empty() {
                return Err(Error::DriverExecute(sqlx::Error::RowNotFound.into()));
            }
            Ok(TableMetadata {
                table: table.clone(),
                columns: rows.into_iter().map(ColumnMetadata::from).collect(),
            })
        })
    }

    fn list_indexes<'a>(&'a self, table: &'a TableRef) -> PoolFuture<'a, Vec<IndexMetadata>> {
        Box::pin(async move {
            self.describe_table(table).await?;
            let rows: Vec<IndexRow> = sqlx::query_as(
                "SELECT CAST(INDEX_NAME AS CHAR) AS name, CAST(NON_UNIQUE AS SIGNED) AS non_unique, CAST(INDEX_TYPE AS CHAR) AS method, CAST(COLUMN_NAME AS CHAR) AS column_name, CAST(EXPRESSION AS CHAR) AS expression, CAST(COLLATION AS CHAR) AS direction, CAST(SUB_PART AS SIGNED) AS prefix FROM information_schema.STATISTICS WHERE TABLE_CATALOG = ? AND TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY INDEX_NAME, SEQ_IN_INDEX")
                .bind(&table.schema.catalog.name).bind(&table.schema.name).bind(&table.name).fetch_all(&self.inner).await.map_err(|e| Error::DriverExecute(e.into()))?;
            let mut indexes: Vec<IndexMetadata> = Vec::new();
            for row in rows {
                if indexes.last().is_none_or(|i| i.name != row.name) {
                    indexes.push(IndexMetadata::from(&row));
                }
                if let Some(index) = indexes.last_mut() {
                    index.keys.push(IndexKey::try_from(row)?);
                }
            }
            Ok(indexes)
        })
    }

    async fn connect(settings: &Self::Settings) -> Result<Self> {
        let connect = async {
            if !settings.params.is_empty() {
                anyhow::bail!("Mysql parameters are not yet supported");
            }
            let mut options = MySqlConnectOptions::new()
                .host(&settings.host)
                .port(settings.port)
                .database(&settings.database)
                .username(&settings.user)
                .ssl_mode(MySqlSslMode::Disabled);
            if let Some(password) = &settings.password {
                options = options.password(password.expose_secret());
            }
            if let Some(tls) = &settings.tls {
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
            let inner = MySqlPool::connect_with(options.disable_statement_logging()).await?;
            Ok::<_, anyhow::Error>(Self { inner })
        };
        connect.await.map_err(|source| Error::DriverConnect {
            connection: "mysql".into(),
            source,
        })
    }
}
