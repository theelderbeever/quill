Local TLS testing
=================

Requires Just and OpenSSL with `req -addext` support.

```sh
just docker --tls
```

The recipe generates certificates before starting Compose with the TLS override.
Existing certificates are reused while valid for at least another day.

Configure clients to trust `docker/tls/certs/ca.pem` and enable hostname
verification. One shared server certificate covers localhost, 127.0.0.1, ::1, and all four
Compose service names. The shared files use RustFS’s required names,
`rustfs_cert.pem` and `rustfs_key.pem`. No client certificate is required.

| Service | TLS endpoint |
| --- | --- |
| Scylla CQL | localhost:9042 |
| MySQL | localhost:3306 (MySQL TLS negotiation) |
| ClickHouse HTTPS | https://localhost:8443 |
| ClickHouse native TLS | localhost:9440 |
| RustFS S3 | https://localhost:9000 |
| RustFS console | https://localhost:9001 |

The override enables TLS for the four added services; PostgreSQL keeps its base
configuration. ClickHouse's base plaintext ports remain published by Compose but
have no listener with the TLS override. MySQL rejects insecure TCP connections.

Example verification:

```sh
curl --cacert docker/tls/certs/ca.pem --user quill:password https://localhost:8443/ping
curl --cacert docker/tls/certs/ca.pem https://localhost:9000/health
mysql --host=127.0.0.1 --user=quill --password \
  --ssl-mode=VERIFY_IDENTITY --ssl-ca=docker/tls/certs/ca.pem quilt_test
```

To return to plaintext, recreate the services with the base configuration:

```sh
sh docker/compose.sh -f docker-compose.yaml up -d --force-recreate
```

Generated certificates expire after 365 days. The generator replaces expired or
missing certificates on the next `just docker`. Reload the client CA after
regeneration. To force rotation, stop the services, run the generator, and
recreate the services:

```sh
sh docker/compose.sh -f docker-compose.yaml -f docker-compose.tls.yaml down
FORCE_REGENERATE=1 sh docker/tls/generate.sh
sh docker/compose.sh -f docker-compose.yaml -f docker-compose.tls.yaml up -d
```

Certificates are written to `docker/tls/certs` so host clients can read the CA.
They are ignored development fixtures readable by container UIDs. Use these only for local tests. The temporary CA private key
is deleted after generation. Manual generation is also available with
`sh docker/tls/generate.sh` (requires OpenSSL with `req -addext` support).
