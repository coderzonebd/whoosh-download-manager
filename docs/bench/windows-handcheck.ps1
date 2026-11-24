# SPDX-License-Identifier: GPL-3.0-or-later
# WDM on a fresh Windows machine, the way a person tries it, with a
# screenshot of each step and what Windows' UI Automation sees in each
# window (the names a screen reader would read):
#   1. the main window (Downloads, Settings, History, Connect your browser);
#   2. three downloads from cdn.kernel.org, while running and when done;
#   3. the main window closed: WDM stays in the tray;
#   4. a download handed over as the browser extension does (wdm-nmhost,
#      fromBrowser): the confirm window;
#   5. a video page handed over the same way: the media picker.
# Run by .github/workflows/windows-handcheck.yml; writes -Out/*.png,
# steps.log, uia-*.txt and the logs.
param(
    [Parameter(Mandatory)] [string] $Bin,
    [Parameter(Mandatory)] [string] $Out
)
$ErrorActionPreference = "Stop"
New-Item -ItemType Directory -Force $Out | Out-Null
$log = Join-Path $Out "steps.log"
function Step([string] $what) { $line = "{0:HH:mm:ss} {1}" -f (Get-Date), $what; $line; Add-Content $log $line }

Add-Type -AssemblyName System.Windows.Forms, System.Drawing, UIAutomationClient, UIAutomationTypes
Add-Type -Namespace W -Name Win -MemberDefinition @"
[DllImport("user32.dll")] public static extern bool PostMessage(System.IntPtr h, uint m, System.IntPtr w, System.IntPtr l);
[DllImport("user32.dll")] public static extern bool SetForegroundWindow(System.IntPtr h);
"@
$b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
Step "screen $($b.Width)x$($b.Height)"

