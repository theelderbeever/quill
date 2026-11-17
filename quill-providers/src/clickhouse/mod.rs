//! Native ClickHouse over HTTP. Results arrive as server-formatted text.
mod query_log;
mod settings;
mod tsv;
pub use settings::ClickhouseSettings;

use quill_core::{
    error::{Error, Result},
    metadata::{
        CatalogRef, ColumnMetadata, IndexKey, IndexKeySource, IndexMetadata, SchemaRef,
        TableMetadata, TableRef,
    },
    query::{ParameterValue, QueryExecution, QueryId, QueryRequest, ServerMetrics},
};
use secrecy::ExposeSecret;

use crate::{
    Capabilities, ConnectionPool, PoolFuture,
    execution::{self, Emitter},
};
use query_log::QueryLog;

/// ClickHouse has no catalogs; one fixed name keeps databases addressable.
const CATALOG: &str = "clickhouse";

/// Result format: names, then types, then rows, all as escaped text.
const FORMAT: &str = "TabSeparatedWithNamesAndTypes";

/// Native ClickHouse client.
pub struct ClickhouseConnectionPool {
    client: clickhouse::Client,
    query_log: QueryLog,
}

fn driver(error: clickhouse::error::Error) -> Error {
    Error::DriverExecute(error.into())
}

fn query_error(error: clickhouse::error::Error) -> Error {
    Error::QueryExecution(Box::new(error))
}

/// Every row of `query` as optional text cells, for metadata lookups.
pub(super) async fn text_rows(
    query: clickhouse::query::Query,
) -> clickhouse::error::Result<Vec<Vec<Option<String>>>> {
    let mut cursor = query.fetch_bytes("TabSeparated")?;
    let mut lines = tsv::Lines::default();
    let mut rows = Vec::new();
    while let Some(chunk) = cursor.next().await? {
        lines.push(&chunk);
        while let Some(line) = lines.next() {
            rows.push(
                tsv::fields(&line)
                    .into_iter()
                    .map(|f| f.map(|b| String::from_utf8_lossy(&b).into_owned()))
                    .collect(),
            );
        }
    }
    Ok(rows)
}

/// The first cell of each row.
async fn names(query: clickhouse::query::Query) -> Result<Vec<String>> {
    Ok(text_rows(query)
        .await
        .map_err(driver)?
        .into_iter()
        .filter_map(|row| row.into_iter().next().flatten())
        .collect())
}

impl ClickhouseSettings {
    /// An HTTP client honoring a custom CA and client identity.
    async fn client(&self) -> anyhow::Result<clickhouse::Client> {
        anyhow::ensure!(
            self.user.is_some() || self.password.is_none(),
            "ClickHouse password requires a user"
        );
        let scheme = if self.tls.is_some() { "https" } else { "http" };
        let endpoint = url::Url::parse(&format!("{scheme}://{}", self.host))?;
        anyhow::ensure!(
            endpoint.host_str().is_some()
                && endpoint.username().is_empty()
                && endpoint.password().is_none()
                && endpoint.path() == "/"
                && endpoint.query().is_none()
                && endpoint.fragment().is_none(),
            "ClickHouse host must contain only a hostname and optional port"
        );
        let mut client = clickhouse::Client::default();
        if let Some(tls) = &self.tls {
            let mut builder = native_tls::TlsConnector::builder();
            if let Some(ca) = &tls.ca_cert {
                let pem = tokio::fs::read(ca).await?;
                for cert in openssl::x509::X509::stack_from_pem(&pem)? {
                    builder
                        .add_root_certificate(native_tls::Certificate::from_der(&cert.to_der()?)?);
                }
            }
            if let Some(identity) = &tls.identity {
                let cert = tokio::fs::read(&identity.cert).await?;
                let key =
                    secrecy::SecretString::from(tokio::fs::read_to_string(&identity.key).await?);
                builder.identity(native_tls::Identity::from_pkcs8(
                    &cert,
                    key.expose_secret().as_bytes(),
                )?);
            }
            let mut http = hyper_util::client::legacy::connect::HttpConnector::new();
            http.enforce_http(false);
            let connector = hyper_tls::HttpsConnector::from((
                http,
                tokio_native_tls::TlsConnector::from(builder.build()?),
            ));
            let http =
                hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                    .build(connector);
            client = clickhouse::Client::with_http_client(http);
        }
        client = client
            .with_url(endpoint.as_str())
            .with_database(&self.database);
        if let Some(user) = &self.user {
            client = client.with_user(user);
        }
        if let Some(password) = &self.password {
            client = client.with_password(password.expose_secret());
        }
        for (key, value) in &self.params {
            client = client.with_setting(key, value);
        }
        Ok(client)
    }
}

