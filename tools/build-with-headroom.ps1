# Cargo build wrapper. Sampling cannot guarantee capacity against unrelated processes.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidatePattern('^[a-z0-9-]+$')][string]$Product,
    [Parameter(Mandatory)][string]$Repository,
    [string[]]$CargoArguments = @('build', '--release', '--locked')
)
$ErrorActionPreference = 'Stop'
if ((Get-PSDrive C).Free -le 15GB) { throw 'Build refused: C: must have more than 15 GiB free before starting.' }
$auditBuildRoot = Join-Path 'D:\verne-build' $Product
$auditTempRoot = Join-Path $auditBuildRoot 'tmp'
New-Item -ItemType Directory -Force -Path $auditTempRoot | Out-Null
$auditSaved = @{}
foreach ($auditName in @('CARGO_TARGET_DIR', 'TEMP', 'TMP', 'npm_config_cache')) {
    $auditSaved[$auditName] = [Environment]::GetEnvironmentVariable($auditName, 'Process')
}
try {
    $env:CARGO_TARGET_DIR = Join-Path $auditBuildRoot 'target'
    $env:TEMP = $auditTempRoot
    $env:TMP = $auditTempRoot
    $env:npm_config_cache = Join-Path $auditBuildRoot 'npm-cache'
    # Fixed executable and separately supplied arguments; no shell evaluation.
    $auditStart = New-Object System.Diagnostics.ProcessStartInfo
    $auditStart.FileName = (Get-Command cargo.exe -ErrorAction Stop).Source
    $auditStart.WorkingDirectory = (Resolve-Path -LiteralPath $Repository).Path
    $auditStart.UseShellExecute = $false
    # Windows PowerShell lacks ProcessStartInfo.ArgumentList. Restrict this wrapper
    # to ordinary cargo flags rather than constructing arbitrary command text.
    foreach ($auditArgument in $CargoArguments) {
        if ($auditArgument -notmatch '^[A-Za-z0-9_./:=+-]+$') { throw 'Unsupported argument syntax.' }
    }
    $auditStart.Arguments = $CargoArguments -join ' '
    $auditProcess = [System.Diagnostics.Process]::Start($auditStart)
    while (!$auditProcess.WaitForExit(1000)) {
        if ((Get-PSDrive C).Free -le 12GB) {
            & taskkill.exe /PID $auditProcess.Id /T /F | Out-Null
            throw 'Build stopped at the 12 GiB guard threshold; inspect other C: writers.'
        }
    }
    if ($auditProcess.ExitCode -ne 0) { throw "Cargo exited with code $($auditProcess.ExitCode)." }
} finally {
    foreach ($auditName in $auditSaved.Keys) {
        [Environment]::SetEnvironmentVariable($auditName, $auditSaved[$auditName], 'Process')
    }
}
