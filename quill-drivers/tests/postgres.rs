use std::collections::BTreeMap;

use quill_core::{ConnectionSpec, PostgresSpec};
use quill_drivers::{ConnectionPool, connect};
use secrecy::SecretString;

#[tokio::test]
async fn connect_to_real_postgres() {
    let Ok(raw) = std::env::var("POSTGRES_URL") else {
        eprintln!("POSTGRES_URL not set; skipping");
        return;
    };
    let url = url::Url::parse(&raw).expect("valid POSTGRES_URL");

    let mut params = BTreeMap::new();
    for (k, v) in url.query_pairs() {
        params.insert(k.into_owned(), v.into_owned());
    }

    let spec = ConnectionSpec::Postgres(PostgresSpec {
        host: url.host_str().unwrap_or("localhost").to_string(),
        port: url.port().unwrap_or(5432),
        database: url.path().trim_start_matches('/').to_string(),
        user: url.username().to_string(),
        password: url.password().map(|p| SecretString::from(p.to_string())),
        params,
    });

    let pool = connect("pg_test", &spec).await.expect("pool connects");
    let ConnectionPool::Postgres(_) = pool;
}
