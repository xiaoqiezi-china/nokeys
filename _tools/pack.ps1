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
$setupBin = 'target\release\setup.exe'
$dist = 'dist'

if (-not $SkipBuild) {
    Get-Process nokeys -ErrorAction SilentlyContinue | Stop-Process -Force
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw 'cargo build --release failed' }
}

New-Item -ItemType Directory -Force -Path $dist | Out-Null
foreach ($f in @($app, $setupBin)) {
    if (-not (Test-Path $f)) { throw "missing $f" }
}

# ---- installer: setup.exe + appended application + trailer ----
$setup = Join-Path $dist "NoKeys-$version-setup.exe"
Copy-Item $setupBin $setup -Force
$payload = [IO.File]::ReadAllBytes((Resolve-Path $app))
$trailer = New-Object byte[] 16
[BitConverter]::GetBytes([int64]$payload.Length).CopyTo($trailer, 0)
[Text.Encoding]::ASCII.GetBytes('NOKEYS01').CopyTo($trailer, 8)
$stream = [IO.File]::Open($setup, 'Append')
$stream.Write($payload, 0, $payload.Length)
$stream.Write($trailer, 0, $trailer.Length)
$stream.Close()

# verify the package can be parsed back
$check = [IO.File]::ReadAllBytes($setup)
$tail = $check[($check.Length - 16)..($check.Length - 1)]
$size = [BitConverter]::ToInt64($tail, 0)
$magic = [Text.Encoding]::ASCII.GetString($tail, 8, 8)
$setupLen = $check.Length - 16 - $size
if ($magic -ne 'NOKEYS01' -or $size -ne $payload.Length -or $setupLen -le 0) {
    throw "package trailer is wrong (magic=$magic size=$size)"
}
$sha = [Security.Cryptography.SHA256]::Create()
$installedApp = $check[$setupLen..($setupLen + $size - 1)]
if ([BitConverter]::ToString($sha.ComputeHash($installedApp)) -ne [BitConverter]::ToString($sha.ComputeHash($payload))) {
    throw 'payload copy is corrupted'
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
