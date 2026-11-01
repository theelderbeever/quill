//! Load one level of the catalog tree from the connection.
use std::sync::Arc;

use quill_core::metadata::{CatalogRef, ColumnMetadata, SchemaRef, TableRef};
use quill_drivers::ConnectionPool;
use tokio::sync::mpsc::UnboundedSender;

use crate::{connection::ConnectionId, event::Msg};

/// What to load beneath a node.
pub enum Load {
    Catalogs,
    Schemas(CatalogRef),
    Tables(SchemaRef),
    Columns(TableRef),
}

pub enum Children {
    Catalogs(Vec<CatalogRef>),
    Schemas(Vec<SchemaRef>),
    Tables(Vec<TableRef>),
    Columns(Vec<ColumnMetadata>),
}

pub struct CatalogMsg {
    pub connection: ConnectionId,
    /// Tree generation the request was made for; bumped on refresh.
    pub generation: u64,
    /// Child indices from the root to the node that was expanded.
    pub path: Vec<usize>,
    pub result: quill_core::error::Result<Children>,
}

pub fn spawn(
    pool: Arc<dyn ConnectionPool>,
    connection: ConnectionId,
    generation: u64,
    path: Vec<usize>,
    load: Load,
    messages: UnboundedSender<Msg>,
) {
    tokio::spawn(async move {
        let result = match &load {
            Load::Catalogs => pool.list_catalogs().await.map(Children::Catalogs),
            Load::Schemas(catalog) => pool.list_schemas(catalog).await.map(Children::Schemas),
            Load::Tables(schema) => pool.list_tables(schema).await.map(Children::Tables),
            Load::Columns(table) => pool
                .describe_table(table)
                .await
                .map(|t| Children::Columns(t.columns)),
        };
        let msg = CatalogMsg {
            connection,
            generation,
            path,
            result,
        };
        let _ = messages.send(Msg::Catalog(msg));
    });
}
