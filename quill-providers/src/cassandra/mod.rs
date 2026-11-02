mod settings;
pub use settings::CassandraSettings;

use std::sync::Arc;

use datafusion::{catalog::TableProvider, common::TableReference};
use datafusion_table_providers::{
    cassandra::CassandraTableFactory, sql::db_connection_pool::cassandrapool,
};
use quill_core::error::{Error, Result};
use scylla::{
    client::{session::Session, session_builder::SessionBuilder},
    value::CqlValue,
};
use secrecy::ExposeSecret;

use crate::{
    PoolFuture,
    provider::{ProviderPool, Tables, factory_table},
};

pub type CassandraConnectionPool = ProviderPool<Arc<Session>, CqlValue>;

/// CQL has no catalogs; one fixed name keeps keyspaces addressable.
const CATALOG: &str = "cassandra";

impl CassandraSettings {
    /// Build the session here rather than through provider params, which
    /// cannot express TLS.
    async fn session(&self) -> anyhow::Result<Session> {
        anyhow::ensure!(
            self.params.is_empty(),
            "Cassandra parameters are not yet supported"
        );
        let mut builder = SessionBuilder::new().known_nodes(&self.nodes);
        if let Some(datacenter) = &self.datacenter {
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
        match (&self.user, &self.password) {
            (Some(user), Some(password)) => builder = builder.user(user, password.expose_secret()),
            (None, None) => {}
            _ => anyhow::bail!("Cassandra authentication requires both user and password"),
        }
        if let Some(tls) = &self.tls {
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
        let session = builder.build().await?;
        if let Some(keyspace) = &self.keyspace {
            session.use_keyspace(keyspace, true).await?;
        }
        Ok(session)
    }
}

impl crate::Connect for CassandraConnectionPool {
    type Settings = CassandraSettings;

    async fn connect(settings: &Self::Settings) -> Result<Self> {
        let session = settings
            .session()
            .await
            .map_err(|source| Error::DriverConnect {
                connection: "cassandra".into(),
                source,
            })?;
        let pool = cassandrapool::CassandraConnectionPool::from_session(Arc::new(session));
        let pool = Arc::new(pool);
        let tables = Arc::new(CassandraTableFactory::new(pool.clone()));
        Ok(Self::new(pool, tables, CATALOG))
    }
}

impl Tables for CassandraTableFactory {
    fn capabilities(&self) -> crate::Capabilities {
        crate::Capabilities {
            named_parameters: false,
            positional_parameters: true,
        }
    }
    fn table(&self, table: TableReference) -> PoolFuture<'_, Arc<dyn TableProvider>> {
        Box::pin(async move { factory_table(self.table_provider(table).await) })
    }
}