impl crate::Connect for ClickhouseConnectionPool {
    type Settings = ClickhouseSettings;

    async fn connect(settings: &Self::Settings) -> Result<Self> {
        let connect = async {
            let client = settings.client().await?;
            // Probing the query log also checks the connection.
            let query_log = QueryLog::probe(&client).await?;
            anyhow::Ok(Self { client, query_log })
        };
        connect.await.map_err(|source| Error::DriverConnect {
            connection: "clickhouse".into(),
            source,
        })
    }
}

fn bind(
    query: clickhouse::query::Query,
    name: Option<&str>,
    value: ParameterValue,
) -> clickhouse::query::Query {
    macro_rules! bind {
        ($value:expr) => {
            match name {
                Some(name) => query.param(name, $value),
                None => query.bind($value),
            }
        };
    }
    match value {
        ParameterValue::Null => bind!(Option::<String>::None),
        ParameterValue::Boolean(v) => bind!(v),
        ParameterValue::Int64(v) => bind!(v),
        ParameterValue::UInt64(v) => bind!(v),
        ParameterValue::Float64(v) => bind!(v),
        ParameterValue::Text(v) => bind!(v),
        ParameterValue::Binary(v) => bind!(String::from_utf8_lossy(&v).into_owned()),
        ParameterValue::Date(v) => bind!(v.to_string()),
        ParameterValue::Time(v) => bind!(v.to_string()),
        ParameterValue::Timestamp(v) => bind!(v.to_string()),
        ParameterValue::TimestampWithTimezone(v) => bind!(v.to_rfc3339()),
        ParameterValue::Uuid(v) => bind!(v.to_string()),
    }
}

impl ClickhouseConnectionPool {
    /// One statement per request; `?` placeholders are bound client-side only
    /// when positional values are given, so literal `?` survives otherwise.
    fn start(&self, request: QueryRequest) -> QueryExecution {
        let client = self.client.clone();
        let id = QueryId::new();
        execution::start_with_id(id, move |sender, control| async move {
            let query_id = Self::server_query_id(id);
            let mut query = if request.bindings.positional.is_empty() {
                client.query_raw(&request.sql)
            } else {
                client.query(&request.sql)
            };
            for value in request.bindings.positional {
                query = bind(query, None, value);
            }
            for (name, value) in request.bindings.named {
                query = bind(query, Some(&name), value);
            }
            let kill = client.clone();
            let killed = query_id.clone();
            control.on_server_cancel(move || {
                tokio::spawn(async move {
                    let _ = kill
                        .query("KILL QUERY WHERE query_id = ? ASYNC")
                        .bind(killed)
                        .execute()
                        .await;
                });
            });
            let mut cursor = query
                .with_setting("query_id", query_id)
                .fetch_bytes(FORMAT)
                .map_err(query_error)?;
            let mut out = Emitter::new(&sender, &control);
            let mut lines = tsv::Lines::default();
            let mut names = None;
            let mut decoder: Option<tsv::Decoder> = None;
            while let Some(chunk) = cursor.next().await.map_err(query_error)? {
                lines.push(&chunk);
                while let Some(line) = lines.next() {
                    if let Some(decoder) = &decoder {
                        out.row(decoder.row(&line)).await?;
                        continue;
                    }
                    match names.take() {
                        None => names = Some(tsv::fields(&line)),
                        Some(names) => {
                            let started = tsv::Decoder::new(names, tsv::fields(&line));
                            out.columns(started.columns()).await?;
                            decoder = Some(started);
                        }
                    }
                }
            }
            if lines.has_remainder() {
                return Err(Error::MissingMetadata("complete ClickHouse result row"));
            }
            let affected = cursor.summary().and_then(|s| s.written_rows());
            match decoder {
                Some(_) => out.complete(None).await,
                None => out.complete(affected).await,
            }
        })
    }
}

