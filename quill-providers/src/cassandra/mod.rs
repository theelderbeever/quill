//! Native Cassandra and ScyllaDB through the scylla driver.
mod settings;
pub use settings::CassandraSettings;

use crate::settings::SettingsError;
use quill_core::error::BoxError;
use std::{collections::BTreeMap, sync::Arc};

use futures::TryStreamExt;
use quill_core::{
    error::{Error, Result},
    metadata::{
        CatalogRef, ColumnMetadata, IndexKey, IndexKeySource, IndexMetadata, SchemaRef,
        TableMetadata, TableRef,
    },
    query::{ParameterValue, QueryExecution, QueryId, QueryRequest},
    value::{Column, Decimal, Row, Value},
};
use scylla::{
    client::{session::Session, session_builder::SessionBuilder},
    frame::response::result::{CollectionType, ColumnType},
    value::{CqlValue, Row as CqlRow},
};
use secrecy::ExposeSecret;

use crate::{
    Capabilities, ConnectionPool, PoolFuture,
    execution::{self, Emitter},
};

/// CQL has no catalogs; one fixed name keeps keyspaces addressable.
const CATALOG: &str = "cassandra";

/// Native Cassandra session.
pub struct CassandraConnectionPool {
    session: Arc<Session>,
}

fn driver(error: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::DriverExecute(error.into())
}

fn query_error(error: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::QueryExecution(Box::new(error))
}

impl CassandraSettings {
    /// A session honoring TLS, authentication and datacenter preference.
    async fn session(&self) -> Result<Session, BoxError> {
        if !self.params.is_empty() {
            return Err(SettingsError::UnsupportedParams("Cassandra").into());
        }
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
            _ => return Err(SettingsError::IncompleteCredentials.into()),
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
        Ok(Self {
            session: Arc::new(session),
        })
    }
}

/// The CQL spelling of a column type.
fn cql_type(ty: &ColumnType<'_>) -> String {
    match ty {
        ColumnType::Native(native) => format!("{native:?}").to_lowercase(),
        ColumnType::Collection { frozen, typ } => {
            let inner = match typ {
                CollectionType::List(t) => format!("list<{}>", cql_type(t)),
                CollectionType::Set(t) => format!("set<{}>", cql_type(t)),
                CollectionType::Map(k, v) => format!("map<{}, {}>", cql_type(k), cql_type(v)),
                other => format!("{other:?}").to_lowercase(),
            };
            if *frozen {
                format!("frozen<{inner}>")
            } else {
                inner
            }
        }
        ColumnType::Vector { typ, dimensions } => {
            format!("vector<{}, {dimensions}>", cql_type(typ))
        }
        ColumnType::UserDefinedType { definition, .. } => definition.name.to_string(),
        ColumnType::Tuple(types) => format!(
            "tuple<{}>",
            types.iter().map(cql_type).collect::<Vec<_>>().join(", ")
        ),
        other => format!("{other:?}").to_lowercase(),
    }
}

