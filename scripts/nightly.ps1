<#
.SYNOPSIS
  Nightly maintenance for mtg-scan: refresh every price, which adds today's
  point to the price history.

.DESCRIPTION
  Run from Task Scheduler (see install-nightly-task.ps1) or by hand. Runs in
  the repository root so mtg-scan finds .env and mtg-scan.toml. Writes a log
  per run to logs\ and keeps the newest 30. Exits non-zero if a step fails,
  so Task Scheduler's "Last Run Result" shows the failure.
#>
[CmdletBinding()]
param(
    [int]$KeepLogs = 30
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# Release zip layout (exe next to scripts\) or a source checkout (cargo build).
$exe = @((Join-Path $root 'mtg-scan.exe'), (Join-Path $root 'target\release\mtg-scan.exe')) |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $exe) { $exe = Join-Path $root 'target\release\mtg-scan.exe' }
$logDir = Join-Path $root 'logs'
New-Item -ItemType Directory -Force $logDir | Out-Null
$log = Join-Path $logDir ("nightly-{0:yyyyMMdd-HHmmss}.log" -f (Get-Date))

function Step([string]$name, [string[]]$arguments) {
    "== $name ($(Get-Date -Format s))" | Tee-Object -FilePath $log -Append
    # Merge stderr into the log; mtg-scan prints warnings there.
    & $exe @arguments 2>&1 | ForEach-Object { "$_" } | Tee-Object -FilePath $log -Append
    if ($LASTEXITCODE -ne 0) { throw "$name failed with exit code $LASTEXITCODE" }
}

$failed = $false
try {
    if (-not (Test-Path $exe)) { throw "mtg-scan is not built: run 'cargo build --release' in $root" }
    Step 'reprice' @('reprice')
    "== done ($(Get-Date -Format s))" | Tee-Object -FilePath $log -Append
}
catch {
    $failed = $true
    "!! $($_.Exception.Message)" | Tee-Object -FilePath $log -Append
}
finally {
    Get-ChildItem $logDir -Filter 'nightly-*.log' |
        Sort-Object Name -Descending |
        Select-Object -Skip $KeepLogs |
        Remove-Item -Force
}
if ($failed) { exit 1 }
