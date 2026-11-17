## Native Providers

- Each kind talks to its database through that database's own client: sqlx for Postgres, MySQL
  and SQLite, `clickhouse` for ClickHouse, `scylla` for Cassandra/ScyllaDB. DataFusion serves only
  local files and object stores.
- SQL is sent unchanged in the database's own dialect. Unbound SQL runs as a script where the
  client supports it, yielding one result set per statement.
- Stream results through `execution::Emitter`; never collect a whole result before delivery.

## Query Execution

- Delegate query and parameter validation to the database. Do not parse SQL or add checks for
  parameter counts, missing or unused bindings, or other conditions the native driver already
  reports descriptively. Adapt bindings to the native API and propagate its errors.
- Register server-side cancellation with `Control::on_server_cancel` where the database supports
  it (`pg_cancel_backend`, `KILL QUERY`).

## Value Handling

- Conversion into `quill_core::value::Value` is total. A cell Quill cannot decode becomes
  `Value::Other` with the server's text, or `Value::Bytes` when only binary is available. Never
  fail a query because of a type.
- Preserve native semantics and precision. Exact numbers stay exact (`Value::Decimal` or
  integers); never round or convert them to floating point.
- Report each column's native type spelling in `Column::native_type`.
- Announce columns for empty results too, describing the statement when the client reports
  columns only alongside rows.
- Test exact decoded values and nulls per type, in every wire format the provider uses.
