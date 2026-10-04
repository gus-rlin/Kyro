# Reproducible local synthetic recipe. No DPAPI access or Nebius inference.
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
$repo=Split-Path -Parent $PSScriptRoot
$testNames=@('kyro-chat-test-db','kyro-chat-test-api','kyro-chat-test-provider','kyro-chat-test-worker')
$testNetwork='kyro-chat-test-network'
$rustImage='rust:1.96.1-bookworm'
$sourceMount="type=bind,source=$repo,target=/workspace"
$targetMount='type=volume,source=kyro-chat-target,target=/workspace/target'
$cargoMount='type=volume,source=kyro-part1-cargo-cache,target=/usr/local/cargo'
function Invoke-ChatDocker([string[]]$Arguments) {
    & docker @Arguments
    if($LASTEXITCODE -ne 0) {throw 'chat_test_docker_failed'}
}
foreach($name in $testNames) {
    & docker container inspect $name --format '{{.Id}}' 2>$null | Out-Null
    if($LASTEXITCODE -eq 0) {throw "Existing test container $name; stop the previous synthetic recipe first."}
}
& docker network inspect $testNetwork --format '{{.Id}}' 2>$null | Out-Null
if($LASTEXITCODE -eq 0) {throw 'Existing synthetic test network; finish the previous recipe first.'}
$networkCreated=$false
try {
    Invoke-ChatDocker @('network','create','--subnet','10.248.77.0/24','--label','kyro.chat-test=1',$testNetwork)
    $networkCreated=$true
    Invoke-ChatDocker @('run','-d','--name',$testNames[0],'--label','kyro.chat-test=1','--network',$testNetwork,'--tmpfs','/var/lib/postgresql','-e','POSTGRES_DB=kyro_chat_e2e','-e','POSTGRES_USER=kyro_admin','-e','POSTGRES_HOST_AUTH_METHOD=trust','--mount',"type=bind,source=$repo/tests/fixtures/chat-test-init.sql,target=/docker-entrypoint-initdb.d/chat.sql,readonly",'postgres:18.6-alpine@sha256:77f585114c32fbca283dc835b0596f4e52b51b4c6662d7810b2f4084f60a1873')
    $ready=$false
    for($attempt=0;$attempt -lt 60;$attempt++) {
        & docker exec $testNames[0] pg_isready -U kyro_admin -d kyro_chat_e2e >$null 2>$null
        if($LASTEXITCODE -eq 0){$ready=$true;break}
        Start-Sleep -Milliseconds 250
    }
    if(-not $ready){throw 'chat_test_database_not_ready'}
    Invoke-ChatDocker @('run','--rm','--mount',$sourceMount,'--mount',$targetMount,'--mount',$cargoMount,'-w','/workspace',$rustImage,'cargo','build','--workspace','--bins','--locked')
    foreach($database in @('kyro_chat_e2e','kyro_p1_test_chat')) {
        Invoke-ChatDocker @('run','--rm','--network',"container:$($testNames[0])",'--mount',$targetMount,'-e',"KYRO_DATABASE_ADMIN_URL=postgres://kyro_admin@127.0.0.1:5432/$database",$rustImage,'/workspace/target/debug/kyro-migrate')
    }
    Invoke-ChatDocker @('run','--rm','--network',"container:$($testNames[0])",'--mount',$sourceMount,'--mount',$targetMount,'--mount',$cargoMount,'-e','KYRO_TEST_DATABASE_URL=postgres://kyro_api@127.0.0.1:5432/kyro_p1_test_chat','-e','KYRO_TEST_WORKER_DATABASE_URL=postgres://kyro_worker@127.0.0.1:5432/kyro_p1_test_chat','-e','KYRO_TEST_DATABASE_ADMIN_URL=postgres://kyro_admin@127.0.0.1:5432/kyro_p1_test_chat','-w','/workspace',$rustImage,'cargo','test','--workspace','--locked','--','--include-ignored')
    Invoke-ChatDocker @('run','-d','--name',$testNames[1],'--label','kyro.chat-test=1','--network',$testNetwork,'-p','127.0.0.1:58690:58690','-p','127.0.0.1:59690:59690','-p','127.0.0.1:59691:59691','--mount',"$sourceMount,readonly",'--mount',"$targetMount,readonly",'--env-file',"$repo/tests/fixtures/chat-api.env",$rustImage,'/workspace/target/debug/kyro-api')
    Invoke-ChatDocker @('run','-d','--name',$testNames[2],'--label','kyro.chat-test=1','--network',"container:$($testNames[1])",'--mount',"$sourceMount,readonly",'-e','KYRO_E2E_PROVIDER_HOST=0.0.0.0','-e','KYRO_E2E_PROVIDER_PORT=59690','-e','KYRO_E2E_CONTROL_PORT=59691','-e','KYRO_E2E_CONTROL_TOKEN=synthetic-chat-control-token-for-local-tests','node:24.18.0-bookworm-slim','node','/workspace/scripts/synthetic-provider.mjs')
    Invoke-ChatDocker @('run','-d','--name',$testNames[3],'--label','kyro.chat-test=1','--network',"container:$($testNames[1])",'--mount',"$sourceMount,readonly",'--mount',"$targetMount,readonly",'--env-file',"$repo/tests/fixtures/chat-api.env",'-e','KYRO_MODEL_API_KEY=synthetic-e2e-model-key',$rustImage,'/workspace/target/debug/kyro-worker')
    $ready=$false
    for($attempt=0;$attempt -lt 60;$attempt++) {
        try {
            $response=Invoke-WebRequest -Uri 'http://127.0.0.1:59691/__e2e/ready' -Headers @{Authorization='Bearer synthetic-chat-control-token-for-local-tests'} -TimeoutSec 1
            if($response.StatusCode -eq 200){$ready=$true;break}
        } catch {}
        Start-Sleep -Milliseconds 250
    }
    if(-not $ready){throw 'chat_test_provider_not_ready'}
    $previousIntegration=$env:KYRO_CHAT_INTEGRATION
    Push-Location (Join-Path $repo 'apps/desktop')
    try {
        & npm.cmd run build
        if($LASTEXITCODE -ne 0){throw 'chat_frontend_build_failed'}
        $env:KYRO_CHAT_INTEGRATION='1'
        & npx.cmd playwright test tests/chat.spec.mjs tests/chat-context.spec.ts tests/model-picker.spec.mjs tests/profile-menu.spec.mjs tests/preview-devices.spec.mjs
        if($LASTEXITCODE -ne 0){throw 'chat_frontend_tests_failed'}
    } finally {$env:KYRO_CHAT_INTEGRATION=$previousIntegration; Pop-Location}
} finally {
    foreach($name in @($testNames[3],$testNames[2],$testNames[1],$testNames[0])) {
        $label= & docker container inspect $name --format '{{index .Config.Labels "kyro.chat-test"}}' 2>$null
        if($LASTEXITCODE -eq 0 -and $label -eq '1'){ & docker rm -f $name | Out-Null }
    }
    if($networkCreated){ & docker network rm $testNetwork | Out-Null }
}
