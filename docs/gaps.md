# Upstream gaps

Behavior lost moving from quill's native drivers to
[datafusion-table-providers](https://github.com/datafusion-contrib/datafusion-table-providers)
(`dtp`) and [datafusion-federation](https://github.com/datafusion-contrib/datafusion-federation).
Resolve these in a fork to reach full parity.

## Cross-provider (`dtp` common)

- **Schema inferred from first rows.** `query_arrow(.., None)` derives the schema from the first
  chunk: empty results carry no columns, and a later chunk may disagree with the first. Need a
  schema from result metadata established before any batch is emitted.
- **No bind parameters for ad-hoc SQL.** Pool params are typed per provider (`&'static dyn ToSql`
  etc.), so there is no generic path from `quill_core::query::ParameterValue`.
- **No server metrics.** No hook to correlate an execution with server-side logs (ClickHouse
  `query_log`).
- **One result set per statement.** Multi-statement input yields at most one result.
- **Arrow-only column metadata.** `get_schema` returns Arrow types; native type spellings are
  lost unless the provider sets `SOURCE_TYPE_METADATA_KEY`. No index discovery API.
- **`SQLExecutor` is per table.** Executing arbitrary native SQL needs an executor that does not
  require a projected schema up front.

## Postgres (`dtp-postgres`)

- `numeric` beyond Decimal128 precision (e.g. `numeric(60,8)`, `numeric(77,0)`) fails with
  `ExceedsMaximumPossibleValue` instead of mapping to Decimal256 or exact `Utf8` with
  `quill.*` field metadata. Unconstrained `numeric` is likewise not annotated.
- Arrays containing NULL elements fail (`FromSql WasNull` on `Int4Array`).
- `void` result columns (e.g. `SELECT pg_sleep(1)`) are unsupported.
- No client certificate (mTLS) parameters; only `sslrootcert`.
- No index metadata (quill previously read `pg_index`/`pg_am`).

## MySQL (`dtp-mysql`)

- No client certificate (mTLS) parameters; only `sslrootcert`.
- `sslmode` accepts only `disabled | required | preferred`; `preferred` disables certificate and
  hostname verification, so quill maps TLS to `required`.
- No index metadata (quill previously read `information_schema.statistics`).

## ClickHouse (`dtp-clickhouse`)

- `String` is decoded as Arrow `Utf8`; values with non-UTF-8 bytes fail the query. quill
  previously mapped `String` to `Binary`.
- Provider params cannot configure a custom CA or client identity; quill builds the
  `clickhouse::Client` itself and constructs `ClickHouseConnectionPool` from its public fields.
- No server metrics: quill previously correlated executions with `system.query_log`
  (read/written rows, peak memory, permission-denied detection).
- No data-skipping index metadata (`system.data_skipping_indices`).

## Cassandra (`dtp-cassandra`)

- Provider params cannot express TLS; quill builds the `scylla::Session` itself and wraps it with
  `CassandraConnectionPool::from_session`.
- No per-request driver history (attempts, retries, nodes) in client metrics.
- No secondary index metadata (`system_schema.indexes`).
- Config `kind = "scylla"` is renamed to `kind = "cassandra"`.

## SQLite (`dtp-sqlite`)

- All-NULL columns are typed `Null` rather than the declared type (`CAST(NULL AS INTEGER)`).
- Only the `main` schema is listed; attached databases are not discovered (quill previously read
  `pragma_database_list`). Configured attachments (`with_databases`) are not yet exposed.
- No index metadata (quill previously read `pragma_index_list`/`pragma_index_xinfo`).

## Federation (`datafusion-federation`, `dtp` factories)

- Member tables are listed once when a workspace connects; tables created afterwards need a
  reconnect. Listing walks every schema up front, which is slow on large catalogs. A lazy
  `SchemaProvider` needs an async `table_names`, which DataFusion does not offer.
- Table factories only build tables, not views or other relations the catalog lists.
- No server metrics for the pushed-down parts of a federated query; plan metrics cover the
  DataFusion side only.
