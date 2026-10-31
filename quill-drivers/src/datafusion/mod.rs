mod settings;
pub use settings::{
    DatafusionSettings, FileFormat, FileTableSettings, ObjectStoreProvider, ObjectStoreSettings,
};

use datafusion::prelude::{CsvReadOptions, JsonReadOptions, ParquetReadOptions, SessionContext};
use quill_core::error::{Error, Result};

use crate::ConnectionPool;

/// An embedded DataFusion session shared through the connection-pool interface.
/// The session owns its runtime, registered stores, and table catalog.
pub struct DatafusionConnectionPool {
    inner: SessionContext,
}

impl DatafusionConnectionPool {
    /// Access the session to plan SQL or stream Arrow results.
    ///
    /// ```no_run
    /// use quill_drivers::datafusion::DatafusionConnectionPool;
    ///
    /// async fn query(pool: &DatafusionConnectionPool) -> anyhow::Result<()> {
    ///     let frame = pool.inner().sql("SELECT 1").await?;
    ///     let stream = frame.execute_stream().await?;
    ///     Ok(())
    /// }
    /// ```
    pub fn inner(&self) -> &SessionContext {
        &self.inner
    }
}

impl ConnectionPool for DatafusionConnectionPool {
    type Settings = DatafusionSettings;

    async fn connect(settings: &Self::Settings) -> Result<Self> {
        let connect = async {
            let inner = SessionContext::new();
            let mut authorities = std::collections::BTreeSet::new();
            for config in &settings.object_stores {
                let (url, store) = config.build()?;
                let authority = url[..url::Position::BeforePath].to_owned();
                if url.scheme() != "file" {
                    anyhow::ensure!(
                        authorities.insert(authority),
                        "duplicate object store authority"
                    );
                }
                inner.register_object_store(&url, store);
                for table in &config.tables {
                    let path = url.join(&table.path)?;
                    anyhow::ensure!(
                        path.scheme() == url.scheme()
                            && path.host_str() == url.host_str()
                            && path.port() == url.port()
                            && path.username().is_empty()
                            && path.password().is_none()
                            && path.query().is_none()
                            && path.fragment().is_none(),
                        "table path must belong to its parent object store"
                    );
                    anyhow::ensure!(
                        !inner.table_exist(table.name.as_str())?,
                        "duplicate table name"
                    );
                    match &table.format {
                        FileFormat::Parquet => {
                            inner
                                .register_parquet(
                                    table.name.as_str(),
                                    path.as_str(),
                                    ParquetReadOptions::default(),
                                )
                                .await?
                        }
                        FileFormat::Csv { has_header } => {
                            inner
                                .register_csv(
                                    table.name.as_str(),
                                    path.as_str(),
                                    CsvReadOptions::new().has_header(*has_header),
                                )
                                .await?
                        }
                        FileFormat::Json => {
                            inner
                                .register_json(
                                    table.name.as_str(),
                                    path.as_str(),
                                    JsonReadOptions::default(),
                                )
                                .await?
                        }
                    }
                }
            }
            Ok::<_, anyhow::Error>(Self { inner })
        };
        connect.await.map_err(|source| Error::DriverConnect {
            connection: "datafusion".into(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn queries_tables_nested_under_a_local_store() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let csv = root.path().join("csv");
        let json = root.path().join("json");
        let parquet = root.path().join("parquet");
        std::fs::create_dir(&csv)?;
        std::fs::create_dir(&json)?;
        std::fs::write(csv.join("one.csv"), "id\n1\n")?;
        std::fs::write(csv.join("two.csv"), "id\n2\n")?;
        std::fs::write(json.join("one.json"), "{\"id\":3}\n")?;
        let mut settings = DatafusionSettings {
            object_stores: vec![ObjectStoreSettings {
                provider: ObjectStoreProvider::Local {
                    path: root.path().to_owned(),
                },
                options: Default::default(),
                tables: vec![
                    FileTableSettings {
                        name: "csv_data".into(),
                        path: "csv/".into(),
                        format: FileFormat::Csv { has_header: true },
                    },
                    FileTableSettings {
                        name: "json_data".into(),
                        path: "json/".into(),
                        format: FileFormat::Json,
                    },
                ],
            }],
        };
        let pool = DatafusionConnectionPool::connect(&settings).await?;
        pool.inner()
            .sql(&format!(
                "COPY csv_data TO '{}/' STORED AS PARQUET",
                parquet.display()
            ))
            .await?
            .collect()
            .await?;
        settings.object_stores[0].tables.push(FileTableSettings {
            name: "parquet_data".into(),
            path: "parquet/".into(),
            format: FileFormat::Parquet,
        });
        let pool = DatafusionConnectionPool::connect(&settings).await?;
        let batches = pool.inner().sql("SELECT id FROM csv_data UNION ALL SELECT id FROM json_data UNION ALL SELECT id FROM parquet_data").await?.collect().await?;
        assert_eq!(
            batches.iter().map(|batch| batch.num_rows()).sum::<usize>(),
            5
        );
        let _shared: Arc<dyn ConnectionPool> = Arc::new(pool);
        Ok(())
    }
}
