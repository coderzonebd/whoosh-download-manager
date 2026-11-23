# SPDX-License-Identifier: GPL-3.0-or-later
# WDM against IDM as people run them, on one Windows machine: the whole app
# (WDM: wdm-prompt with its main window, wdm-tray, wdm-daemon; IDM: IDMan,
# IEMonitor and helpers), one after the other, never together.
#
# For each app, from the same local server (wdm-labserver, loopback):
#   1. main window open, nothing downloading (15 s);
#   2. N files downloading at once, until all are complete (time, memory, CPU);
#   3. main window closed, the app in the tray (15 s).
# Memory is the private working set, the number Task Manager shows, summed
# over the app's processes and sampled twice a second.
#
# Run by .github/workflows/apps-vs-idm.yml; needs the WDM binaries in -Bin
# and IDM installed. Writes results.json and summary.md to -Out.
param(
    [Parameter(Mandatory)] [string] $Bin,
    [Parameter(Mandatory)] [string] $Out,
    [int] $Files = 8,
    [string] $Size = "200MiB"
)
$ErrorActionPreference = "Stop"
New-Item -ItemType Directory -Force $Out | Out-Null

Add-Type -Namespace W -Name Win -MemberDefinition @"
[DllImport("user32.dll")] public static extern bool PostMessage(System.IntPtr h, uint m, System.IntPtr w, System.IntPtr l);
"@

# ------------------------------------------------------------- the server
$scen = New-Item -ItemType Directory -Force (Join-Path $env:RUNNER_TEMP "scenarios")
$toml = "description = `"head to head`"`n"
for ($i = 1; $i -le $Files; $i++) { $toml += "[[files]]`nname = `"f$i.bin`"`nsize = `"$Size`"`n" }
Set-Content (Join-Path $scen "h2h.toml") $toml
$lab = Start-Process "$Bin\wdm-labserver.exe" -ArgumentList "--no-tls", "--port", "18080", "--scenarios", "$scen" -PassThru -WindowStyle Hidden
Start-Sleep 3
$bytes = (Invoke-WebRequest -UseBasicParsing -Method Head "http://127.0.0.1:18080/s/h2h/f1.bin").Headers["Content-Length"]
$bytes = [int64]("$bytes")
"server up, $Files files of $bytes bytes"
$urls = 1..$Files | ForEach-Object { "http://127.0.0.1:18080/s/h2h/f$_.bin" }

# ------------------------------------------------------------- measuring
function Private-Set([string[]] $names) {
    # Task Manager's "Memory": the private working set, per process name.
    $by = @{}
    foreach ($p in Get-CimInstance Win32_PerfFormattedData_PerfProc_Process) {
        $n = $p.Name -replace '#\d+$', ''
        if ($names -contains $n) { $by[$n] = [int64]$by[$n] + [int64]$p.WorkingSetPrivate }
    }
    $by
}
function Cpu-Seconds([string[]] $names) {
    $s = 0.0
    foreach ($p in Get-Process -Name $names -ErrorAction SilentlyContinue) {
        try { $s += $p.TotalProcessorTime.TotalSeconds } catch {}
    }
    $s
}
# Samples memory from before `start` runs (the clock includes handing the
# files over) until `until` holds.
function Watch([string[]] $names, [scriptblock] $start, [scriptblock] $until, [int] $limitSec) {
    $samples = @()
    $peaks = @{}
    $cpu0 = Cpu-Seconds $names
    $sw = [Diagnostics.Stopwatch]::StartNew()
    & $start
    while ($sw.Elapsed.TotalSeconds -lt $limitSec) {
        if (& $until) { break }
        $by = Private-Set $names
        $sum = 0
        foreach ($k in $by.Keys) { $sum += $by[$k]; if ($by[$k] -gt [int64]$peaks[$k]) { $peaks[$k] = $by[$k] } }
        $samples += $sum
        Start-Sleep -Milliseconds 250
    }
    $secs = $sw.Elapsed.TotalSeconds
    $each = [ordered]@{}
    foreach ($k in ($peaks.Keys | Sort-Object)) { $each[$k] = [math]::Round($peaks[$k] / 1MB, 1) }
    [pscustomobject]@{
        seconds = [math]::Round($secs, 2)
        peak_mb = [math]::Round((($samples | Measure-Object -Maximum).Maximum) / 1MB, 1)
        avg_mb  = [math]::Round((($samples | Measure-Object -Average).Average) / 1MB, 1)
        cpu_s   = [math]::Round((Cpu-Seconds $names) - $cpu0, 1)
        done    = [bool](& $until)
        peak_by_process_mb = $each
    }
}
function Complete([string] $dir) {
    $ok = @(Get-ChildItem $dir -Filter "f*.bin" -ErrorAction SilentlyContinue | Where-Object { $_.Length -eq $bytes })
    $ok.Count -ge $Files
}
function Windows([string[]] $names) {
    @(Get-Process -Name $names -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | ForEach-Object { "$($_.Name): $($_.MainWindowTitle)" })
}
function Close-Window([string] $name) {
    foreach ($p in Get-Process -Name $name -ErrorAction SilentlyContinue) {
        if ($p.MainWindowHandle -ne 0) { [W.Win]::PostMessage($p.MainWindowHandle, 0x10, 0, 0) | Out-Null }
    }
    Start-Sleep 5
    $left = Windows @($name)
    "windows of $name after closing: $($left -join '; ')"
    $left.Count -eq 0
}
$never = { $false }
$nothing = { }
$results = [ordered]@{ files = $Files; bytes_each = $bytes }

