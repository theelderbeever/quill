#!/bin/sh
set -eu

if docker compose version >/dev/null 2>&1; then
    exec docker compose "$@"
fi

exec docker-compose "$@"
