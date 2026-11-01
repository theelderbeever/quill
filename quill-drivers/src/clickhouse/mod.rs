mod metadata;
use metadata::{ColumnRow, IndexRow, NameRow, SchemaRow, TableRow};

mod query_log;
use query_log::QueryLog;

mod settings;
pub use settings::ClickhouseSettings;

use crate::{ConnectionPool, PoolFuture};
use quill_core::{
    error::{Error, Result},
    metadata::*,
};
use secrecy::ExposeSecret;

/// Official ClickHouse client backed by a reusable HTTP connection pool.
pub struct ClickhouseConnectionPool {
    client: clickhouse::Client,
    /// Chosen when connecting, from whether replica-wide reads work.
    query_log: QueryLog,
}

impl ClickhouseConnectionPool {
    /// Access the official client for driver-specific operations.
    pub fn inner(&self) -> &clickhouse::Client {
        &self.client
    }

    /// Stream query results as Arrow batches without collecting the result set.
    ///
    /// The query must omit a FORMAT clause; the Arrow extension supplies it.
    /// ClickHouse strings are returned as Arrow binary values to preserve arbitrary bytes.
    ///
    /// ```no_run
    /// use quill_drivers::clickhouse::ClickhouseConnectionPool;
    ///
    /// async fn first_batch(pool: &ClickhouseConnectionPool) -> std::result::Result<(), Box<dyn std::error::Error>> {
    ///     let mut cursor = pool.query_arrow("SELECT 1")?;
    ///     let batch: Option<arrow::record_batch::RecordBatch> = cursor.next().await?;
    ///     Ok(())
    /// }
    /// ```
    pub fn query_arrow(&self, sql: &str) -> Result<clickhouse_ext_arrow::ArrowCursor> {
        use clickhouse_ext_arrow::ArrowQueryExt;
        self.client
            .query(sql)
            // ClickHouse String does not guarantee UTF-8.
            .with_setting("output_format_arrow_string_as_string", "0")
            .fetch_arrow()
            .map_err(|error| Error::DriverExecute(error.into()))
    }
}

