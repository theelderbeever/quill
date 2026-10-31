#!/bin/sh
# Throwaway certificates for local integration tests only.
set -eu
cert_dir=${CERT_DIR:-"$(dirname "$0")/certs"}
umask 077
mkdir -p "$cert_dir"
cd "$cert_dir"

# Reuse valid certificates so restarting Compose does not change client trust.
if [ "${FORCE_REGENERATE:-0}" != 1 ] &&
    [ -s development_key.pem ] &&
    openssl x509 -in ca.pem -checkend 86400 -noout >/dev/null 2>&1 &&
    openssl x509 -in development_cert.pem -checkend 86400 -noout >/dev/null 2>&1 &&
    openssl verify -CAfile ca.pem -verify_hostname localhost development_cert.pem >/dev/null 2>&1; then
    chmod 755 .
    chmod 644 ./*.pem
    exit 0
fi
work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT HUP INT TERM
openssl req -x509 -newkey rsa:2048 -nodes -sha256 -days 365 \
    -subj '/CN=Quill local test CA' \
    -addext 'basicConstraints=critical,CA:TRUE' \
    -addext 'keyUsage=critical,keyCertSign,cRLSign' \
    -keyout "$work_dir/ca-key.pem" -out ca.pem
    openssl req -new -newkey rsa:2048 -nodes \
        -subj '/CN=localhost' \
        -keyout development_key.pem -out "$work_dir/server.csr"
    cat > "$work_dir/extensions" <<EXT
basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature,keyEncipherment
extendedKeyUsage=serverAuth
subjectAltName=DNS:localhost,DNS:scylla,DNS:mysql,DNS:clickhouse,DNS:rustfs,IP:127.0.0.1,IP:::1
EXT
    openssl x509 -req -sha256 -days 365 -in "$work_dir/server.csr" \
        -CA ca.pem -CAkey "$work_dir/ca-key.pem" \
        -set_serial "0x$(openssl rand -hex 16)" \
        -extfile "$work_dir/extensions" -out development_cert.pem
# Containers use different UIDs. These public, throwaway keys must be readable
# through the read-only bind mount; never use this setup for real credentials.
chmod 755 .
chmod 644 *.pem
