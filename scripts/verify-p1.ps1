[CmdletBinding()]
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$RunnerArguments
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = Split-Path -Parent $PSScriptRoot
$runner = Join-Path $PSScriptRoot 'verify-p1.mjs'
if (-not (Test-Path -LiteralPath $runner -PathType Leaf)) {
    throw "E2E runner not found: $runner"
}

Push-Location $repoRoot
try {
    & node $runner @RunnerArguments
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
}
finally {
    Pop-Location
}
