# Start development services; --tls generates certificates and enables TLS.
# Seeds Postgres with fake data when faker-cli (`fake`) is installed.
[arg('tls', long, value='true')]
docker tls='false':
    #!/bin/sh
    set -eu
    if [ ! -f docker/postgres/data/people.csv ] && command -v fake >/dev/null; then
        sh docker/postgres/generate.sh
    fi
    if [ '{{ tls }}' = true ]; then
        sh docker/tls/generate.sh
        sh docker/compose.sh -f docker-compose.yaml -f docker-compose.tls.yaml up -d
    else
        sh docker/compose.sh -f docker-compose.yaml up -d
    fi

stop-docker:
    sh docker/compose.sh -f docker-compose.yaml -f docker-compose.tls.yaml down

# Regenerate the fake Postgres seed data; applied when the pg container is recreated.
seed rows='10000':
    sh docker/postgres/generate.sh {{ rows }}
