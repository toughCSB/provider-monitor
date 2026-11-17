$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw 'Native verification must run on Windows.' }
$pmWindowsRoot = Split-Path -Parent $PSScriptRoot
Push-Location $pmWindowsRoot
try {
    foreach ($pmCheck in Get-ChildItem (Join-Path $PSScriptRoot 'check-*.mjs') | Sort-Object Name) {
        & node $pmCheck.FullName
        if ($LASTEXITCODE -ne 0) { throw "UI check failed: $($pmCheck.Name)" }
    }
    & cargo test --locked -p codenotch
    if ($LASTEXITCODE -ne 0) { throw "Native tests failed: $LASTEXITCODE" }
} finally {
    Pop-Location
}
