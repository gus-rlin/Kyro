param(
    [Parameter(Mandatory=$true)][ValidatePattern('^kyro-p2-[a-z0-9-]+$')][string]$Name,
    [Parameter(Mandatory=$true)][ValidatePattern('^kyro-p2-[a-z0-9-]+$')][string]$Network,
    [string]$ToolsImage = 'p2-p1-baseline-tools:20261005'
)
$ErrorActionPreference = 'Stop'
# Disposable synthetic provider. Never copy operator credentials into it.
$fixtureVolume = "$Name-tls"
docker volume create $fixtureVolume | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'fixture volume creation failed' }
$prepare = @'
set -eu
openssl req -x509 -newkey rsa:2048 -nodes -keyout /fixture/ca.key -out /fixture/ca.crt -days 2 -subj /CN=Kyro-Synthetic-External-CA -addext basicConstraints=critical,CA:TRUE -addext keyUsage=critical,keyCertSign,cRLSign 2>/dev/null
openssl req -newkey rsa:2048 -nodes -keyout /fixture/server.key -out /fixture/server.csr -subj /CN=external.test 2>/dev/null
printf '%s\n' 'subjectAltName=DNS:external.test' 'basicConstraints=critical,CA:FALSE' 'keyUsage=critical,digitalSignature,keyEncipherment' 'extendedKeyUsage=serverAuth' >/fixture/extensions
openssl x509 -req -in /fixture/server.csr -CA /fixture/ca.crt -CAkey /fixture/ca.key -CAcreateserial -out /fixture/server.crt -days 2 -extfile /fixture/extensions 2>/dev/null
chown 70:70 /fixture/server.key
chmod 600 /fixture/server.key /fixture/ca.key
chmod 644 /fixture/server.crt /fixture/ca.crt
'@
docker run --rm --network none --entrypoint /bin/sh --mount "type=volume,source=$fixtureVolume,target=/fixture" $ToolsImage -c $prepare
if ($LASTEXITCODE -ne 0) { throw 'fixture certificate preparation failed' }
docker run --detach --name $Name --network $Network --mount "type=volume,source=$fixtureVolume,target=/tls,readonly" --env POSTGRES_USER=kyro_external_admin --env POSTGRES_DB=kyro_external_synthetic --env POSTGRES_PASSWORD=public-synthetic-external-admin-password-32 --env POSTGRES_HOST_AUTH_METHOD=scram-sha-256 postgres:18.6-alpine@sha256:77f585114c32fbca283dc835b0596f4e52b51b4c6662d7810b2f4084f60a1873 postgres -c ssl=on -c ssl_cert_file=/tls/server.crt -c ssl_key_file=/tls/server.key | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'fixture provider startup failed' }
for ($fixtureAttempt = 0; $fixtureAttempt -lt 30; $fixtureAttempt++) {
    docker exec $Name pg_isready -U kyro_external_admin -d kyro_external_synthetic *> $null
    if ($LASTEXITCODE -eq 0) { Write-Output "$Name ready (synthetic, TLS enabled, no host port)"; exit 0 }
    Start-Sleep -Milliseconds 500
}
throw 'fixture provider did not become ready'