impl ConnectionPool for ClickhouseConnectionPool {
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
        let client = self.client.clone();
        Box::pin(async move {
            let id = quill_core::query::QueryId::new();
            Ok(crate::execution::start_with_id(
                id,
                move |sender, control| async move {
                    use clickhouse_ext_arrow::ArrowQueryExt;
                    use quill_core::query::QueryEvent;
                    let mut query = client
                        .query(&request.sql)
                        .with_setting("query_id", Self::server_query_id(id))
                        // Choose a byte-preserving schema before reading any values.
                        .with_setting("output_format_arrow_string_as_string", "0");
                    for value in request.bindings.positional {
                        query = Self::bind_parameter(query, None, value);
                    }
                    for (name, value) in request.bindings.named {
                        query = Self::bind_parameter(query, Some(&name), value);
                    }
                    let mut cursor = query
                        .fetch_arrow()
                        .map_err(|e| Error::QueryExecution(Box::new(e)))?;
                    let mut schema_sent = false;
                    loop {
                        let batch = cursor
                            .next()
                            .await
                            .map_err(|e| Error::QueryExecution(Box::new(e)))?;
                        if !schema_sent && let Some(schema) = cursor.schema() {
                            control
                                .send(
                                    &sender,
                                    QueryEvent::Schema {
                                        result_set: 0,
                                        schema,
                                    },
                                )
                                .await?;
                            schema_sent = true;
                        }
                        let Some(batch) = batch else {
                            break;
                        };
                        control
                            .send(
                                &sender,
                                QueryEvent::Batch {
                                    result_set: 0,
                                    batch,
                                },
                            )
                            .await?;
                    }
                    control
                        .send(
                            &sender,
                            QueryEvent::ResultSetComplete {
                                result_set: 0,
                                affected_rows: None,
                            },
                        )
                        .await?;
                    Ok(())
                },
            ))
        })
    }
    /// Reads `system.query_log` across replicas when connecting found a usable
    /// `{cluster}` macro, otherwise on the connected server. Returns `Ok(None)` until
    /// the server flushes its log, by default within a few seconds.
    fn execution_metrics(
        &self,
        id: quill_core::query::QueryId,
    ) -> PoolFuture<'_, Option<quill_core::query::ServerMetrics>> {
        Box::pin(self.logged_metrics(id))
    }

    type Settings = ClickhouseSettings;

    fn list_catalogs(&self) -> PoolFuture<'_, Vec<CatalogRef>> {
        Box::pin(async {
            Ok(vec![CatalogRef {
                name: "clickhouse".into(),
            }])
        })
    }
    fn list_schemas<'a>(&'a self, catalog: &'a CatalogRef) -> PoolFuture<'a, Vec<SchemaRef>> {
        Box::pin(async move {
            if catalog.name != "clickhouse" {
                return Ok(Vec::new());
            }
            let rows = self
                .client
                .query("SELECT name FROM system.databases ORDER BY name")
                .fetch_all::<NameRow>()
                .await
                .map_err(|e| Error::DriverExecute(e.into()))?;
            Ok(rows
                .into_iter()
                .map(|row| SchemaRef::from(SchemaRow { row, catalog }))
                .collect())
        })
    }
    fn list_tables<'a>(&'a self, schema: &'a SchemaRef) -> PoolFuture<'a, Vec<TableRef>> {
        Box::pin(async move {
            if schema.catalog.name != "clickhouse" {
                return Ok(Vec::new());
            }
            let rows = self
                .client
                .query("SELECT name FROM system.tables WHERE database = ? ORDER BY name")
                .bind(&schema.name)
                .fetch_all::<NameRow>()
                .await
                .map_err(|e| Error::DriverExecute(e.into()))?;
            Ok(rows
                .into_iter()
                .map(|row| TableRef::from(TableRow { row, schema }))
                .collect())
        })
    }
    fn describe_table<'a>(&'a self, table: &'a TableRef) -> PoolFuture<'a, TableMetadata> {
        Box::pin(async move {
            if table.schema.catalog.name != "clickhouse" {
                return Err(Error::DriverExecute(anyhow::anyhow!("catalog not found")));
            }
            let exists = self
                .client
                .query("SELECT count() FROM system.tables WHERE database = ? AND name = ?")
                .bind(&table.schema.name)
                .bind(&table.name)
                .fetch_one::<u64>()
                .await
                .map_err(|e| Error::DriverExecute(e.into()))?;
            if exists == 0 {
                return Err(Error::DriverExecute(anyhow::anyhow!("table not found")));
            }
            let rows = self.client.query("SELECT name, type AS native_type FROM system.columns WHERE database = ? AND table = ? ORDER BY position").bind(&table.schema.name).bind(&table.name).fetch_all::<ColumnRow>().await.map_err(|e| Error::DriverExecute(e.into()))?;
            Ok(TableMetadata {
                table: table.clone(),
                columns: rows.into_iter().map(ColumnMetadata::from).collect(),
            })
        })
    }
    fn list_indexes<'a>(&'a self, table: &'a TableRef) -> PoolFuture<'a, Vec<IndexMetadata>> {
        Box::pin(async move {
            self.describe_table(table).await?;
            let rows = self.client.query("SELECT name, type_full AS access_method, expr AS expression FROM system.data_skipping_indices WHERE database = ? AND table = ? ORDER BY name").bind(&table.schema.name).bind(&table.name).fetch_all::<IndexRow>().await.map_err(|e| Error::DriverExecute(e.into()))?;
            Ok(rows.into_iter().map(IndexMetadata::from).collect())
        })
    }

    async fn connect(settings: &Self::Settings) -> Result<Self> {
        let connect = async {
            anyhow::ensure!(
                settings.user.is_some() || settings.password.is_none(),
                "ClickHouse password requires a user"
            );
            let scheme = if settings.tls.is_some() {
                "https"
            } else {
                "http"
            };
            let endpoint = url::Url::parse(&format!("{scheme}://{}", settings.host))?;
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
            if let Some(tls) = &settings.tls {
                let mut builder = native_tls::TlsConnector::builder();
                if let Some(ca) = &tls.ca_cert {
                    let pem = tokio::fs::read(ca).await?;
                    for cert in openssl::x509::X509::stack_from_pem(&pem)? {
                        builder.add_root_certificate(native_tls::Certificate::from_der(
                            &cert.to_der()?,
                        )?);
                    }
                }
                if let Some(identity) = &tls.identity {
                    let cert = tokio::fs::read(&identity.cert).await?;
                    let key = secrecy::SecretString::from(
                        tokio::fs::read_to_string(&identity.key).await?,
                    );
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
                let http = hyper_util::client::legacy::Client::builder(
                    hyper_util::rt::TokioExecutor::new(),
                )
                .build(connector);
                client = clickhouse::Client::with_http_client(http);
            }
            client = client
                .with_url(endpoint.as_str())
                .with_database(&settings.database);
            if let Some(user) = &settings.user {
                client = client.with_user(user);
            }
            if let Some(password) = &settings.password {
                client = client.with_password(password.expose_secret());
            }
            for (key, value) in &settings.params {
                client = client.with_setting(key, value);
            }
            let query_log = QueryLog::probe(&client).await?;
            let pool = Self { client, query_log };
            Ok::<_, anyhow::Error>(pool)
        };
        connect.await.map_err(|source| Error::DriverConnect {
            connection: "clickhouse".into(),
            source,
        })
    }
}

impl ClickhouseConnectionPool {
    fn bind_parameter(
        mut query: clickhouse::query::Query,
        name: Option<&str>,
        value: quill_core::query::ParameterValue,
    ) -> clickhouse::query::Query {
        use quill_core::query::ParameterValue;
        macro_rules! bind {
            ($value:expr) => {
                if let Some(name) = name {
                    query.param(name, $value)
                } else {
                    query.bind($value)
                }
            };
        }
        query = match value {
            ParameterValue::Null => bind!(Option::<String>::None),
            ParameterValue::Boolean(v) => bind!(v),
            ParameterValue::Int64(v) => bind!(v),
            ParameterValue::UInt64(v) => bind!(v),
            ParameterValue::Float64(v) => bind!(v),
            ParameterValue::Text(v) => bind!(v),
            ParameterValue::Binary(v) => bind!(v),
            ParameterValue::Date(v) => bind!(v.to_string()),
            ParameterValue::Time(v) => bind!(v.to_string()),
            ParameterValue::Timestamp(v) => bind!(v.to_string()),
            ParameterValue::TimestampWithTimezone(v) => bind!(v.to_rfc3339()),
            ParameterValue::Uuid(v) => bind!(v.to_string()),
        };
        query
    }
}
