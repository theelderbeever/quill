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

## Native result conversion

Result schemas are fixed before batches are emitted. SQLx conversion errors are
propagated rather than coercing a value into a different storage class.

- PostgreSQL supports primitive scalars, JSON/JSONB (exact JSON text), OID,
  internal char, money (exact integer minor units, annotated as such), intervals,
  and one-dimensional primitive, bytea, UUID, numeric and temporal arrays.
  Numeric array elements use annotated exact decimal text because element
  precision/scale is not declared. Scalar numeric uses declared Arrow precision
  and scale where available. Intervals whose microseconds overflow Arrow's
  nanosecond component return an error. Multidimensional arrays, temporal
  infinities, non-finite numerics, timetz, network/geometric/search types,
  ranges/multiranges, XML and custom types remain unsupported; native decoding
  or the unsupported-type error identifies failures. Cast these explicitly in SQL
  when a different representation is appropriate.
- MySQL supports all SQLx native result type names. TIME is a signed Arrow
  Duration(Microsecond), including negative and greater-than-24-hour values.
  DECIMAL uses annotated exact text: SQLx's public column metadata does not
  expose precision/scale. JSON retains its numeric tokens without floating-point
  conversion. BIT is UInt64. GEOMETRY is the native SRID-prefixed WKB binary
  payload, annotated with `mysql_srid_wkb`. SET uses the server's text encoding.
  SQLx treats zero DATE/DATETIME values as null; partial-zero dates fail native
  decoding. Applications requiring those sentinel values must cast them to text
  in SQL.
- Scylla supports every current native scalar type. Decimal and varint use exact
  annotated text, inet/UUID/timeuuid use annotated canonical text, duration uses
  Arrow MonthDayNano, and date/time/timestamp preserve native epoch units.
  Collections, vectors, tuples and user-defined types return an unsupported-type
  error containing their type description. Legacy empty scalar values return a
  decoding error; they are never silently emitted as null.
- SQLite supports its five native storage classes plus declared logical types.
  Mixed runtime storage classes that cannot fit the fixed declared schema fail
  decoding. Expressions with unavailable type metadata can only emit nulls;
  cast expressions explicitly to establish their schema.
- ClickHouse forwards server Arrow output with strings retained as binary.
  Types unsupported by the server Arrow output format fail at the server. No
  universal ClickHouse-native type coverage is implied.
- DataFusion forwards its planned Arrow schema and batches, including nested
  types; support depends on the selected table provider and query plan.

Text or binary representations of semantic types carry `quill.native_type`,
`quill.logical_type` and `quill.encoding` metadata. Consumers should inspect this
metadata before treating those fields as ordinary strings or bytes.

## SQLx configuration

The workspace uses SQLx 0.9 with `sqlx-toml` enabled. The crate-local
`quill-drivers/sqlx.toml` fixes query-macro type preferences to Chrono and
BigDecimal, matching runtime decoding even if other dependencies enable competing
SQLx type features. SQLx reads this file for this crate only; it does not replace
Quill's runtime connection settings. Current execution uses runtime queries, so
these preferences apply when adding compile-time checked queries.

Use `sqlx-cli` 0.9.0 for SQLx development commands, and run them from
`quill-drivers` so the CLI reads the crate's configuration. See the
[SQLx configuration reference](https://github.com/transact-rs/sqlx/blob/v0.9.0/sqlx-core/src/config/reference.toml)
for supported options.
