#!/bin/sh
set -eu
umask 077
export KYRO_WORKER_DATABASE_URL="$(cat /run/secrets/worker_database_url)"
mkfifo -m 600 /run/kyro-secrets/key.pipe
touch /run/kyro-secrets/waiting
cat /run/kyro-secrets/key.pipe > /run/kyro-secrets/nebius_api_key
rm /run/kyro-secrets/key.pipe /run/kyro-secrets/waiting
chmod 400 /run/kyro-secrets/nebius_api_key
exec /usr/local/bin/kyro-worker
