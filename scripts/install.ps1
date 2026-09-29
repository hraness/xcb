# Install xcb for Windows x86_64 from an exact GitHub Release.
#
#   irm https://xcb.sh/install.ps1 | iex
#   $env:XCB_VERSION = "<version>"; irm https://xcb.sh/install.ps1 | iex   # one exact version
#
# This script downloads xcb-<version>-windows-x86_64.zip from the GitHub
# Release, checks it against its .sha256 file, and installs
# %LOCALAPPDATA%\Programs\xcb\bin\xcb.exe for this user. Nothing runs as
# administrator. The release is not Authenticode-signed yet, so Windows may
# show a SmartScreen prompt the first time xcb.exe runs.
#
# On Windows xcb runs everything except the providers: running or signing in
# to Claude Code, Codex, or Devin needs the Linux build inside WSL2.
#
# Options (environment): XCB_VERSION, XCB_INSTALL_PREFIX (default
# %LOCALAPPDATA%\Programs\xcb), XCB_ADD_PATH=yes (add the bin directory to
# this user's PATH). `xcb upgrade` runs the copy of this script kept in
# <prefix>\share\xcb\install.ps1.
# Source: https://github.com/hraness/xcb/blob/main/scripts/install.ps1
#
# Everything is inside a script block, so a partial download runs nothing.

