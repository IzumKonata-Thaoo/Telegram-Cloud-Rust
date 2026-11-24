# Load .env into this process and run tgvault (default: serve).
param([string]$Command = "serve")
$root = Split-Path -Parent $PSScriptRoot
$envFile = Join-Path $root ".env"
if (-not (Test-Path $envFile)) { throw "No .env found. Run scripts\setup.ps1 first." }
foreach ($line in Get-Content $envFile) {
    if ($line -match '^\s*#' -or $line -notmatch '=') { continue }
    $k, $v = $line -split '=', 2
    [Environment]::SetEnvironmentVariable($k.Trim(), $v.Trim(), "Process")
}
Set-Location $root
& (Join-Path $root "target\release\tgvault.exe") $Command
