use std::collections::BTreeMap;

use secrecy::SecretString;
use serde::Deserialize;
use url::Url;

/// An embedded query session with object stores and named file tables.
///
/// Example application configuration:
/// ```toml
/// [connection.files]
/// kind = "datafusion"
///
/// [[connection.files.object_stores]]
/// kind = "local"
/// path = "./analytics"
///
/// tables = [
///     { name = "events", format = "parquet", path = "events/" }
/// ]
///
/// [[connection.files.object_stores]]
/// kind = "aws"
/// url = "s3://analytics"
/// from_env = true
/// region = "us-east-1"
///
/// tables = [
///     { name = "events", format = "parquet", path = "events/" }
/// ]
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct DatafusionSettings {
    #[serde(default)]
    pub object_stores: Vec<ObjectStoreSettings>,
}

/// A store with named tables. Typed fields override `options`, which override
/// environment configuration when `from_env = true`.
#[derive(Debug, Clone, Deserialize)]
pub struct ObjectStoreSettings {
    #[serde(flatten)]
    pub provider: ObjectStoreProvider,
    #[serde(default)]
    pub options: BTreeMap<String, SecretString>,
    #[serde(default)]
    pub tables: Vec<FileTableSettings>,
}

/// Provider-specific connection parameters. Local paths are filesystem paths,
/// not URLs. Cloud URLs identify a bucket/container without a directory prefix.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ObjectStoreProvider {
    Local {
        path: std::path::PathBuf,
    },
    Aws {
        url: Url,
        #[serde(default)]
        from_env: bool,
        region: Option<String>,
        endpoint: Option<String>,
        access_key_id: Option<SecretString>,
        secret_access_key: Option<SecretString>,
        session_token: Option<SecretString>,
        allow_http: Option<bool>,
    },
    Azure {
        url: Url,
        #[serde(default)]
        from_env: bool,
        account: Option<String>,
        access_key: Option<SecretString>,
        sas_token: Option<SecretString>,
        allow_http: Option<bool>,
    },
    Gcp {
        url: Url,
        #[serde(default)]
        from_env: bool,
        service_account_path: Option<String>,
        service_account_key: Option<SecretString>,
    },
}

