# quill-providers

Connection pools for each supported database, built on
[datafusion-table-providers](https://github.com/datafusion-contrib/datafusion-table-providers),
and federated sessions built on
[datafusion-federation](https://github.com/datafusion-contrib/datafusion-federation).

| `kind`       | Upstream                               | Notes                                      |
| ------------ | -------------------------------------- | ------------------------------------------ |
| `postgres`   | `datafusion-table-providers-postgres`  |                                            |
| `mysql`      | `datafusion-table-providers-mysql`     |                                            |
| `sqlite`     | `datafusion-table-providers-sqlite`    | `path = ":memory:"` opens a private DB     |
| `clickhouse` | `datafusion-table-providers-clickhouse`| quill builds the client (custom CA, mTLS)  |
| `cassandra`  | `datafusion-table-providers-cassandra` | Cassandra and ScyllaDB; quill builds the session (TLS) |
| `datafusion` | embedded `SessionContext`              | object stores and file tables              |

Every upstream kind is a `ProviderPool`, which serves two paths:

- **Single connection.** SQL is sent unchanged to the database in its own dialect;
  DataFusion does not plan it.
- **Federated.** `federation::federate` registers each member connection as a catalog
  named after it, so one query can join `"pg"."public"."users"` with
  `"ch"."default"."events"`. Each connection's share of the query is pushed down to it.

Behavior the upstream providers do not yet match is tracked in
[`docs/gaps.md`](../docs/gaps.md).

## Integration tests

Tests needing a database are `#[ignore]`d and use the repository's Compose services:

```sh
just docker --tls
QUILL_TEST_TLS=1 cargo test -p quill-providers -- --ignored
```

SQLite and federation-over-SQLite tests run with the normal `cargo test`.
