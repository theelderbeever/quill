mod settings;
pub use settings::ScyllaSettings;

use crate::ConnectionPool;
use quill_core::error::{Error, Result};
use scylla::client::{session::Session, session_builder::SessionBuilder};
use secrecy::ExposeSecret;

/// Native Scylla session, which manages the cluster's connection pools.
pub struct ScyllaConnectionPool {
    inner: Session,
}

impl ScyllaConnectionPool {
    /// Access the native cluster session.
    pub fn inner(&self) -> &Session {
        &self.inner
    }
}

impl ConnectionPool for ScyllaConnectionPool {
    type Settings = ScyllaSettings;

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
            Ok::<_, anyhow::Error>(Self { inner })
        };
        connect.await.map_err(|source| Error::DriverConnect {
            connection: "scylla".into(),
            source,
        })
    }
}