impl ObjectStoreSettings {
    pub(super) fn build(
        &self,
    ) -> anyhow::Result<(Url, std::sync::Arc<dyn object_store::ObjectStore>)> {
        use secrecy::ExposeSecret;
        match &self.provider {
            ObjectStoreProvider::Local { path } => {
                anyhow::ensure!(
                    self.options.is_empty(),
                    "local stores do not accept provider options"
                );
                let path = std::fs::canonicalize(path)?;
                anyhow::ensure!(path.is_dir(), "local store path must be a directory");
                let url = Url::from_directory_path(path)
                    .map_err(|_| anyhow::anyhow!("invalid local directory"))?;
                Ok((
                    url,
                    std::sync::Arc::new(object_store::local::LocalFileSystem::new()),
                ))
            }
            ObjectStoreProvider::Aws {
                url,
                from_env,
                region,
                endpoint,
                access_key_id,
                secret_access_key,
                session_token,
                allow_http,
            } => {
                anyhow::ensure!(
                    url.path().trim_matches('/').is_empty()
                        && url.query().is_none()
                        && url.fragment().is_none()
                        && url.username().is_empty()
                        && url.password().is_none(),
                    "cloud store URL must contain only a scheme and authority"
                );
                anyhow::ensure!(
                    matches!(url.scheme(), "s3" | "s3a"),
                    "URL scheme does not match provider"
                );
                let mut builder = if *from_env {
                    object_store::aws::AmazonS3Builder::from_env()
                } else {
                    object_store::aws::AmazonS3Builder::new()
                };
                for (key, value) in &self.options {
                    builder = builder.with_config(key.parse()?, value.expose_secret());
                }
                if let Some(value) = region {
                    builder = builder.with_config("aws_region".parse()?, value.to_string());
                }
                if let Some(value) = endpoint {
                    builder = builder.with_config("aws_endpoint".parse()?, value.to_string());
                }
                if let Some(value) = access_key_id {
                    builder =
                        builder.with_config("aws_access_key_id".parse()?, value.expose_secret());
                }
                if let Some(value) = secret_access_key {
                    builder = builder
                        .with_config("aws_secret_access_key".parse()?, value.expose_secret());
                }
                if let Some(value) = session_token {
                    builder =
                        builder.with_config("aws_session_token".parse()?, value.expose_secret());
                }
                if let Some(value) = allow_http {
                    builder = builder.with_config("aws_allow_http".parse()?, value.to_string());
                }
                let store = builder.with_url(url.as_str()).build()?;
                Ok((url.clone(), std::sync::Arc::new(store)))
            }
            ObjectStoreProvider::Azure {
                url,
                from_env,
                account,
                access_key,
                sas_token,
                allow_http,
            } => {
                anyhow::ensure!(
                    url.path().trim_matches('/').is_empty()
                        && url.query().is_none()
                        && url.fragment().is_none()
                        && url.username().is_empty()
                        && url.password().is_none(),
                    "cloud store URL must contain only a scheme and authority"
                );
                anyhow::ensure!(
                    matches!(url.scheme(), "az" | "azure" | "abfs" | "abfss"),
                    "URL scheme does not match provider"
                );
                let mut builder = if *from_env {
                    object_store::azure::MicrosoftAzureBuilder::from_env()
                } else {
                    object_store::azure::MicrosoftAzureBuilder::new()
                };
                for (key, value) in &self.options {
                    builder = builder.with_config(key.parse()?, value.expose_secret());
                }
                if let Some(value) = account {
                    builder = builder
                        .with_config("azure_storage_account_name".parse()?, value.to_string());
                }
                if let Some(value) = access_key {
                    builder = builder
                        .with_config("azure_storage_account_key".parse()?, value.expose_secret());
                }
                if let Some(value) = sas_token {
                    builder = builder
                        .with_config("azure_storage_sas_key".parse()?, value.expose_secret());
                }
                if let Some(value) = allow_http {
                    builder = builder.with_config("azure_allow_http".parse()?, value.to_string());
                }
                let store = builder.with_url(url.as_str()).build()?;
                Ok((url.clone(), std::sync::Arc::new(store)))
            }
            ObjectStoreProvider::Gcp {
                url,
                from_env,
                service_account_path,
                service_account_key,
            } => {
                anyhow::ensure!(
                    url.path().trim_matches('/').is_empty()
                        && url.query().is_none()
                        && url.fragment().is_none()
                        && url.username().is_empty()
                        && url.password().is_none(),
                    "cloud store URL must contain only a scheme and authority"
                );
                anyhow::ensure!(url.scheme() == "gs", "URL scheme does not match provider");
                let mut builder = if *from_env {
                    object_store::gcp::GoogleCloudStorageBuilder::from_env()
                } else {
                    object_store::gcp::GoogleCloudStorageBuilder::new()
                };
                for (key, value) in &self.options {
                    builder = builder.with_config(key.parse()?, value.expose_secret());
                }
                if let Some(value) = service_account_path {
                    builder = builder
                        .with_config("google_service_account_path".parse()?, value.to_string());
                }
                if let Some(value) = service_account_key {
                    builder = builder
                        .with_config("google_service_account_key".parse()?, value.expose_secret());
                }
                let store = builder.with_url(url.as_str()).build()?;
                Ok((url.clone(), std::sync::Arc::new(store)))
            }
        }
    }
}

/// A named table within its parent object store.
/// Paths are relative to the store root; directory paths end with a slash.
#[derive(Debug, Clone, Deserialize)]
pub struct FileTableSettings {
    pub name: String,
    pub path: String,
    #[serde(flatten)]
    pub format: FileFormat,
}

/// File encoding and its read options. JSON is newline-delimited JSON.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "format", rename_all = "lowercase")]
pub enum FileFormat {
    Parquet,
    Csv { has_header: bool },
    Json,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_provider_fields_and_nested_tables() -> anyhow::Result<()> {
        let config: DatafusionSettings = toml::from_str(
            r#"
            [[object_stores]]
            kind = "aws"
            url = "s3://analytics"
            from_env = true
            region = "us-east-1"
            secret_access_key = "test-secret"
            options = { aws_region = "us-west-2" }
            tables = [{ name = "events", format = "parquet", path = "events/" }]
        "#,
        )?;
        let store = &config.object_stores[0];
        assert!(
            matches!(&store.provider, ObjectStoreProvider::Aws { from_env: true, region: Some(region), .. } if region == "us-east-1")
        );
        assert_eq!(store.tables[0].name, "events");
        assert!(!format!("{store:?}").contains("test-secret"));
        Ok(())
    }

    #[test]
    fn rejects_unknown_provider_options() -> anyhow::Result<()> {
        let store: ObjectStoreSettings = toml::from_str(
            r#"
            kind = "aws"
            url = "s3://analytics"
            options = { typo_region = "us-east-1" }
        "#,
        )?;
        assert!(store.build().is_err());
        Ok(())
    }
}