impl ConnectionPool for ClickhouseConnectionPool {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            named_parameters: true,
            positional_parameters: true,
        }
    }

    fn execute(&self, request: QueryRequest) -> PoolFuture<'_, QueryExecution> {
        Box::pin(async move { Ok(self.start(request)) })
    }

    /// Reads `system.query_log` across replicas when connecting found a usable
    /// cluster, else on the server that answers.
    fn execution_metrics(&self, id: QueryId) -> PoolFuture<'_, Option<ServerMetrics>> {
        Box::pin(self.logged_metrics(id))
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
            let query = self
                .client
                .query("SELECT name FROM system.databases ORDER BY name");
            Ok(names(query)
                .await?
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
            let query = self
                .client
                .query("SELECT name FROM system.tables WHERE database = ? ORDER BY name")
                .bind(&schema.name);
            Ok(names(query)
                .await?
                .into_iter()
                .map(|name| TableRef {
                    schema: schema.clone(),
                    name,
                })
                .collect())
        })
    }

    fn describe_table<'a>(&'a self, table: &'a TableRef) -> PoolFuture<'a, TableMetadata> {
        Box::pin(async move {
            let query = self
                .client
                .query(
                    "SELECT name, type FROM system.columns \
                     WHERE database = ? AND table = ? ORDER BY position",
                )
                .bind(&table.schema.name)
                .bind(&table.name);
            let rows = text_rows(query).await.map_err(driver)?;
            if rows.is_empty() {
                return Err(Error::DriverExecute(anyhow::anyhow!(
                    "table {}.{} not found",
                    table.schema.name,
                    table.name
                )));
            }
            Ok(TableMetadata {
                table: table.clone(),
                columns: rows
                    .into_iter()
                    .map(|row| {
                        let mut cells = row.into_iter().map(Option::unwrap_or_default);
                        let name = cells.next().unwrap_or_default();
                        let native_type = cells.next().unwrap_or_default();
                        ColumnMetadata {
                            nullable: Some(native_type.starts_with("Nullable(")),
                            name,
                            native_type,
                        }
                    })
                    .collect(),
            })
        })
    }

    /// The primary key, then data-skipping indexes.
    fn list_indexes<'a>(&'a self, table: &'a TableRef) -> PoolFuture<'a, Vec<IndexMetadata>> {
        Box::pin(async move {
            self.describe_table(table).await?;
            let key = |definition: String| IndexKey {
                source: IndexKeySource::Expression(definition.clone()),
                definition,
                ordering: None,
            };
            let primary = self
                .client
                .query(
                    "SELECT primary_key FROM system.tables \
                     WHERE database = ? AND name = ? AND primary_key != ''",
                )
                .bind(&table.schema.name)
                .bind(&table.name);
            let mut indexes: Vec<IndexMetadata> = names(primary)
                .await?
                .into_iter()
                .map(|expression| IndexMetadata {
                    name: "PRIMARY".into(),
                    access_method: "primary".into(),
                    unique: false,
                    primary: true,
                    valid: None,
                    keys: expression.split(", ").map(|k| key(k.to_owned())).collect(),
                    included_columns: Vec::new(),
                    predicate: None,
                })
                .collect();
            let skipping = self
                .client
                .query(
                    "SELECT name, type_full, expr FROM system.data_skipping_indices \
                     WHERE database = ? AND table = ? ORDER BY name",
                )
                .bind(&table.schema.name)
                .bind(&table.name);
            for row in text_rows(skipping).await.map_err(driver)? {
                let mut cells = row.into_iter().map(Option::unwrap_or_default);
                let (name, method, expression) = (
                    cells.next().unwrap_or_default(),
                    cells.next().unwrap_or_default(),
                    cells.next().unwrap_or_default(),
                );
                indexes.push(IndexMetadata {
                    name,
                    access_method: method,
                    unique: false,
                    primary: false,
                    valid: None,
                    keys: vec![key(expression)],
                    included_columns: Vec::new(),
                    predicate: None,
                });
            }
            Ok(indexes)
        })
    }
}
