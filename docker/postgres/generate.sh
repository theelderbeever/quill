#!/bin/sh
# Generate fake rows for the Postgres `people` table with faker-cli (`fake`).
# Usage: sh docker/postgres/generate.sh [rows]
set -eu

rows=${1:-10000}
out="$(dirname "$0")/data"
mkdir -p "$out"
fake -n "$rows" -f csv \
    -c id,name,email,city,country,signed_up,last_seen,logins \
    uuid4,name,email,city,country_code,date_this_decade,date_time_this_year,random_int \
    > "$out/people.csv"
echo "wrote $rows rows to $out/people.csv"
