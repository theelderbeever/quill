#!/bin/sh
# Runs once, when the Postgres container initialises an empty data directory.
# Loads the CSV from docker/postgres/generate.sh if it was generated.
set -eu

csv=/seed/people.csv
if [ ! -f "$csv" ]; then
    echo "seed: $csv not found; run docker/postgres/generate.sh to create it"
    exit 0
fi

psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" <<SQL
CREATE TABLE people (
    id uuid PRIMARY KEY,
    name text NOT NULL,
    email text NOT NULL,
    city text NOT NULL,
    country char(2) NOT NULL,
    signed_up date NOT NULL,
    last_seen timestamp NOT NULL,
    logins integer NOT NULL
);
\copy people FROM '$csv' WITH (FORMAT csv, HEADER true)
SQL
