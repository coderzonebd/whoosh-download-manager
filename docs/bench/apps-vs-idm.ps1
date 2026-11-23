# SPDX-License-Identifier: GPL-3.0-or-later
# WDM against IDM as people run them, on one Windows machine: the whole app
# (WDM: wdm-prompt with its main window, wdm-tray, wdm-daemon; IDM: IDMan,
# IEMonitor and helpers), one after the other, never together.
#
# For each app, from the same local server (wdm-labserver, loopback), or
# from real servers over HTTPS (-Urls, checked against -SumsUrl):
#   1. main window open, nothing downloading (15 s);
#   2. N files downloading at once, until all are complete (time, memory, CPU);
#   3. main window closed, the app in the tray (15 s).
# Memory is the private working set, the number Task Manager shows, summed
# over the app's processes and sampled about four times a second.
#
# Each app is started once and closed before it is measured, so both are
# measured as people run them every day, not on first start (WDM fetches
# yt-dlp, ffmpeg and deno on its first start; IDM finishes setting up).
#
# Run by .github/workflows/apps-vs-idm.yml; needs the WDM binaries in -Bin
# and IDM installed. Writes results.json and summary.md to -Out.
param(
    [Parameter(Mandatory)] [string] $Bin,
    [Parameter(Mandatory)] [string] $Out,
    [int] $Files = 8,
    [string] $Size = "200MiB",
    # Real files instead of the local server (all at once, as with it).
    [string[]] $Urls = @(),
    # SHA-256 list (sha256sum format, may be signed) to check them against.
    [string] $SumsUrl = "",
    # Measure IDM first: a CDN serves the second app from a warmer cache,
    # so real-file runs are made in both orders.
    [switch] $IdmFirst
)
$ErrorActionPreference = "Stop"
New-Item -ItemType Directory -Force $Out | Out-Null

Add-Type -Namespace W -Name Win -MemberDefinition @"
[DllImport("user32.dll")] public static extern bool PostMessage(System.IntPtr h, uint m, System.IntPtr w, System.IntPtr l);
"@

# ------------------------------------------------------------- the files
$lab = $null
if ($Urls.Count -gt 0) {
    $urls = $Urls
    $Files = $urls.Count
    $where = "real servers over HTTPS"
} else {
    $scen = New-Item -ItemType Directory -Force (Join-Path $env:RUNNER_TEMP "scenarios")
    $toml = "description = `"head to head`"`n"
    for ($i = 1; $i -le $Files; $i++) { $toml += "[[files]]`nname = `"f$i.bin`"`nsize = `"$Size`"`n" }
    Set-Content (Join-Path $scen "h2h.toml") $toml
    $lab = Start-Process "$Bin\wdm-labserver.exe" -ArgumentList "--no-tls", "--port", "18080", "--scenarios", "$scen" -PassThru -WindowStyle Hidden
    Start-Sleep 3
    $urls = 1..$Files | ForEach-Object { "http://127.0.0.1:18080/s/h2h/f$_.bin" }
    $where = "loopback"
}
# Each file's name and length, from the server.
$expect = [ordered]@{}
foreach ($u in $urls) {
    $len = (Invoke-WebRequest -UseBasicParsing -Method Head $u).Headers["Content-Length"]
    $expect[[uri]::UnescapeDataString(([uri]$u).Segments[-1])] = [int64]("$len")
}
$bytes = ($expect.Values | Measure-Object -Sum).Sum
"$Files files, $bytes bytes in all, from $where"
$sums = @{}
if ($SumsUrl) {
    foreach ($line in ((Invoke-WebRequest -UseBasicParsing $SumsUrl).Content -split "`n")) {
        $line = $line.Trim()
        if ($line -match '^([0-9a-f]{64})\s+\*?(\S+)$' -and $expect.Contains($Matches[2])) { $sums[$Matches[2]] = $Matches[1] }
    }
    if ($sums.Count -ne $Files) { throw "SHA-256 found for $($sums.Count) of $Files files" }
}

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
    $sums = @{}
    $cpu0 = Cpu-Seconds $names
    $sw = [Diagnostics.Stopwatch]::StartNew()
    & $start
    while ($sw.Elapsed.TotalSeconds -lt $limitSec) {
        if (& $until) { break }
        $by = Private-Set $names
        $sum = 0
        foreach ($k in $by.Keys) { $sum += $by[$k]; $sums[$k] = [int64]$sums[$k] + $by[$k]; if ($by[$k] -gt [int64]$peaks[$k]) { $peaks[$k] = $by[$k] } }
        $samples += $sum
        Start-Sleep -Milliseconds 250
    }
    $secs = $sw.Elapsed.TotalSeconds
    $each = [ordered]@{}
    $avgEach = [ordered]@{}
    foreach ($k in ($peaks.Keys | Sort-Object)) {
        $each[$k] = [math]::Round($peaks[$k] / 1MB, 1)
        $avgEach[$k] = [math]::Round($sums[$k] / [math]::Max(1, $samples.Count) / 1MB, 1)
    }
    [pscustomobject]@{
        seconds = [math]::Round($secs, 2)
        peak_mb = [math]::Round((($samples | Measure-Object -Maximum).Maximum) / 1MB, 1)
        avg_mb  = [math]::Round((($samples | Measure-Object -Average).Average) / 1MB, 1)
        cpu_s   = [math]::Round((Cpu-Seconds $names) - $cpu0, 1)
        done    = [bool](& $until)
        peak_by_process_mb = $each
        avg_by_process_mb = $avgEach
    }
}
function Complete([string] $dir) {
    foreach ($name in $expect.Keys) {
        $f = Get-Item -LiteralPath (Join-Path $dir $name) -ErrorAction SilentlyContinue
        if (-not $f -or $f.Length -ne $expect[$name]) { return $false }
    }
    $true
}
# Every file's SHA-256 matches the list (true when there is none).
function Verified([string] $dir) {
    foreach ($name in $sums.Keys) {
        $h = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $dir $name) -ErrorAction SilentlyContinue).Hash
        if (-not $h -or $h.ToLower() -ne $sums[$name]) { Write-Host "$name does not match its SHA-256"; return $false }
    }
    $true
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
$wdmPhase = {
$wdmNames = @("wdm-prompt", "wdm-tray", "wdm-daemon")
$env:WDM_DATA_DIR = Join-Path $env:RUNNER_TEMP "wdm-data"
# When and how much the daemon gives memory back, in its log (copied to -Out).
$env:WDM_LOG = "info,wdm_daemon::memory=debug"
$wdmDir = New-Item -ItemType Directory -Force (Join-Path $env:RUNNER_TEMP "dl-wdm")
# Once before measuring (first-start setup), then as every day.
Start-Process "$Bin\wdm-prompt.exe" -ArgumentList "--main"
Start-Sleep 45
Get-Process -Name $wdmNames -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep 3
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
$results.wdm_verified = Verified $wdmDir
"WDM: " + ($results | ConvertTo-Json -Compress)
Get-Process -Name $wdmNames -ErrorAction SilentlyContinue | Stop-Process -Force
Get-ChildItem $env:WDM_DATA_DIR -Recurse -Filter "*.log*" -ErrorAction SilentlyContinue | Copy-Item -Destination $Out
Remove-Item -Recurse -Force $wdmDir
Start-Sleep 3
}

