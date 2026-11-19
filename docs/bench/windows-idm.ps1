# SPDX-License-Identifier: Apache-2.0
# Benchmarks WDM against Internet Download Manager on this Windows PC.
#
# Run it from the unzipped "wdm-bench-windows-x64" kit (built by
# .github/workflows/bench-kit.yml), in PowerShell:
#
#   powershell -ExecutionPolicy Bypass -File .\windows-idm.ps1          # full matrix, a few hours
#   powershell -ExecutionPolicy Bypass -File .\windows-idm.ps1 -Quick   # small matrix, ~15 minutes
#
# Everything runs on this PC: the test server (labserver) starts inside
# wdm-bench on 127.0.0.1, so the network and your internet speed play no
# part. The result is a zip next to this script; send it back.
# Details and limits: docs/bench/README.md, "Windows, with IDM".

param(
    [switch]$Quick,
    # Extra clients from `wdm-bench fetch` (aria2-next, mdm, surge, gopeed).
    [switch]$All,
    [int]$Runs = 0
)

$ErrorActionPreference = "Stop"
$Kit = $PSScriptRoot
$Bench = Join-Path $Kit "wdm-bench.exe"
$Wdm = Join-Path $Kit "wdm.exe"

function Say($msg) { Write-Host "wdm-bench: $msg" -ForegroundColor Cyan }
function Fail($msg) { Write-Host "wdm-bench: $msg" -ForegroundColor Red; exit 1 }

foreach ($f in @($Bench, $Wdm, (Join-Path $Kit "wdm-daemon.exe"))) {
    if (-not (Test-Path $f)) { Fail "$f is missing; unzip the whole kit and run the script from inside it." }
}

# IDM must be installed and must not be running: /q only exits the instance
# the command starts, so a running IDM makes every run wait for its timeout.
$idm = @("${env:ProgramFiles(x86)}", "$env:ProgramFiles") |
    Where-Object { $_ } |
    ForEach-Object { Join-Path $_ "Internet Download Manager\IDMan.exe" } |
    Where-Object { Test-Path $_ } |
    Select-Object -First 1
if (-not $idm) { Fail "Internet Download Manager is not installed (looked in Program Files). Install it, then run this again." }
if (Get-Process -Name IDMan -ErrorAction SilentlyContinue) {
    Fail "IDM is running. Right-click its tray icon > Exit, then run this again."
}
$idmVersion = (Get-Item $idm).VersionInfo.ProductVersion
Say "IDM $idmVersion at $idm"

Write-Host @"

Before the benchmark, check these once in IDM (Downloads > Options):
  - Download: turn OFF "Show download complete dialog" and "Show start download dialog".
  - Connection: note the "Default max. conn. number" (IDM's default is 8). Leave it as it is.
And in Windows Security > Virus & threat protection > Manage settings > Exclusions,
add this folder, or Defender's scanning will dominate the small downloads:
  $env:LOCALAPPDATA\wdm-bench

Keep the PC plugged in, close other programs, and don't use it while the benchmark runs.
"@
$answer = Read-Host "Ready? (y/n)"
if ($answer -notmatch '^[yY]') { Say "stopped"; exit 0 }

$clients = "wdm,idm,curl"
if ($All) {
    Say "fetching the other downloaders from their official releases"
    & $Bench fetch
    $clients = "wdm,idm,curl,aria2-next,mdm,surge,gopeed"
}

$label = "{0}-{1}-idm" -f (Get-Date -Format yyyy-MM-dd), $env:COMPUTERNAME.ToLower()
$out = Join-Path $Kit "results"
$work = Join-Path $env:LOCALAPPDATA "wdm-bench\work"
$benchArgs = @("run", "--clients", $clients, "--seed", "20261120", "--label", $label,
          "--out", $out, "--work-dir", $work, "--bin", "wdm=$Wdm")
if ($Quick) { $benchArgs += "--quick" }
if ($Runs -gt 0) { $benchArgs += @("--runs", "$Runs") }

& $Bench list --bin "wdm=$Wdm"
Say "running: wdm-bench $($benchArgs -join ' ')"
$started = Get-Date
& $Bench @benchArgs
if ($LASTEXITCODE -ne 0) { Say "wdm-bench exited with $LASTEXITCODE; the partial results are still saved" }

$dir = Join-Path $out $label
& $Bench report $dir -o (Join-Path $dir "report.md")
"IDM version: $idmVersion`r`nWindows: $([Environment]::OSVersion.VersionString)`r`nStarted: $started`r`nFinished: $(Get-Date)" |
    Set-Content -Path (Join-Path $dir "windows-notes.txt")

$zip = Join-Path $Kit "$label.zip"
Compress-Archive -Path $dir -DestinationPath $zip -Force
Say "done. Report: $dir\report.md"
Say "send this file back: $zip"
