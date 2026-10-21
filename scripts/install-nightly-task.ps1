<#
.SYNOPSIS
  Register (or update) the "mtg-scan nightly" Windows scheduled task that runs
  scripts\nightly.ps1 every day.

.DESCRIPTION
  The task runs as the current user while logged on, so no password is
  stored. If the PC is off or asleep at the scheduled time, it runs as soon
  as possible afterwards.

  Remove it with:  Unregister-ScheduledTask -TaskName 'mtg-scan nightly'
  Run it now with: Start-ScheduledTask -TaskName 'mtg-scan nightly'
#>
[CmdletBinding()]
param(
    [string]$At = '02:30'
)

$ErrorActionPreference = 'Stop'
$script = Join-Path $PSScriptRoot 'nightly.ps1'
$root = Split-Path -Parent $PSScriptRoot

$action = New-ScheduledTaskAction -Execute 'powershell.exe' `
    -Argument "-NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$script`"" `
    -WorkingDirectory $root
$trigger = New-ScheduledTaskTrigger -Daily -At $At
$settings = New-ScheduledTaskSettingsSet -StartWhenAvailable -RunOnlyIfNetworkAvailable `
    -ExecutionTimeLimit (New-TimeSpan -Hours 1) -MultipleInstances IgnoreNew
$principal = New-ScheduledTaskPrincipal -UserId "$env:USERDOMAIN\$env:USERNAME" -LogonType Interactive -RunLevel Limited

Register-ScheduledTask -TaskName 'mtg-scan nightly' -Description 'mtg-scan: refresh prices (price history)' `
    -Action $action -Trigger $trigger -Settings $settings -Principal $principal -Force | Out-Null

Get-ScheduledTask -TaskName 'mtg-scan nightly' | Get-ScheduledTaskInfo |
    Select-Object TaskName, NextRunTime, LastRunTime, LastTaskResult