/// A CQL literal for values without a [`Value`] of their own.
fn literal(value: &CqlValue) -> String {
    match value {
        CqlValue::Ascii(s) | CqlValue::Text(s) => format!("'{}'", s.replace('\'', "''")),
        CqlValue::Map(pairs) => format!(
            "{{{}}}",
            pairs
                .iter()
                .map(|(k, v)| format!("{}: {}", literal(k), literal(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        CqlValue::Tuple(items) => format!(
            "({})",
            items
                .iter()
                .map(optional_literal)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        CqlValue::UserDefinedType { fields, .. } => format!(
            "{{{}}}",
            fields
                .iter()
                .map(|(name, v)| format!("{name}: {}", optional_literal(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        CqlValue::List(items) | CqlValue::Set(items) | CqlValue::Vector(items) => format!(
            "[{}]",
            items.iter().map(literal).collect::<Vec<_>>().join(", ")
        ),
        other => value_of(other.clone()).to_string(),
    }
}

fn optional_literal(value: &Option<CqlValue>) -> String {
    value.as_ref().map_or_else(|| "null".into(), literal)
}

fn value_of(value: CqlValue) -> Value {
    use bigdecimal::BigDecimal;
    match value {
        CqlValue::Ascii(s) | CqlValue::Text(s) => Value::Text(s),
        CqlValue::Boolean(v) => Value::Bool(v),
        CqlValue::Blob(v) => Value::Bytes(v),
        CqlValue::Counter(v) => Value::Int(v.0),
        CqlValue::Decimal(v) => {
            let decimal = BigDecimal::from(v).to_plain_string();
            Decimal::parse(&decimal).map_or(Value::Other(decimal), Value::Decimal)
        }
        CqlValue::Varint(v) => {
            Value::decimal_or_other(&bigdecimal::num_bigint::BigInt::from(v).to_string())
        }
        CqlValue::Date(v) => TryInto::<chrono::NaiveDate>::try_into(v)
            .map_or_else(|_| Value::Other(format!("{}", v.0)), Value::Date),
        CqlValue::Double(v) => Value::Float(v),
        CqlValue::Float(v) => Value::Float(v.into()),
        CqlValue::Int(v) => Value::Int(v.into()),
        CqlValue::BigInt(v) => Value::Int(v),
        CqlValue::SmallInt(v) => Value::Int(v.into()),
        CqlValue::TinyInt(v) => Value::Int(v.into()),
        CqlValue::Timestamp(v) => TryInto::<chrono::DateTime<chrono::Utc>>::try_into(v)
            .map_or_else(
                |_| Value::Other(v.0.to_string()),
                |t| Value::TimestampTz(t.fixed_offset()),
            ),
        CqlValue::Time(v) => TryInto::<chrono::NaiveTime>::try_into(v)
            .map_or_else(|_| Value::Other(v.0.to_string()), Value::Time),
        CqlValue::Uuid(v) => Value::Uuid(v),
        CqlValue::Timeuuid(v) => Value::Uuid(uuid::Uuid::from_bytes(*v.as_bytes())),
        CqlValue::Inet(v) => Value::Other(v.to_string()),
        CqlValue::Duration(v) => {
            Value::Other(format!("{}mo{}d{}ns", v.months, v.days, v.nanoseconds))
        }
        CqlValue::Empty => Value::Null,
        CqlValue::List(items) | CqlValue::Set(items) | CqlValue::Vector(items) => {
            Value::List(items.into_iter().map(value_of).collect())
        }
        other => Value::Other(literal(&other)),
    }
}

fn row(cql: CqlRow) -> Row {
    cql.columns
        .into_iter()
        .map(|cell| cell.map_or(Value::Null, value_of))
        .collect()
}

fn cql_value(value: ParameterValue) -> Option<CqlValue> {
    Some(match value {
        ParameterValue::Null => return None,
        ParameterValue::Boolean(v) => CqlValue::Boolean(v),
        ParameterValue::Int64(v) => CqlValue::BigInt(v),
        ParameterValue::UInt64(v) => match i64::try_from(v) {
            Ok(v) => CqlValue::BigInt(v),
            Err(_) => CqlValue::Text(v.to_string()),
        },
        ParameterValue::Float64(v) => CqlValue::Double(v),
        ParameterValue::Text(v) => CqlValue::Text(v),
        ParameterValue::Binary(v) => CqlValue::Blob(v),
        ParameterValue::Date(v) => CqlValue::Date(v.into()),
        ParameterValue::Time(v) => CqlValue::Time(v.try_into().ok()?),
        ParameterValue::Timestamp(v) => CqlValue::Timestamp(v.and_utc().into()),
        ParameterValue::TimestampWithTimezone(v) => CqlValue::Timestamp(v.to_utc().into()),
        ParameterValue::Uuid(v) => CqlValue::Uuid(v),
    })
}

impl CassandraConnectionPool {
    /// CQL has no server-side cancellation; cancelling stops paging.
    fn start(&self, request: QueryRequest) -> QueryExecution {
        let session = self.session.clone();
        execution::start_with_id(QueryId::new(), move |sender, control| async move {
            let QueryRequest { sql, bindings } = request;
            let pager = match (bindings.positional.is_empty(), bindings.named.is_empty()) {
                (true, true) => session.query_iter(sql, ()).await,
                (false, true) => {
                    let values: Vec<_> = bindings.positional.into_iter().map(cql_value).collect();
                    session.query_iter(sql, values).await
                }
                (true, false) => {
                    let values: BTreeMap<String, _> = bindings
                        .named
                        .into_iter()
                        .map(|(k, v)| (k, cql_value(v)))
                        .collect();
                    session.query_iter(sql, values).await
                }
                (false, false) => {
                    return Err(Error::UnsupportedOperation(
                        "mixing positional and named CQL parameters",
                    ));
                }
            }
            .map_err(query_error)?;
            let columns: Arc<[Column]> = pager
                .column_specs()
                .iter()
                .map(|spec| Column::new(spec.name(), cql_type(spec.typ())))
                .collect();
            let mut out = Emitter::new(&sender, &control);
            if columns.is_empty() {
                return out.complete(None).await;
            }
            out.columns(columns).await?;
            let mut rows = pager.rows_stream::<CqlRow>().map_err(query_error)?;
            while let Some(cql) = rows.try_next().await.map_err(query_error)? {
                out.row(row(cql)).await?;
            }
            out.complete(None).await
        })
    }

    /// The first column of every row of `cql`.
    async fn names(&self, cql: &'static str, values: Vec<&str>) -> Result<Vec<String>> {
        let rows: Vec<(String,)> = self
            .session
            .query_iter(cql, values)
            .await
            .map_err(driver)?
            .rows_stream::<(String,)>()
            .map_err(driver)?
            .try_collect()
            .await
            .map_err(driver)?;
        Ok(rows.into_iter().map(|(name,)| name).collect())
    }
}

impl ConnectionPool for CassandraConnectionPool {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            named_parameters: true,
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
            let mut names = self
                .names(
                    "SELECT keyspace_name FROM system_schema.keyspaces",
                    Vec::new(),
                )
                .await?;
            names.sort_unstable();
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
            let mut names = self
                .names(
                    "SELECT table_name FROM system_schema.tables WHERE keyspace_name = ?",
                    vec![schema.name.as_str()],
                )
                .await?;
            names.sort_unstable();
            Ok(names
                .into_iter()
                .map(|name| TableRef {
                    schema: schema.clone(),
                    name,
                })
                .collect())
        })
    }

    /// Partition key columns first, then clustering columns, then the rest.
    fn describe_table<'a>(&'a self, table: &'a TableRef) -> PoolFuture<'a, TableMetadata> {
        Box::pin(async move {
            let mut rows: Vec<(String, String, String, i32)> = self
                .session
                .query_iter(
                    "SELECT column_name, type, kind, position FROM system_schema.columns \
                     WHERE keyspace_name = ? AND table_name = ?",
                    (&table.schema.name, &table.name),
                )
                .await
                .map_err(driver)?
                .rows_stream::<(String, String, String, i32)>()
                .map_err(driver)?
                .try_collect()
                .await
                .map_err(driver)?;
            if rows.is_empty() {
                return Err(Error::TableNotFound {
                    schema: table.schema.name.clone(),
                    table: table.name.clone(),
                });
            }
            let rank = |kind: &str| match kind {
                "partition_key" => 0,
                "clustering" => 1,
                _ => 2,
            };
            rows.sort_by(|a, b| (rank(&a.2), a.3, &a.0).cmp(&(rank(&b.2), b.3, &b.0)));
            Ok(TableMetadata {
                table: table.clone(),
                columns: rows
                    .into_iter()
                    .map(|(name, native_type, kind, _)| ColumnMetadata {
                        nullable: Some(rank(&kind) == 2),
                        name,
                        native_type,
                    })
                    .collect(),
            })
        })
    }

    fn list_indexes<'a>(&'a self, table: &'a TableRef) -> PoolFuture<'a, Vec<IndexMetadata>> {
        Box::pin(async move {
            self.describe_table(table).await?;
            let rows: Vec<(String, String, BTreeMap<String, String>)> = self
                .session
                .query_iter(
                    "SELECT index_name, kind, options FROM system_schema.indexes \
                     WHERE keyspace_name = ? AND table_name = ?",
                    (&table.schema.name, &table.name),
                )
                .await
                .map_err(driver)?
                .rows_stream::<(String, String, BTreeMap<String, String>)>()
                .map_err(driver)?
                .try_collect()
                .await
                .map_err(driver)?;
            let mut indexes: Vec<IndexMetadata> = rows
                .into_iter()
                .map(|(name, kind, options)| {
                    let target = options.get("target").cloned().unwrap_or_default();
                    IndexMetadata {
                        name,
                        access_method: options.get("class_name").cloned().unwrap_or(kind),
                        unique: false,
                        primary: false,
                        valid: None,
                        keys: vec![IndexKey {
                            source: IndexKeySource::Expression(target.clone()),
                            definition: target,
                            ordering: None,
                        }],
                        included_columns: Vec::new(),
                        predicate: None,
                    }
                })
                .collect();
            indexes.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(indexes)
        })
    }
}
