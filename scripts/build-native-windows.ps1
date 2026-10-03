# Build the native xcb (Excalibur) CLI for Windows x86_64 and emit
# xcb-<version>-windows-x86_64.zip plus a SHA-256 checksum, the Windows
# counterpart of scripts/build-native.sh. The zip holds exactly one entry,
# xcb.exe; scripts/install.ps1 refuses anything else. The binary is not
# Authenticode-signed.
#
# XCB_CARGO_PROFILE picks the build: `release` (the default, and the only
# profile a release uses), `ci`, or `dev` (reuses the binary `cargo test`
# already built, so CI can prove the packaging without another compile).
# XCB_SKIP_BUILD=1 packages the existing binary without running cargo.

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'

function Fail([string] $Message) {
  Write-Error "error: $Message"
  exit 1
}

$root = Split-Path -Parent $PSScriptRoot
$cargo = if ($env:CARGO) { $env:CARGO } else { 'cargo' }
$profileName = if ($env:XCB_CARGO_PROFILE) { $env:XCB_CARGO_PROFILE } else { 'release' }
$targetDirectory = switch ($profileName) {
  'release' { 'release' }
  'ci' { 'ci' }
  'dev' { 'debug' }
  default { Fail 'XCB_CARGO_PROFILE must be release, ci, or dev' }
}

if ($env:XCB_VERSION) {
  $version = $env:XCB_VERSION
  if ($version.StartsWith('v')) { $version = $version.Substring(1) }
} else {
  $line = Select-String -LiteralPath (Join-Path $root 'Cargo.toml') -Pattern '^version = "(.*)"' | Select-Object -First 1
  if ($null -eq $line) { Fail 'could not read version from workspace Cargo.toml' }
  $version = $line.Matches[0].Groups[1].Value
}
if ($version -cnotmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') {
  Fail "version must be a stable semantic version (got '$version')"
}

$architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
if ($architecture -ne [System.Runtime.InteropServices.Architecture]::X64) {
  Fail "unsupported architecture: $architecture"
}

Push-Location $root
try {
  if ($env:XCB_SKIP_BUILD -ne '1') {
    & $cargo build --profile $profileName --locked -p xcb-cli
    if ($LASTEXITCODE -ne 0) { Fail 'cargo build failed' }
  }
} finally {
  Pop-Location
}

$targetRoot = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' }
$binary = Join-Path $targetRoot "$targetDirectory\xcb.exe"
if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) { Fail "expected $binary after build" }
$reported = ((& $binary --version) | Out-String).Trim()
if ($LASTEXITCODE -ne 0 -or $reported -cne "xcb $version") {
  Fail "native binary reports '$reported', expected 'xcb $version'"
}

$artifacts = Join-Path $root 'artifacts'
New-Item -ItemType Directory -Force -Path $artifacts | Out-Null
$name = "xcb-$version-windows-x86_64"
$archive = Join-Path $artifacts "$name.zip"
$checksum = "$archive.sha256"
Remove-Item -LiteralPath $archive, $checksum -Force -ErrorAction SilentlyContinue

# One entry named xcb.exe, written from the binary's bytes: no directory
# entries and no other members.
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [System.IO.Compression.ZipFile]::Open($archive, 'Create')
try {
  [System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $binary, 'xcb.exe', 'Optimal') | Out-Null
} finally {
  $zip.Dispose()
}
$digest = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
[System.IO.File]::WriteAllText($checksum, "$digest`n", (New-Object System.Text.UTF8Encoding $false))

# Re-admit the packaged bytes exactly as the installer will: matching
# checksum, one entry named xcb.exe, and an extracted binary that reports
# this version.
$recorded = ([System.IO.File]::ReadAllText($checksum) -replace '\s', '')
if ($recorded -cnotmatch '^[0-9a-f]{64}$' -or $recorded -ne (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()) {
  Fail "checksum mismatch for $name.zip"
}
$work = Join-Path ([System.IO.Path]::GetTempPath()) ("xcb-admit-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null
try {
  $zip = [System.IO.Compression.ZipFile]::OpenRead($archive)
  try {
    if ($zip.Entries.Count -ne 1 -or $zip.Entries[0].FullName -cne 'xcb.exe') {
      Fail "$name.zip must contain exactly one entry, xcb.exe"
    }
    $extracted = Join-Path $work 'xcb.exe'
    [System.IO.Compression.ZipFileExtensions]::ExtractToFile($zip.Entries[0], $extracted)
  } finally {
    $zip.Dispose()
  }
  $admitted = ((& $extracted --version) | Out-String).Trim()
  if ($LASTEXITCODE -ne 0 -or $admitted -cne "xcb $version") {
    Fail "extracted binary reports '$admitted', expected 'xcb $version'"
  }
} finally {
  Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Output "ok: $name.zip sha256=$digest reports '$reported'"
Write-Output "zip=$archive"
Write-Output "sha256=$checksum"
