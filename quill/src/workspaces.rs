//! `[workspace.<name>]` tables: several connections from one file, opened
//! together as one federated session.
use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    sync::Arc,
};

use indexmap::IndexMap;
use quill_core::{
    error::{Error, Result},
    metadata::TableRef,
};
use quill_providers::{
    ConnectionPool, PoolFuture,
    federation::{Member, federate},
    settings::DatafusionSettings,
};
use quill_tui::{ConnectionSource, quote};
use serde::Deserialize;

use crate::connections::{Connection, ConnectionSettings};

/// One `[workspace.<name>]` table as written.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceToml {
    /// `[connection.<name>]` tables in the same file.
    pub connections: Vec<MemberToml>,
    /// Saved query directory, relative to the file's directory.
    pub queries: Option<PathBuf>,
    /// Completed executions whose plan metrics are kept.
    #[serde(default = "default_retained_metrics")]
    pub retained_metrics: u64,
}

const fn default_retained_metrics() -> u64 {
    DatafusionSettings::DEFAULT_RETAINED_METRICS
}

/// A workspace entry: `"pg"`, or `{ conn = "postgres", alias = "pg" }`. The
/// alias, else the connection name, names the member's catalog.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum MemberToml {
    Name(String),
    Aliased { conn: String, alias: Option<String> },
}

impl MemberToml {
    fn connection(&self) -> &str {
        match self {
            Self::Name(name) | Self::Aliased { conn: name, .. } => name,
        }
    }

    fn catalog(&self) -> &str {
        match self {
            Self::Aliased {
                alias: Some(alias), ..
            } => alias,
            _ => self.connection(),
        }
    }
}

/// Why a workspace's `connections` list is unusable.
#[derive(Debug, thiserror::Error)]
pub enum MembersError {
    #[error("workspace `{0}` lists no connections")]
    Empty(String),
    #[error("workspace `{workspace}` names catalog `{name}` twice")]
    Repeated { workspace: String, name: String },
    #[error("workspace `{workspace}` names `{name}`, which is not a connection in this file")]
    Unknown { workspace: String, name: String },
}

/// A workspace's members by catalog name: at least one, each a connection
/// defined in the same file, no catalog repeated. Only [`Members::resolve`]
/// builds one.
#[derive(Debug, Clone)]
pub struct Members(Vec<(String, ConnectionSettings)>);

impl Members {
    /// Look up `entries` among `connections`, the file's `[connection.*]` tables.
    ///
    /// ```
    /// # use indexmap::IndexMap;
    /// # use quill::workspaces::{Members, MemberToml};
    /// let connections = IndexMap::new();
    /// assert!(Members::resolve("w", &[], &connections).is_err());
    /// let missing = MemberToml::Name("missing".into());
    /// assert!(Members::resolve("w", &[missing], &connections).is_err());
    /// ```
    pub fn resolve(
        workspace: &str,
        entries: &[MemberToml],
        connections: &IndexMap<String, Connection>,
    ) -> std::result::Result<Self, MembersError> {
        if entries.is_empty() {
            return Err(MembersError::Empty(workspace.into()));
        }
        let mut members: Vec<(String, ConnectionSettings)> = Vec::with_capacity(entries.len());
        for entry in entries {
            let catalog = entry.catalog();
            if members.iter().any(|(name, _)| name == catalog) {
                let (workspace, name) = (workspace.to_owned(), catalog.to_owned());
                return Err(MembersError::Repeated { workspace, name });
            }
            let connection = connections.get(entry.connection()).ok_or_else(|| {
                let (workspace, name) = (workspace.to_owned(), entry.connection().to_owned());
                MembersError::Unknown { workspace, name }
            })?;
            members.push((catalog.to_owned(), connection.driver.clone()));
        }
        Ok(Self(members))
    }

    pub(crate) fn names(&self) -> impl ExactSizeIterator<Item = &str> {
        self.0.iter().map(|(name, _)| name.as_str())
    }
}

/// A workspace with its query directory resolved against its file.
pub struct ResolvedWorkspace {
    pub members: Members,
    pub query_dir: PathBuf,
    pub retained_metrics: u64,
}

impl ResolvedWorkspace {
    /// Connect every member at once, then federate them.
    async fn open(&self) -> Result<Arc<dyn ConnectionPool>> {
        let connecting = self.members.0.iter().map(|(name, settings)| async move {
            let pool = settings.connect().await?;
            Ok::<_, Error>(Member {
                name: name.clone(),
                pool,
            })
        });
        let members = futures::future::try_join_all(connecting).await?;
        Ok(Arc::new(federate(members, self.retained_metrics).await?))
    }
}

impl ConnectionSource for ResolvedWorkspace {
    fn user(&self) -> Option<&str> {
        None
    }

    fn kind(&self) -> &'static str {
        "workspace"
    }

    /// The member catalogs, e.g. `pg +2`.
    fn host(&self) -> Option<Cow<'_, str>> {
        crate::connections::listed(self.members.names())
    }

    fn connect(&self) -> PoolFuture<'_, Arc<dyn ConnectionPool>> {
        Box::pin(self.open())
    }

    fn query_dir(&self) -> &Path {
        &self.query_dir
    }

    /// Each member is a catalog: `"catalog"."schema"."table"`.
    fn table_reference(&self, table: &TableRef) -> String {
        format!(
            "{}.{}.{}",
            quote(&table.schema.catalog.name, '"'),
            quote(&table.schema.name, '"'),
            quote(&table.name, '"')
        )
    }
}
