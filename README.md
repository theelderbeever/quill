# Quill

A Rust-native, multi-database SQL editor.

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
QUILL_TEST_TLS=1 cargo test -p quill-drivers --test metadata -- --ignored
```

The tests verify catalog, schema, table, column, and index discovery for Postgres,
MySQL, ClickHouse, and Scylla, including missing-table errors. They use the local
CA in `docker/tls/certs/ca.pem` for the services configured with TLS. Postgres uses
plaintext in the current Compose setup.

### Compose without TLS

Start only the base configuration:

```sh
just docker
```

Wait for the databases to finish starting, then run without `QUILL_TEST_TLS`:

```sh
cargo test -p quill-drivers --test metadata -- --ignored
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
