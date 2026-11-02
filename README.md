# Quill

A Rust-native, multi-database SQL editor. Connections are built on DataFusion table
providers; workspaces federate several connections into one DataFusion session.

## Testing

Run unit tests and doctests without external database services:

```sh
cargo test --workspace
```

Database integration tests are ignored by default. They create and remove test
fixtures, so run them against the development Compose services, not a production
database. Avoid running multiple copies of the integration suite simultaneously.

### Compose with TLS

Generate local certificates and start the services:

```sh
just docker --tls
```

Wait for the databases to finish starting, then run:

```sh
QUILL_TEST_TLS=1 cargo test -p quill-providers --test metadata -- --ignored
```

The tests verify catalog, schema, table, and column discovery for Postgres, MySQL,
ClickHouse, and Cassandra (the Compose Scylla service), including missing-table errors. They use the local
CA in `docker/tls/certs/ca.pem` for the services configured with TLS. Postgres uses
plaintext in the current Compose setup.

Run every database test, including per-driver execution and a Postgres–MySQL
federated join:

```sh
QUILL_TEST_TLS=1 cargo test --no-fail-fast -p quill-providers -- --ignored
```

A few execution tests stay ignored with an `upstream provider …` reason; they document
gaps tracked in [`docs/gaps.md`](docs/gaps.md) and fail until those are fixed.

### Compose without TLS

Start only the base configuration:

```sh
just docker
```

Wait for the databases to finish starting, then run without `QUILL_TEST_TLS`:

```sh
cargo test -p quill-providers --test metadata -- --ignored
```

### Checks and cleanup

```sh
cargo fmt --all -- --check
cargo check --workspace
```

Stop the development services when finished:

```sh
docker compose -f docker-compose.yaml -f docker-compose.tls.yaml down
```