function Shot([string] $name) {
    Start-Sleep -Milliseconds 800
    $img = New-Object System.Drawing.Bitmap $b.Width, $b.Height
    [System.Drawing.Graphics]::FromImage($img).CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
    $img.Save((Join-Path $Out "$name.png"))
    Step "shot $name"
}
$A = [System.Windows.Automation.AutomationElement]
$T = [System.Windows.Automation.TreeScope]
# Every top-level window of WDM's processes, and the names of what is in it.
function Uia([string] $name) {
    $f = Join-Path $Out "uia-$name.txt"
    $ids = @(Get-Process -Name "wdm-prompt", "wdm-tray" -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
    foreach ($w in $A::RootElement.FindAll($T::Children, [System.Windows.Automation.Condition]::TrueCondition)) {
        if ($ids -notcontains $w.Current.ProcessId) { continue }
        $r = $w.Current.BoundingRectangle
        Add-Content $f ("WINDOW '{0}' {1}x{2} at {3},{4}" -f $w.Current.Name, [int]$r.Width, [int]$r.Height, [int]$r.X, [int]$r.Y)
        $all = $w.FindAll($T::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
        foreach ($e in $all) {
            $n = $e.Current.Name
            if ($n) { Add-Content $f ("  {0}: {1}" -f $e.Current.ControlType.ProgrammaticName.Replace("ControlType.", ""), $n) }
        }
    }
    Step "uia $name"
}
# Clicks the first element of a WDM window whose name is one of `names`.
function Click([string[]] $names) {
    $ids = @(Get-Process -Name "wdm-prompt" -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
    foreach ($w in $A::RootElement.FindAll($T::Children, [System.Windows.Automation.Condition]::TrueCondition)) {
        if ($ids -notcontains $w.Current.ProcessId) { continue }
        foreach ($n in $names) {
            $e = $w.FindFirst($T::Descendants, (New-Object System.Windows.Automation.PropertyCondition($A::NameProperty, $n)))
            if (-not $e) { continue }
            try {
                $e.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
                Step "clicked '$n'"
                return $true
            } catch { Step "could not click '$n': $_" }
        }
    }
    Step "nothing to click among: $($names -join ', ')"
    $false
}
function Windows() {
    @(Get-Process -Name "wdm-prompt" -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | ForEach-Object { "$($_.MainWindowTitle)" })
}
function Running() { (@("wdm-prompt", "wdm-tray", "wdm-daemon") | Where-Object { Get-Process $_ -ErrorAction SilentlyContinue }) -join ", " }

$env:WDM_DATA_DIR = Join-Path $env:RUNNER_TEMP "wdm-data"
$env:WDM_LOG = "info"
$dl = New-Item -ItemType Directory -Force (Join-Path $env:RUNNER_TEMP "dl")

# ------------------------------------------------------------- 1. main window
Step "start: wdm-prompt --main"
Start-Process "$Bin\wdm-prompt.exe" -ArgumentList "--main"
Start-Sleep 12
Step "running: $(Running); windows: $((Windows) -join '; ')"
& "$Bin\wdm.exe" settings set "defaultDirectory=$dl" "maxActiveDownloads=3" | Out-Null
Shot "01-downloads-empty"; Uia "01-downloads-empty"
foreach ($page in "settings", "history", "browsers") {
    Start-Process "$Bin\wdm-prompt.exe" -ArgumentList "--main", "--page", $page -Wait
    Start-Sleep 3
    Shot "02-$page"; Uia "02-$page"
}
# Back to the list by the window's own navigation.
Click @("All downloads", "Downloads", "All") | Out-Null
Start-Sleep 2

# ------------------------------------------------------------- 2. downloads
$base = "https://cdn.kernel.org/pub/linux/kernel/v6.x"
$files = "linux-6.6.tar.xz", "linux-6.5.tar.xz", "linux-6.4.tar.xz"
Step "add: $($files -join ', ')"
& "$Bin\wdm.exe" add @($files | ForEach-Object { "$base/$_" }) | Out-Null
Start-Sleep 3
Shot "03-downloading"; Uia "03-downloading"
$sw = [Diagnostics.Stopwatch]::StartNew()
while ($sw.Elapsed.TotalSeconds -lt 300) {
    $done = @($files | Where-Object { Test-Path (Join-Path $dl $_) -PathType Leaf }).Count
    if ($done -eq $files.Count) { break }
    Start-Sleep 1
}
Step "finished $done of $($files.Count) in $([int]$sw.Elapsed.TotalSeconds) s"
Start-Sleep 2
Shot "04-completed"; Uia "04-completed"
& "$Bin\wdm.exe" list | Add-Content (Join-Path $Out "list.txt")

# ------------------------------------------------------------- 3. tray
foreach ($p in Get-Process -Name "wdm-prompt" -ErrorAction SilentlyContinue) {
    if ($p.MainWindowHandle -ne 0) { [W.Win]::PostMessage($p.MainWindowHandle, 0x10, 0, 0) | Out-Null }
}
Start-Sleep 5
Step "after closing the main window: running: $(Running); windows: $((Windows) -join '; ')"
Shot "05-tray-only"

# ------------------------------------------------------------- 4. confirm
$nm = Join-Path $PSScriptRoot "nm-send.py"
$add = '{"jsonrpc":"2.0","id":"a1","method":"download.add","params":{"url":"' + "$base/linux-6.3.tar.xz" + '","fromBrowser":true}}'
Step "extension hands over a download (fromBrowser)"
$job = Start-Job { param($py, $h, $m, $o) python $py $h 40 '{"jsonrpc":"2.0","id":"s","method":"nm.status"}' $m *>> $o } -ArgumentList $nm, "$Bin\wdm-nmhost.exe", $add, (Join-Path $Out "nm-confirm.txt")
Start-Sleep 15
Step "running: $(Running); windows: $((Windows) -join '; ')"
Shot "06-confirm"; Uia "06-confirm"
Click @("Start download", "Download", "Start") | Out-Null
Start-Sleep 6
Shot "07-after-confirm"; Uia "07-after-confirm"
Wait-Job $job -Timeout 60 | Out-Null; Receive-Job $job | Out-Null

# ------------------------------------------------------------- 5. media
$offer = '{"jsonrpc":"2.0","id":"m1","method":"media.offer","params":{"url":"https://www.youtube.com/watch?v=jNQXAC9IVRw","title":"Me at the zoo"}}'
Step "extension offers a video page"
$job = Start-Job { param($py, $h, $m, $o) python $py $h 60 $m *>> $o } -ArgumentList $nm, "$Bin\wdm-nmhost.exe", $offer, (Join-Path $Out "nm-media.txt")
Start-Sleep 30
Step "running: $(Running); windows: $((Windows) -join '; ')"
Shot "08-media"; Uia "08-media"
Start-Sleep 20
Shot "09-media-later"; Uia "09-media-later"
Wait-Job $job -Timeout 60 | Out-Null; Receive-Job $job | Out-Null

# ------------------------------------------------------------- logs
& "$Bin\wdm.exe" list | Add-Content (Join-Path $Out "list.txt")
Get-Process -Name "wdm-prompt", "wdm-tray", "wdm-daemon", "wdm-nmhost" -ErrorAction SilentlyContinue | Stop-Process -Force
Get-ChildItem $env:WDM_DATA_DIR -Recurse -Filter "*.log*" -ErrorAction SilentlyContinue | Copy-Item -Destination $Out
Step "done"