# ------------------------------------------------------------- IDM
$idmPhase = {
$idmNames = @("IDMan", "IEMonitor", "IDMIntegrator64", "idmBroker", "IDMMsgHost")
$idmDir = New-Item -ItemType Directory -Force (Join-Path $env:RUNNER_TEMP "dl-idm")
$idm = "${env:ProgramFiles(x86)}\Internet Download Manager\IDMan.exe"
# Once before measuring, as for WDM.
Start-Process $idm
Start-Sleep 45
Get-Process -Name $idmNames -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep 3
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
$results.idm_verified = Verified $idmDir
"IDM: " + ($results | ConvertTo-Json -Compress)
Get-Process -Name $idmNames -ErrorAction SilentlyContinue | Stop-Process -Force
Remove-Item -Recurse -Force $idmDir
Start-Sleep 3
}

$results.order = if ($IdmFirst) { "IDM first" } else { "WDM first" }
if ($IdmFirst) { . $idmPhase; . $wdmPhase } else { . $wdmPhase; . $idmPhase }
if ($lab) { $lab | Stop-Process -Force }

# ------------------------------------------------------------- report
$results | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $Out "results.json")
function Row([string] $what, $w, $i, [string] $unit, [switch] $lowerWins) {
    $win = if ($w -eq $i) { "tie" } elseif (($w -lt $i) -eq [bool]$lowerWins) { "**WDM**" } else { "IDM" }
    "| $what | $w $unit | $i $unit | $win |"
}
$mbps = { param($r) if ($r.done -and $r.seconds -gt 0) { [math]::Round($bytes / 1MB / $r.seconds, 1) } else { 0 } }
$md = @(
    "## WDM vs IDM, the whole app ($Files files, $([math]::Round($bytes / 1MB)) MB in all, at once, $where; $($results.order))",
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
    "Peak memory by process while downloading: WDM $(($results.wdm_downloading.peak_by_process_mb | ConvertTo-Json -Compress)); IDM $(($results.idm_downloading.peak_by_process_mb | ConvertTo-Json -Compress)). Average by process, window open and idle: WDM $(($results.wdm_idle_window.avg_by_process_mb | ConvertTo-Json -Compress)); IDM $(($results.idm_idle_window.avg_by_process_mb | ConvertTo-Json -Compress)). Main window closed for the tray rows: WDM $($results.wdm_window_closed), IDM $($results.idm_window_closed).",
    "",
    "Memory is the private working set (Task Manager's number) summed over each app's processes. Each app runs alone, with its default settings, started once before it is measured; WDM is allowed $Files downloads at once like IDM. Completed: WDM $($results.wdm_downloading.done), IDM $($results.idm_downloading.done). SHA-256 checked: WDM $($results.wdm_verified), IDM $($results.idm_verified)."
)
$md -join "`n" | Set-Content (Join-Path $Out "summary.md")
$md -join "`n"
