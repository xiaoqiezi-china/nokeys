# Builds the release binaries and packs the installer + portable zip into dist\.
#   powershell -ExecutionPolicy Bypass -File _tools\pack.ps1
param(
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$version = (Select-String -Path 'Cargo.toml' -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$app = 'target\release\nokeys.exe'
$dist = 'dist'

if (-not $SkipBuild) {
    Get-Process nokeys -ErrorAction SilentlyContinue | Stop-Process -Force
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw 'cargo build --release failed' }
}

New-Item -ItemType Directory -Force -Path $dist | Out-Null
if (-not (Test-Path $app)) { throw "missing $app" }

# ---- MSI (WiX 3): standard Windows Installer package ----
$wixBin = $env:WIX_BIN
if (-not $wixBin) {
    foreach ($candidate in @(
        (Join-Path $env:LOCALAPPDATA 'nokeys-tools\wix\bin'),
        'C:\Program Files (x86)\WiX Toolset v3.14\bin',
        'C:\Program Files (x86)\WiX Toolset v3.11\bin'
    )) {
        if (Test-Path (Join-Path $candidate 'candle.exe')) { $wixBin = $candidate; break }
    }
}
if (-not $wixBin) {
    Write-Warning 'WiX 3 not found (set WIX_BIN), skipping the MSI'
} else {
    $obj = 'build\wix\NoKeys.wixobj'
    New-Item -ItemType Directory -Force -Path 'build\wix' | Out-Null
    Push-Location packaging
    & (Join-Path $wixBin 'candle.exe') -nologo -arch x64 -out "..\$obj" 'NoKeys.wxs'
    if ($LASTEXITCODE -ne 0) { Pop-Location; throw 'candle failed' }
    & (Join-Path $wixBin 'light.exe') -nologo -ext WixUIExtension -cultures:zh-CN -out "..\$dist\NoKeys-$version.msi" "..\$obj"
    $lightExit = $LASTEXITCODE
    Pop-Location
    if ($lightExit -ne 0) { throw 'light failed' }
}

# ---- portable zip ----
$zip = Join-Path $dist "NoKeys-$version-portable.zip"
if (Test-Path $zip) { Remove-Item $zip -Force }
$stage = Join-Path $env:TEMP ("nokeys-portable-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $stage | Out-Null
Copy-Item $app (Join-Path $stage 'nokeys.exe')
if (Test-Path 'README.md') { Copy-Item 'README.md' $stage }
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip -Force
Remove-Item $stage -Recurse -Force

Write-Host ''
Write-Host "version $version"
Get-ChildItem $dist | ForEach-Object {
    $h = (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower()
    '{0,-32} {1,10:N1} MB  sha256 {2}' -f $_.Name, ($_.Length / 1MB), $h
}
