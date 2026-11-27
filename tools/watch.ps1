<#
.SYNOPSIS
Rebuilds Mallet whenever its source changes and restarts it on the new build.

.DESCRIPTION
Watches the workspace's crates and Cargo files. After a change it builds the release
build. Only when the build succeeds does it close the running Mallet and start the new
one, so a broken build leaves the old window open. Mallet is closed the normal way, so
it still asks about maps with unsaved changes, and the script waits for those answers.

Mallet runs from a copy in target\watch, which keeps target\release\mallet.exe free for
the next build while the editor is open.

.PARAMETER Map
A map to open each time Mallet starts.

.EXAMPLE
.\tools\watch.ps1 D:\maps\test.vmf
#>
param(
    [string]$Map
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$built = Join-Path $root 'target\release\mallet.exe'
$runDir = Join-Path $root 'target\watch'
$runExe = Join-Path $runDir 'mallet.exe'
$script:mallet = $null

<#
.SYNOPSIS
The newest write time among the files the build depends on.
#>
function Get-SourceStamp {
    $files = @(Get-ChildItem (Join-Path $root 'crates') -Recurse -File |
        Where-Object { $_.FullName -notmatch '\\target\\' })
    $files += Get-Item (Join-Path $root 'Cargo.toml'), (Join-Path $root 'Cargo.lock')
    ($files | Measure-Object LastWriteTimeUtc -Maximum).Maximum
}

<#
.SYNOPSIS
Builds the release build and reports whether it succeeded.
#>
function Invoke-Build {
    Write-Host "`n[watch] Building..." -ForegroundColor Cyan
    Push-Location $root
    try {
        cargo build --release -p mallet | Out-Host
        $LASTEXITCODE -eq 0
    }
    finally {
        Pop-Location
    }
}

<#
.SYNOPSIS
Closes the Mallet this script started, waiting while it asks about unsaved changes.
#>
function Stop-Mallet {
    if ($null -eq $script:mallet -or $script:mallet.HasExited) {
        return
    }
    Write-Host '[watch] Closing Mallet...' -ForegroundColor Cyan
    [void]$script:mallet.CloseMainWindow()
    if (-not $script:mallet.WaitForExit(3000)) {
        Write-Host '[watch] Waiting for Mallet to close (answer its unsaved changes question).' -ForegroundColor Yellow
        $script:mallet.WaitForExit()
    }
}

<#
.SYNOPSIS
Copies the new build next to the running copy and starts it.
#>
function Start-Mallet {
    New-Item -ItemType Directory -Force $runDir | Out-Null
    Copy-Item $built $runExe -Force
    $arguments = @{ FilePath = $runExe; WorkingDirectory = $root; PassThru = $true }
    if ($Map) {
        $arguments.ArgumentList = "`"$((Resolve-Path $Map).Path)`""
    }
    $script:mallet = Start-Process @arguments
    Write-Host "[watch] Mallet started. Watching for changes (Ctrl+C to stop)." -ForegroundColor Green
}

$locked = Get-Process mallet -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -eq $built }
if ($locked) {
    Write-Host '[watch] A Mallet started from target\release is open, so builds cannot replace it. Close it first.' -ForegroundColor Yellow
}

$stamp = Get-SourceStamp
if (Invoke-Build) {
    Start-Mallet
}
else {
    Write-Host '[watch] Build failed. Fix it and save to try again.' -ForegroundColor Red
}

while ($true) {
    Start-Sleep -Milliseconds 700
    $now = Get-SourceStamp
    if ($now -eq $stamp) {
        continue
    }
    Start-Sleep -Milliseconds 300
    $stamp = Get-SourceStamp
    if (Invoke-Build) {
        Stop-Mallet
        Start-Mallet
    }
    else {
        Write-Host '[watch] Build failed. The running Mallet is unchanged.' -ForegroundColor Red
    }
}
