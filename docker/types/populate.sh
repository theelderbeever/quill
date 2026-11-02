#!/bin/sh
# Run from the repository root after starting the development services.
set -eu
docker compose exec -T pg sh -c 'exec psql -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB"' < docker/types/postgres.sql
docker compose exec -T mysql sh -c 'MYSQL_PWD="$MYSQL_PASSWORD" exec mysql -u "$MYSQL_USER" "$MYSQL_DATABASE"' < docker/types/mysql.sql
docker compose exec -T scylla sh -c '
    if [ -f /etc/quill-tls/ca.pem ]; then
        export SSL_CERTFILE=/etc/quill-tls/ca.pem
        exec cqlsh --ssl scylla
    fi
    exec cqlsh
'  < docker/types/scylla.cql
docker compose exec -T clickhouse sh -c 'exec clickhouse-client --user "$CLICKHOUSE_USER" --password "$CLICKHOUSE_PASSWORD" --database "$CLICKHOUSE_DB" --multiquery' < docker/types/clickhouse.sql
if docker compose exec -T rustfs test -f /etc/quill-tls/ca.pem; then
    uv run docker/types/objects.py
else
    RUSTFS_ENDPOINT=http://localhost:9000 uv run docker/types/objects.py
fi