& {
  param([string] $Self)
  Set-StrictMode -Version 3.0
  $ErrorActionPreference = 'Stop'
  $ProgressPreference = 'SilentlyContinue'

  function Fail([string] $Message) {
    throw "xcb install: $Message"
  }

  # xcb.sh renders this from site/published-release.json; never type it.
  $defaultVersion = '@XCB_RELEASE_VERSION@'
  $repository = if ($env:XCB_GITHUB) { $env:XCB_GITHUB } else { 'hraness/xcb' }
  $guide = 'https://xcb.sh/install'

  $version = if ($env:XCB_VERSION) { $env:XCB_VERSION } else { $defaultVersion }
  if ($version.StartsWith('v')) { $version = $version.Substring(1) }
  if ($version -cnotmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') {
    Fail "XCB_VERSION must be an exact release version, MAJOR.MINOR.PATCH (got '$version')"
  }

  # The one Windows host with a release archive.
  $arch = $env:PROCESSOR_ARCHITECTURE
  if ($env:PROCESSOR_ARCHITEW6432) { $arch = $env:PROCESSOR_ARCHITEW6432 }
  if ($arch -ne 'AMD64') {
    Fail "there is no release build for Windows $arch yet; build from source: $guide#source"
  }
  $platform = 'windows-x86_64'

  if (-not $env:XCB_INSTALL_PREFIX -and -not $env:LOCALAPPDATA) {
    Fail 'LOCALAPPDATA is not set; set XCB_INSTALL_PREFIX to choose where xcb goes'
  }
  $prefix = if ($env:XCB_INSTALL_PREFIX) { $env:XCB_INSTALL_PREFIX } else { Join-Path $env:LOCALAPPDATA 'Programs\xcb' }
  if (-not [System.IO.Path]::IsPathRooted($prefix)) { Fail 'XCB_INSTALL_PREFIX must be an absolute path' }
  $prefix = [System.IO.Path]::GetFullPath($prefix)
  $binDir = Join-Path $prefix 'bin'
  $shareDir = Join-Path $prefix 'share\xcb'

  # Windows PowerShell 5.1 still offers TLS 1.0 by default.
  [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

  function Test-Real([string] $Path) {
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
    if ($null -eq $item) { return $true }
    return -not ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)
  }
  function Get-Sha256([string] $Path) {
    (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
  }

  foreach ($directory in @($prefix, $binDir, (Join-Path $prefix 'share'), $shareDir)) {
    if (-not (Test-Real $directory)) { Fail "$directory must not be a symlink or junction" }
    New-Item -ItemType Directory -Force -Path $directory | Out-Null
  }

  # One installer at a time. The lock is an exclusive open, so a killed
  # installer never leaves a stale lock behind.
  $lockPath = Join-Path $binDir '.xcb-install-lock'
  try {
    $lock = [System.IO.File]::Open($lockPath, 'OpenOrCreate', 'ReadWrite', 'None')
  } catch {
    Fail "another installation owns $lockPath; retry when it has finished"
  }
  $stage = Join-Path $binDir (".xcb-install-" + [Guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory -Path $stage | Out-Null
  try {
    # A replaced xcb.exe that was running during the last install could not
    # be removed then; it can be now, unless it is still running.
    Get-ChildItem -LiteralPath $binDir -Filter 'xcb.exe.old-*' -Force -ErrorAction SilentlyContinue |
      ForEach-Object { Remove-Item -LiteralPath $_.FullName -Force -ErrorAction SilentlyContinue }

    $asset = "xcb-$version-$platform.zip"
    $baseUrl = "https://github.com/$repository/releases/download/v$version"
    # CI serves a locally built archive from loopback; nothing else may
    # replace the GitHub Release as the source.
    if ($env:XCB_RELEASE_BASE_URL) {
      if ($env:XCB_RELEASE_BASE_URL -cnotmatch '^http://127\.0\.0\.1:[0-9]{1,5}$') {
        Fail 'XCB_RELEASE_BASE_URL may only name a loopback test server'
      }
      $baseUrl = $env:XCB_RELEASE_BASE_URL
    }
    $archive = Join-Path $stage 'archive.zip'
    Write-Host "Installing xcb $version for Windows x86_64"
    try {
      Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/$asset" -OutFile $archive
    } catch {
      Fail "download failed for $asset; v$version may have no Windows build (see https://github.com/$repository/releases/tag/v$version)"
    }
    try {
      $recorded = (Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/$asset.sha256").Content
    } catch {
      Fail "download failed for $asset.sha256"
    }
    if ($recorded -is [byte[]]) { $recorded = [Text.Encoding]::ASCII.GetString($recorded) }
    $expected = ($recorded -replace '\s', '')
    if ($expected -cnotmatch '^[0-9a-f]{64}$') { Fail 'invalid release checksum' }
    if ((Get-Sha256 $archive) -ne $expected) { Fail "checksum mismatch for $asset" }

    # Admit exactly one regular entry named xcb.exe, then copy its bytes to
    # our own path: archive paths and attributes never create objects.
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $candidate = Join-Path $stage 'xcb.exe'
    $zip = [System.IO.Compression.ZipFile]::OpenRead($archive)
    try {
      if ($zip.Entries.Count -ne 1 -or $zip.Entries[0].FullName -cne 'xcb.exe') {
        Fail 'archive must contain only xcb.exe'
      }
      $source = $zip.Entries[0].Open()
      try {
        $target = [System.IO.File]::Open($candidate, 'CreateNew', 'Write', 'None')
        try { $source.CopyTo($target) } finally { $target.Dispose() }
      } finally { $source.Dispose() }
    } finally { $zip.Dispose() }

    $candidateDigest = Get-Sha256 $candidate
    $reported = (& $candidate --version) | Out-String
    $reported = $reported.Trim()
    if ($LASTEXITCODE -ne 0) { Fail 'candidate --version failed' }
    if ($reported -cne "xcb $version") { Fail "candidate reports '$reported', expected 'xcb $version'" }
    if ((Get-Sha256 $candidate) -ne $candidateDigest) { Fail 'candidate changed during verification' }

    # A running xcb.exe cannot be overwritten or deleted, but it can be
    # renamed. Keep the previous binary as xcb.previous.<sha256>.exe, move
    # the new one into place, and leave a still-running copy for the next
    # install to remove.
    $destination = Join-Path $binDir 'xcb.exe'
    if (-not (Test-Real $destination)) { Fail "$destination must be a regular file" }
    if (Test-Path -LiteralPath $destination) {
      $previousDigest = Get-Sha256 $destination
      $backup = Join-Path $binDir "xcb.previous.$previousDigest.exe"
      if (Test-Path -LiteralPath $backup) {
        if (-not (Test-Real $backup) -or (Get-Sha256 $backup) -ne $previousDigest) {
          Fail "existing backup is unsafe or has changed: $backup"
        }
        $aside = Join-Path $binDir ("xcb.exe.old-" + [Guid]::NewGuid().ToString('N'))
        Move-Item -LiteralPath $destination -Destination $aside
        Remove-Item -LiteralPath $aside -Force -ErrorAction SilentlyContinue
      } else {
        Move-Item -LiteralPath $destination -Destination $backup
      }
      Write-Host "Previous binary preserved at $backup"
      # Keep this backup plus the two most recent others.
      Get-ChildItem -LiteralPath $binDir -Filter 'xcb.previous.*.exe' -Force |
        Where-Object { $_.Name -cmatch '^xcb\.previous\.[0-9a-f]{64}\.exe$' -and $_.FullName -ne $backup } |
        Sort-Object LastWriteTime -Descending |
        Select-Object -Skip 2 |
        ForEach-Object { Remove-Item -LiteralPath $_.FullName -Force -ErrorAction SilentlyContinue }
    }
    Move-Item -LiteralPath $candidate -Destination $destination
    if ((Get-Sha256 $destination) -ne $candidateDigest) { Fail 'installed binary changed' }

    # Keep this installer beside the binary so `xcb upgrade` can run the same
    # checks. When it came through `irm | iex` there is no file to copy, so
    # fetch the exact installer from the release tag.
    $helper = Join-Path $shareDir 'install.ps1'
    $stagedHelper = Join-Path $stage 'install.ps1'
    if ($Self) {
      Copy-Item -LiteralPath $Self -Destination $stagedHelper
    } else {
      try {
        Invoke-WebRequest -UseBasicParsing -Uri "https://raw.githubusercontent.com/$repository/v$version/scripts/install.ps1" -OutFile $stagedHelper
      } catch {
        Fail "could not download the installer for v$version"
      }
    }
    if (-not (Select-String -LiteralPath $stagedHelper -Pattern '^# Install xcb for Windows' -Quiet)) {
      Fail 'the downloaded installer is not the xcb installer'
    }
    Move-Item -LiteralPath $stagedHelper -Destination $helper -Force

    $manifest = [ordered]@{
      version        = 1
      installMethod  = 'release'
      channel        = 'stable'
      versionString  = $version
      prefix         = $prefix
      helperPath     = $helper
      sourceRoot     = ''
      binaryPath     = $destination
    } | ConvertTo-Json -Compress
    $stagedManifest = Join-Path $stage 'install.json'
    [System.IO.File]::WriteAllText($stagedManifest, $manifest + "`n", (New-Object System.Text.UTF8Encoding $false))
    Move-Item -LiteralPath $stagedManifest -Destination (Join-Path $shareDir 'install.json') -Force

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $entries = @()
    if ($userPath) { $entries = $userPath.Split(';') | Where-Object { $_ } }
    if ($entries -notcontains $binDir) {
      if ($env:XCB_ADD_PATH -eq 'yes') {
        [Environment]::SetEnvironmentVariable('Path', (($entries + $binDir) -join ';'), 'User')
        Write-Host "Added $binDir to your user PATH; open a new terminal to use it."
      } elseif ($env:XCB_ADD_PATH -ne 'no') {
        Write-Host "$binDir is not on PATH. Add it with:"
        Write-Host "  [Environment]::SetEnvironmentVariable('Path', [Environment]::GetEnvironmentVariable('Path', 'User') + ';$binDir', 'User')"
      }
    }

    Write-Host "Installed $destination ($candidateDigest)"
    Write-Host $reported
    Write-Host 'Claude Code, Codex, and Devin run only in the Linux build of xcb; on Windows install it inside WSL2.'
    Write-Host "Guide: $guide"
  } finally {
    Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue
    $lock.Dispose()
    Remove-Item -LiteralPath $lockPath -Force -ErrorAction SilentlyContinue
  }
} $PSCommandPath
