#!/usr/bin/env bash
set -euo pipefail
# Synthetic TLS/SCRAM provider. No host port and no installation credentials.
name=${1:?provider name required}
network=${2:?test network required}
[[ $name =~ ^kyro-p2-[a-z0-9-]+$ && $network =~ ^kyro-p2-[a-z0-9-]+$ ]] || exit 2
image='postgres:18.6-alpine@sha256:77f585114c32fbca283dc835b0596f4e52b51b4c6662d7810b2f4084f60a1873'
certificates=$(mktemp -d /tmp/kyro-p2-external-tls.XXXXXXXX)
chmod 755 "$certificates"
openssl req -x509 -newkey rsa:2048 -nodes -keyout "$certificates/ca.key" -out "$certificates/ca.crt" -days 2 -subj /CN=Kyro-Synthetic-External-CA -addext basicConstraints=critical,CA:TRUE -addext keyUsage=critical,keyCertSign,cRLSign 2>/dev/null
openssl req -newkey rsa:2048 -nodes -keyout "$certificates/server.key" -out "$certificates/server.csr" -subj /CN=external.test 2>/dev/null
printf '%s\n' 'subjectAltName=DNS:external.test' 'basicConstraints=critical,CA:FALSE' 'keyUsage=critical,digitalSignature,keyEncipherment' 'extendedKeyUsage=serverAuth' >"$certificates/extensions"
openssl x509 -req -in "$certificates/server.csr" -CA "$certificates/ca.crt" -CAkey "$certificates/ca.key" -CAcreateserial -out "$certificates/server.crt" -days 2 -extfile "$certificates/extensions" 2>/dev/null
chmod 600 "$certificates/ca.key" "$certificates/server.key"
chmod 644 "$certificates/ca.crt" "$certificates/server.crt"
docker run --rm --network none --entrypoint sh --mount "type=bind,source=$certificates,target=/tls" "$image" -c 'chown 70:70 /tls/server.key'
docker network inspect "$network" >/dev/null 2>&1 || docker network create --internal "$network" >/dev/null
docker run --detach --name "$name" --network "$network" --mount "type=bind,source=$certificates,target=/tls,readonly" --env POSTGRES_USER=kyro_external_admin --env POSTGRES_DB=kyro_external_synthetic --env POSTGRES_PASSWORD=public-synthetic-external-admin-password-32 --env POSTGRES_HOST_AUTH_METHOD=scram-sha-256 "$image" postgres -c ssl=on -c ssl_cert_file=/tls/server.crt -c ssl_key_file=/tls/server.key >/dev/null
for attempt in {1..30}; do
  if docker exec "$name" pg_isready -U kyro_external_admin -d kyro_external_synthetic >/dev/null 2>&1; then
    address=$(docker inspect "$name" --format '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}')
    if [[ -n ${GITHUB_ENV:-} ]]; then
      printf '%s\n' "KYRO_P2_TEST_EXTERNAL_ADMIN_URL=postgres://kyro_external_admin:public-synthetic-external-admin-password-32@$address/kyro_external_synthetic" "KYRO_P2_TEST_EXTERNAL_CA_FILE=$certificates/ca.crt" >>"$GITHUB_ENV"
    fi
    printf 'synthetic provider %s ready; public CA: %s/ca.crt; address: %s\n' "$name" "$certificates" "$address"
    exit 0
  fi
  sleep 0.5
done
echo 'synthetic provider did not become ready' >&2
exit 1
