# Generate local test certificates and start the services with TLS enabled.
docker:
    sh docker/tls/generate.sh
    docker compose -f docker-compose.yaml -f docker-compose.tls.yaml up -d
