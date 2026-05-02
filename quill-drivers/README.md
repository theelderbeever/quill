# quill-drivers

Vendor connection pools, feature-gated per database. Wraps `datafusion-table-providers` for shared use across direct and federated execution paths.

## Features

- `postgres` — enables the Postgres connection pool via `datafusion-table-providers`.

## Running the postgres integration test

The repo's `docker-compose.yaml` provides a Postgres service for local testing.

```sh
docker compose up -d pg
POSTGRES_URL='postgres://quill:password@localhost:5432/quilt_test?sslmode=disable' \
    cargo test -p quill-drivers --features postgres -- --nocapture
```

The test is skipped (no failure) when `POSTGRES_URL` is unset.
