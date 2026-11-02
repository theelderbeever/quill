# Provider parity gaps

Behavior lost moving from quill's native drivers to
[datafusion-table-providers](https://github.com/datafusion-contrib/datafusion-table-providers)
(`dtp`) and [datafusion-federation](https://github.com/datafusion-contrib/datafusion-federation).
The list below records the original migration gaps. Current integration status follows.

## Fork integration status

The workspace patches `datafusion-table-providers` to
[`theelderbeever/feat/quill-features`](https://github.com/theelderbeever/datafusion-table-providers/tree/feat/quill-features);
`Cargo.lock` pins the revision. Quill uses its native executor, provider-neutral
bind parameters, query IDs, and index discovery. Postgres and MySQL TLS identities
map to `sslcert`/`sslkey`. SQLite `databases` config maps to configured attachments,
and `kind = "scylla"` remains an alias for `cassandra`. Arrow field metadata passes
through query events without conversion.

Still outstanding:

- Multi-statement/multiple-result execution: the fork executor returns one Arrow stream.
  The editor submits only the semicolon-delimited statement at the cursor; selections
  do not enable multi-statement execution.
- Server metrics lookup and ClickHouse permission-denied classification. Query IDs
  now reach native execution, but metrics retrieval is not wired.
- Cassandra driver history: the fork accepts a history listener extension; Quill
  does not yet collect it into client snapshots.
- Full index fidelity: the fork reports names, keys, kinds, uniqueness and primary
  status, but omits validity, ordering, included columns and predicates. Quill
  reports unknown validity and retains keys as native expressions.
- ClickHouse custom TLS and Cassandra TLS still use Quill-built clients/sessions.
- MySQL TLS still uses `required` to preserve certificate/hostname verification.
- Federation catalog refresh/lazy discovery and server metrics remain unresolved.
- Provider conversion fixes are supplied by the fork; full parity still requires
  live database verification of the cases below.

## Original migration gaps

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