# ------------------------------------------------------------- WDM
$wdmNames = @("wdm-prompt", "wdm-tray", "wdm-daemon")
$env:WDM_DATA_DIR = Join-Path $env:RUNNER_TEMP "wdm-data"
# When and how much the daemon gives memory back, in its log (copied to -Out).
$env:WDM_LOG = "info,wdm_daemon::memory=debug"
$wdmDir = New-Item -ItemType Directory -Force (Join-Path $env:RUNNER_TEMP "dl-wdm")
Start-Process "$Bin\wdm-prompt.exe" -ArgumentList "--main"
Start-Sleep 15
foreach ($n in $wdmNames) { if (-not (Get-Process $n -ErrorAction SilentlyContinue)) { throw "$n is not running" } }
# Every file at once, as IDM does; connections stay at WDM's default.
& "$Bin\wdm.exe" settings set "defaultDirectory=$wdmDir" "maxActiveDownloads=$Files" | Out-Null
if ($LASTEXITCODE -ne 0) { throw "wdm settings set failed" }
"WDM windows: $((Windows $wdmNames) -join '; ')"
$results.wdm_idle_window = Watch $wdmNames $nothing $never 15
$results.wdm_downloading = Watch $wdmNames {
    & "$Bin\wdm.exe" add @urls | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "wdm add failed" }
} { Complete $wdmDir } 900
$results.wdm_window_closed = Close-Window "wdm-prompt"
$results.wdm_idle_tray = Watch $wdmNames $nothing $never 15
"WDM: " + ($results | ConvertTo-Json -Compress)
Get-Process -Name $wdmNames -ErrorAction SilentlyContinue | Stop-Process -Force
Get-ChildItem $env:WDM_DATA_DIR -Recurse -Filter "*.log*" -ErrorAction SilentlyContinue | Copy-Item -Destination $Out
Remove-Item -Recurse -Force $wdmDir
Start-Sleep 3

# ------------------------------------------------------------- IDM
$idmNames = @("IDMan", "IEMonitor", "IDMIntegrator64", "idmBroker", "IDMMsgHost")
$idmDir = New-Item -ItemType Directory -Force (Join-Path $env:RUNNER_TEMP "dl-idm")
$idm = "${env:ProgramFiles(x86)}\Internet Download Manager\IDMan.exe"
Start-Process $idm
Start-Sleep 15
if (-not (Get-Process IDMan -ErrorAction SilentlyContinue)) { throw "IDM is not running" }
"IDM windows: $((Windows $idmNames) -join '; ')"
$results.idm_idle_window = Watch $idmNames $nothing $never 15
# /n: no questions, starts at once (every file together, IDM's own connections).
$results.idm_downloading = Watch $idmNames {
    foreach ($u in $urls) { Start-Process $idm -ArgumentList "/d", $u, "/p", "$idmDir", "/n" -Wait }
} { Complete $idmDir } 900
$results.idm_window_closed = Close-Window "IDMan"
$results.idm_idle_tray = Watch $idmNames $nothing $never 15
Get-Process -Name $idmNames -ErrorAction SilentlyContinue | Stop-Process -Force
$lab | Stop-Process -Force

# ------------------------------------------------------------- report
$results | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $Out "results.json")
function Row([string] $what, $w, $i, [string] $unit, [switch] $lowerWins) {
    $win = if ($w -eq $i) { "tie" } elseif (($w -lt $i) -eq [bool]$lowerWins) { "**WDM**" } else { "IDM" }
    "| $what | $w $unit | $i $unit | $win |"
}
$mbps = { param($r) if ($r.done -and $r.seconds -gt 0) { [math]::Round($Files * $bytes / 1MB / $r.seconds, 1) } else { 0 } }
$md = @(
    "## WDM vs IDM, the whole app ($Files files of $([math]::Round($bytes / 1MB)) MB at once, loopback)",
    "",
    "| | WDM | IDM | Better |",
    "|---|---|---|---|",
    (Row "Memory, main window open, idle" $results.wdm_idle_window.avg_mb $results.idm_idle_window.avg_mb "MB" -lowerWins),
    (Row "Memory, in the tray, idle" $results.wdm_idle_tray.avg_mb $results.idm_idle_tray.avg_mb "MB" -lowerWins),
    (Row "Memory while downloading, peak" $results.wdm_downloading.peak_mb $results.idm_downloading.peak_mb "MB" -lowerWins),
    (Row "Memory while downloading, average" $results.wdm_downloading.avg_mb $results.idm_downloading.avg_mb "MB" -lowerWins),
    (Row "Time for all files" $results.wdm_downloading.seconds $results.idm_downloading.seconds "s" -lowerWins),
    (Row "Throughput" (& $mbps $results.wdm_downloading) (& $mbps $results.idm_downloading) "MB/s"),
    (Row "CPU time while downloading" $results.wdm_downloading.cpu_s $results.idm_downloading.cpu_s "s" -lowerWins),
    "",
    "Peak memory by process while downloading: WDM $(($results.wdm_downloading.peak_by_process_mb | ConvertTo-Json -Compress)); IDM $(($results.idm_downloading.peak_by_process_mb | ConvertTo-Json -Compress)). Main window closed for the tray rows: WDM $($results.wdm_window_closed), IDM $($results.idm_window_closed).",
    "",
    "Memory is the private working set (Task Manager's number) summed over each app's processes. Each app runs alone, with its default settings; WDM is allowed $Files downloads at once like IDM. Completed: WDM $($results.wdm_downloading.done), IDM $($results.idm_downloading.done)."
)
$md -join "`n" | Set-Content (Join-Path $Out "summary.md")
$md -join "`n"
