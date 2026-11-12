# Installs lig on Windows, from a release:
#
#   irm https://github.com/KevenGoncalves/lig/releases/latest/download/install.ps1 | iex
#
# Set LIG_INSTALL_DIR to install elsewhere than %LOCALAPPDATA%\Programs\lig, or LIG_VERSION
# for another release.
$ErrorActionPreference = 'Stop'
$version = if ($env:LIG_VERSION) { $env:LIG_VERSION } else { '@VERSION@' }
if ($version.StartsWith('@')) { throw "lig: this copy of the script has no version; run the one from a release, or set LIG_VERSION" }
$arm = $env:PROCESSOR_ARCHITECTURE -eq 'ARM64' -or $env:PROCESSOR_ARCHITEW6432 -eq 'ARM64'
$name = "lig-$version-$(if ($arm) { 'aarch64' } else { 'x86_64' })-pc-windows-msvc"
$base = "https://github.com/KevenGoncalves/lig/releases/download/v$version"
$dir = if ($env:LIG_INSTALL_DIR) { $env:LIG_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\lig' }
$work = Join-Path ([IO.Path]::GetTempPath()) "lig-install-$PID"
New-Item -ItemType Directory -Force $work | Out-Null
try {
    Write-Host "lig: downloading $name.zip"
    $zip = Join-Path $work "$name.zip"
    Invoke-WebRequest "$base/$name.zip" -OutFile $zip -UseBasicParsing
    Invoke-WebRequest "$base/$name.zip.sha256" -OutFile "$zip.sha256" -UseBasicParsing
    $expected = (Get-Content "$zip.sha256" -Raw).Trim().Split()[0]
    if ((Get-FileHash $zip -Algorithm SHA256).Hash -ne $expected) { throw "lig: the download doesn't match its checksum" }
    Expand-Archive $zip $work -Force
    New-Item -ItemType Directory -Force $dir | Out-Null
    $exe = Join-Path $dir 'lig.exe'
    # A running lig.exe can't be replaced, but it can be renamed.
    if (Test-Path $exe) { Move-Item $exe "$exe.old" -Force }
    Copy-Item (Join-Path $work "$name\lig.exe") $exe
} finally {
    Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
}
$path = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($path -split ';') -notcontains $dir) {
    [Environment]::SetEnvironmentVariable('Path', $(if ($path) { "$path;$dir" } else { $dir }), 'User')
    $env:Path += ";$dir"
    Write-Host "lig: added $dir to your PATH"
}
Write-Host "lig: installed $exe"
Write-Host "lig: run 'lig install' to turn on the popup in PowerShell, or 'lig install cmd' for a cmd profile in Windows Terminal."
