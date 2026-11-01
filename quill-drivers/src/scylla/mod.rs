mod execute;
mod metadata;
mod result;
use metadata::{ColumnRow, IndexRow, NameRow, SchemaRow, TableRow};

mod settings;
pub use settings::ScyllaSettings;

use crate::{ConnectionPool, PoolFuture};
use quill_core::{
    error::{Error, Result},
    metadata::*,
};
use scylla::client::{session::Session, session_builder::SessionBuilder};
use secrecy::ExposeSecret;

/// Native Scylla session, which manages the cluster's connection pools.
pub struct ScyllaConnectionPool {
    inner: std::sync::Arc<Session>,
}

impl ScyllaConnectionPool {
    /// Access the native cluster session.
    pub fn inner(&self) -> &Session {
        &self.inner
    }
}

impl ConnectionPool for ScyllaConnectionPool {
    fn capabilities(&self) -> crate::Capabilities {
        crate::Capabilities {
            named_parameters: true,
            positional_parameters: true,
        }
    }
    fn execute(
        &self,
        request: quill_core::query::QueryRequest,
    ) -> PoolFuture<'_, quill_core::query::QueryExecution> {
        let session = self.inner.clone();
        Box::pin(async move {
            Ok(crate::execution::start(move |sender, control| async move {
                Self::run(session, request, sender, control).await
            }))
        })
    }
    type Settings = ScyllaSettings;

    fn list_catalogs(&self) -> PoolFuture<'_, Vec<CatalogRef>> {
        Box::pin(async {
            Ok(vec![CatalogRef {
                name: "scylla".into(),
            }])
        })
    }
    fn list_schemas<'a>(&'a self, catalog: &'a CatalogRef) -> PoolFuture<'a, Vec<SchemaRef>> {
        Box::pin(async move {
            if catalog.name != "scylla" {
                return Ok(Vec::new());
            }
            let work = async {
                let result = self
                    .inner
                    .query_unpaged(
                        "SELECT keyspace_name AS name FROM system_schema.keyspaces",
                        (),
                    )
                    .await?
                    .into_rows_result()?;
                let mut schemas = Vec::new();
                for row in result.rows::<NameRow>()? {
                    schemas.push(SchemaRef::from(SchemaRow { row: row?, catalog }));
                }
                schemas.sort_by(|a, b| a.name.cmp(&b.name));
                Ok::<_, anyhow::Error>(schemas)
            };
            work.await.map_err(Error::DriverExecute)
        })
    }
    fn list_tables<'a>(&'a self, schema: &'a SchemaRef) -> PoolFuture<'a, Vec<TableRef>> {
        Box::pin(async move {
            if schema.catalog.name != "scylla" {
                return Ok(Vec::new());
            }
            let work = async {
                let statement = self
                    .inner
                    .prepare("SELECT table_name AS name FROM system_schema.tables WHERE keyspace_name = ?")
                    .await?;
                let result = self
                    .inner
                    .execute_unpaged(&statement, (&schema.name,))
                    .await?
                    .into_rows_result()?;
                let mut tables = Vec::new();
                for row in result.rows::<NameRow>()? {
                    tables.push(TableRef::from(TableRow { row: row?, schema }));
                }
                tables.sort_by(|a, b| a.name.cmp(&b.name));
                Ok::<_, anyhow::Error>(tables)
            };
            work.await.map_err(Error::DriverExecute)
        })
    }
    fn list_indexes<'a>(&'a self, table: &'a TableRef) -> PoolFuture<'a, Vec<IndexMetadata>> {
        Box::pin(async move {
            self.describe_table(table).await?;
            let work = async {
                let statement = self.inner.prepare("SELECT index_name, kind, options FROM system_schema.indexes WHERE keyspace_name = ? AND table_name = ?").await?;
                let result = self
                    .inner
                    .execute_unpaged(&statement, (&table.schema.name, &table.name))
                    .await?
                    .into_rows_result()?;
                let mut indexes = Vec::new();
                for row in result.rows::<IndexRow>()? {
                    indexes.push(IndexMetadata::try_from(row?)?);
                }
                indexes.sort_by(|a, b| a.name.cmp(&b.name));
                Ok::<_, anyhow::Error>(indexes)
            };
            work.await.map_err(Error::DriverExecute)
        })
    }

    fn describe_table<'a>(&'a self, table: &'a TableRef) -> PoolFuture<'a, TableMetadata> {
        Box::pin(async move {
            if table.schema.catalog.name != "scylla" {
                return Err(Error::DriverExecute(anyhow::anyhow!("catalog not found")));
            }
            let work = async {
                let statement = self.inner.prepare("SELECT column_name, type AS native_type, kind, position FROM system_schema.columns WHERE keyspace_name = ? AND table_name = ?").await?;
                let result = self
                    .inner
                    .execute_unpaged(&statement, (&table.schema.name, &table.name))
                    .await?
                    .into_rows_result()?;
                let mut rows = Vec::new();
                for row in result.rows::<ColumnRow>()? {
                    rows.push(row?);
                }
                if rows.is_empty() {
                    anyhow::bail!("table not found");
                }
                rows.sort_by(|a, b| {
                    let rank = |kind: &str| match kind {
                        "partition_key" => 0,
                        "clustering" => 1,
                        _ => 2,
                    };
                    (rank(&a.kind), a.position, &a.column_name).cmp(&(
                        rank(&b.kind),
                        b.position,
                        &b.column_name,
                    ))
                });
                Ok::<_, anyhow::Error>(TableMetadata {
                    table: table.clone(),
                    columns: rows.into_iter().map(ColumnMetadata::from).collect(),
                })
            };
            work.await.map_err(Error::DriverExecute)
        })
    }

    async fn connect(settings: &Self::Settings) -> Result<Self> {
        let connect = async {
            anyhow::ensure!(
                settings.params.is_empty(),
                "Scylla parameters are not yet supported"
            );
            let mut builder = SessionBuilder::new().known_nodes(&settings.nodes);
            if let Some(datacenter) = &settings.datacenter {
                use scylla::{
                    client::execution_profile::ExecutionProfile,
                    policies::load_balancing::DefaultPolicy,
                };
                let policy = DefaultPolicy::builder()
                    .prefer_datacenter(datacenter.clone())
                    .build();
                let profile = ExecutionProfile::builder()
                    .load_balancing_policy(policy)
                    .build();
                builder = builder.default_execution_profile_handle(profile.into_handle());
            }
            match (&settings.user, &settings.password) {
                (Some(user), Some(password)) => {
                    builder = builder.user(user, password.expose_secret())
                }
                (None, None) => {}
                _ => anyhow::bail!("Scylla authentication requires both user and password"),
            }
            if let Some(tls) = &settings.tls {
                use openssl::ssl::{SslConnector, SslFiletype, SslMethod};
                let mut context = SslConnector::builder(SslMethod::tls_client())?;
                if let Some(ca) = &tls.ca_cert {
                    context.set_ca_file(ca)?;
                }
                if let Some(identity) = &tls.identity {
                    context.set_certificate_chain_file(&identity.cert)?;
                    context.set_private_key_file(&identity.key, SslFiletype::PEM)?;
                    context.check_private_key()?;
                }
                builder = builder.tls_context(Some(context));
            }
            let inner = builder.build().await?;
            if let Some(keyspace) = &settings.keyspace {
                inner.use_keyspace(keyspace, true).await?;
            }
            Ok::<_, anyhow::Error>(Self {
                inner: std::sync::Arc::new(inner),
            })
        };
        connect.await.map_err(|source| Error::DriverConnect {
            connection: "scylla".into(),
            source,
        })
    }
}
