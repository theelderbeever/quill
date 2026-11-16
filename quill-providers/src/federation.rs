//! Federated sessions: several connections queried together through
//! DataFusion, each exposed as a catalog named after its connection.
use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use datafusion::{
    catalog::{CatalogProvider, SchemaProvider, TableProvider},
    error::DataFusionError,
    execution::{
        object_store::{DefaultObjectStoreRegistry, ObjectStoreRegistry},
        runtime_env::RuntimeEnvBuilder,
        session_state::SessionStateBuilder,
    },
    prelude::{SessionConfig, SessionContext},
};
use datafusion_federation::{FederatedQueryPlanner, default_optimizer_rules};
use quill_core::{
    error::Result,
    metadata::{SchemaRef, TableRef},
};

use crate::{ConnectionPool, datafusion::DatafusionConnectionPool};

/// One connection joining a federated session; `name` becomes its catalog.
pub struct Member {
    pub name: String,
    pub pool: Arc<dyn ConnectionPool>,
}

/// A session over `members` that pushes each connection's share of a query
/// down to it, in its own dialect, and joins the rest in DataFusion.
///
/// Members' tables are listed once here; later schema changes need a new
/// session. There is no default catalog, so tables are named
/// `"connection"."schema"."table"`.
///
/// ```no_run
/// use std::sync::Arc;
/// use quill_providers::{ConnectionPool, federation::{Member, federate}};
///
/// async fn join(pg: Arc<dyn ConnectionPool>, ch: Arc<dyn ConnectionPool>) -> quill_core::error::Result<()> {
///     let members = vec![
///         Member { name: "pg".into(), pool: pg },
///         Member { name: "ch".into(), pool: ch },
///     ];
///     let pool = federate(members, 256).await?;
///     let frame = pool.inner().sql(r#"SELECT * FROM "pg"."public"."users" u
///         JOIN "ch"."default"."events" e ON e.user_id = u.id"#).await;
///     Ok(())
/// }
/// ```
pub async fn federate(
    members: Vec<Member>,
    retained_metrics: u64,
) -> Result<DatafusionConnectionPool> {
    let stores = MemberStores {
        members: members
            .iter()
            .filter_map(|m| m.pool.object_stores())
            .collect(),
        own: DefaultObjectStoreRegistry::new(),
    };
    let runtime = RuntimeEnvBuilder::new()
        .with_object_store_registry(Arc::new(stores))
        .build_arc()
        .map_err(|e| quill_core::error::Error::DriverExecute(e.into()))?;
    let config = SessionConfig::new().with_create_default_catalog_and_schema(false);
    let state = SessionStateBuilder::new()
        .with_config(config)
        .with_runtime_env(runtime)
        .with_default_features()
        .with_optimizer_rules(default_optimizer_rules())
        .with_query_planner(Arc::new(FederatedQueryPlanner::new()))
        .build();
    let ctx = SessionContext::new_with_state(state);
    for member in members {
        let catalog = MemberCatalog::list(member.pool).await?;
        ctx.register_catalog(member.name, Arc::new(catalog));
    }
    Ok(DatafusionConnectionPool::from_context(
        ctx,
        retained_metrics,
    ))
}

/// Members' object stores, so their file-backed tables can be scanned from
/// the federated session. Should two members register the same URL, the
/// first listed wins; stores registered on the session itself come last.
#[derive(Debug)]
struct MemberStores {
    members: Vec<Arc<dyn ObjectStoreRegistry>>,
    own: DefaultObjectStoreRegistry,
}

impl ObjectStoreRegistry for MemberStores {
    fn register_store(
        &self,
        url: &url::Url,
        store: Arc<dyn object_store::ObjectStore>,
    ) -> Option<Arc<dyn object_store::ObjectStore>> {
        self.own.register_store(url, store)
    }

    fn get_store(
        &self,
        url: &url::Url,
    ) -> datafusion::error::Result<Arc<dyn object_store::ObjectStore>> {
        self.members
            .iter()
            .find_map(|registry| registry.get_store(url).ok())
            .map_or_else(|| self.own.get_store(url), Ok)
    }
}

/// A member's schemas, flattened across its own catalogs. Should two of its
/// catalogs share a schema name, the first listed wins.
#[derive(Debug)]
struct MemberCatalog {
    schemas: BTreeMap<String, Arc<MemberSchema>>,
}

impl MemberCatalog {
    async fn list(pool: Arc<dyn ConnectionPool>) -> Result<Self> {
        let mut schemas = BTreeMap::new();
        for catalog in pool.list_catalogs().await? {
            for schema in pool.list_schemas(&catalog).await? {
                if schemas.contains_key(&schema.name) {
                    continue;
                }
                let tables = pool
                    .list_tables(&schema)
                    .await?
                    .into_iter()
                    .map(|t| t.name)
                    .collect();
                let name = schema.name.clone();
                let member = MemberSchema {
                    pool: pool.clone(),
                    schema,
                    tables,
                };
                schemas.insert(name, Arc::new(member));
            }
        }
        Ok(Self { schemas })
    }
}

impl CatalogProvider for MemberCatalog {
    fn schema_names(&self) -> Vec<String> {
        self.schemas.keys().cloned().collect()
    }

    fn schema(&self, name: &str) -> Option<Arc<dyn SchemaProvider>> {
        self.schemas
            .get(name)
            .map(|s| s.clone() as Arc<dyn SchemaProvider>)
    }
}

/// Tables listed when the session was built, resolved on first use.
struct MemberSchema {
    pool: Arc<dyn ConnectionPool>,
    schema: SchemaRef,
    tables: Vec<String>,
}

impl std::fmt::Debug for MemberSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemberSchema")
            .field("schema", &self.schema)
            .field("tables", &self.tables)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl SchemaProvider for MemberSchema {
    fn table_names(&self) -> Vec<String> {
        self.tables.clone()
    }

    async fn table(&self, name: &str) -> datafusion::error::Result<Option<Arc<dyn TableProvider>>> {
        if !self.table_exist(name) {
            return Ok(None);
        }
        let table = TableRef {
            schema: self.schema.clone(),
            name: name.to_owned(),
        };
        let provider = self
            .pool
            .table_provider(&table)
            .await
            .map_err(|e| DataFusionError::External(Box::new(e)))?;
        Ok(Some(provider))
    }

    fn table_exist(&self, name: &str) -> bool {
        self.tables.iter().any(|t| t == name)
    }
}
