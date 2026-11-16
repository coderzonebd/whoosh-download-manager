# SPDX-License-Identifier: GPL-3.0-or-later
#
# Installs Whoosh Download Manager on Windows with one command (PowerShell):
#
#   irm https://raw.githubusercontent.com/coderzonebd/whoosh-download-manager/pub/packaging/install/install.ps1 | iex
#
# With winget (Windows 10 1809+ and 11 have it): `winget install`, so Windows' list of programs
# and winget both know the app. Without it: the release's per-user installer (no administrator
# rights), checked against the release's SHA256SUMS. The app keeps itself up to date afterwards.
# $env:WDM_VERSION = "v1.2.3" installs that release; $env:WDM_CHANNEL = "beta" prefers pre-releases.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$Repo = 'coderzonebd/whoosh-download-manager'
$WingetId = 'CoderZoneBD.Whoosh'

function Say($m) { Write-Host "wdm: $m" }

function Get-ReleaseTag {
    if ($env:WDM_VERSION) { return $env:WDM_VERSION }
    $api = "https://api.github.com/repos/$Repo/releases"
    if ($env:WDM_CHANNEL -ne 'beta') {
        try { return (Invoke-RestMethod "$api/latest").tag_name } catch { }
    }
    $all = Invoke-RestMethod "$api`?per_page=20"
    if (-not $all) { throw 'no release found' }
    return $all[0].tag_name
}

function Install-WithWinget {
    $winget = Get-Command winget -ErrorAction SilentlyContinue
    if (-not $winget -or $env:WDM_VERSION -or $env:WDM_CHANNEL -eq 'beta') { return $false }
    Say 'installing with winget'
    & winget install --id $WingetId --exact --source winget --accept-source-agreements --accept-package-agreements
    # 0 = installed; -1978335189 = already installed and up to date.
    return ($LASTEXITCODE -eq 0 -or $LASTEXITCODE -eq -1978335189)
}

function Install-FromRelease {
    $tag = Get-ReleaseTag
    $version = $tag.TrimStart('v')
    $arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64' -or $env:PROCESSOR_ARCHITEW6432 -eq 'ARM64') { 'arm64' } else { 'x64' }
    $name = "Whoosh-Download-Manager_${version}_${arch}-setup.exe"
    $base = "https://github.com/$Repo/releases/download/$tag"
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("wdm-" + [guid]::NewGuid())
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        Say "downloading $name"
        $sums = (Invoke-WebRequest "$base/SHA256SUMS" -UseBasicParsing).Content
        $want = ($sums -split "`n" | Where-Object { $_ -match "^([0-9a-fA-F]{64})\s+\*?$([regex]::Escape($name))\s*$" } | ForEach-Object { $Matches[1] }) | Select-Object -First 1
        if (-not $want) { throw "$name is not in SHA256SUMS" }
        $file = Join-Path $tmp $name
        Invoke-WebRequest "$base/$name" -OutFile $file -UseBasicParsing
        $got = (Get-FileHash $file -Algorithm SHA256).Hash
        if ($got -ne $want.ToUpperInvariant()) { throw "$name`: checksum mismatch, nothing was installed" }
        Say 'installing (per user, no administrator rights needed)'
        $p = Start-Process -FilePath $file -ArgumentList '/S' -Wait -PassThru
        if ($p.ExitCode -ne 0) { throw "the installer stopped with code $($p.ExitCode)" }
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
}

if (-not (Install-WithWinget)) { Install-FromRelease }
Say 'done. Whoosh Download Manager keeps itself up to date; find it in the Start menu.'
