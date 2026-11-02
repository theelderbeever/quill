# quill

Binary entrypoint. Loads config and connection files, then hands them to the TUI.

## Connection files

Each `[connection.<name>]` table selects a driver with `kind`. A `[workspace.<name>]`
table opens several connections from the same file as one federated session:

```toml
[connection.pg]
kind = "postgres"
host = "localhost"
database = "app"
user = "quill"

[connection.events]
kind = "clickhouse"
host = "localhost:8123"
database = "default"

[workspace.analytics]
# A name, or a table whose `alias` renames the member's catalog.
connections = ["pg", { conn = "events", alias = "ch" }]
# Optional, relative to this file; defaults like a connection's.
queries = "queries/analytics"
```

Opening a connection runs SQL in that database's own dialect. Opening a workspace runs
SQL through DataFusion, with every member addressable as a catalog:
`SELECT … FROM "pg"."public"."users" JOIN "ch"."default"."clicks" …`.

Query results are held in memory for the connection's session and released when it
disconnects.
