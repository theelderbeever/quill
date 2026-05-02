use std::collections::HashMap;

use datafusion_table_providers::sql::db_connection_pool::postgrespool::PostgresConnectionPool;
use quill_core::{Error, PostgresSpec, Result};
use secrecy::SecretString;

use crate::pool::ConnectionPool;

pub(crate) async fn connect(name: &str, spec: &PostgresSpec) -> Result<ConnectionPool> {
    let mut params: HashMap<String, SecretString> = HashMap::new();
    params.insert("host".into(), SecretString::from(spec.host.clone()));
    params.insert("port".into(), SecretString::from(spec.port.to_string()));
    params.insert("db".into(), SecretString::from(spec.database.clone()));
    params.insert("user".into(), SecretString::from(spec.user.clone()));
    if let Some(pw) = &spec.password {
        params.insert("pass".into(), pw.clone());
    }
    for (k, v) in &spec.params {
        params.insert(k.clone(), SecretString::from(v.clone()));
    }

    PostgresConnectionPool::new(params)
        .await
        .map(ConnectionPool::Postgres)
        .map_err(|e| Error::DriverConnect {
            connection: name.to_string(),
            source: anyhow::Error::new(e),
        })
}
