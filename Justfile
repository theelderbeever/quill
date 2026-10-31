# Start development services; --tls generates certificates and enables TLS.
[arg('tls', long, value='true')]
docker tls='false':
    #!/bin/sh
    set -eu
    if [ '{{ tls }}' = true ]; then
        sh docker/tls/generate.sh
        docker compose -f docker-compose.yaml -f docker-compose.tls.yaml up -d
    else
        docker compose -f docker-compose.yaml up -d
    fi

stop-docker:
    docker compose -f docker-compose.yaml -f docker-compose.tls.yaml down
