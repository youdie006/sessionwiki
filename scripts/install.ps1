# sessionwiki installer for Windows (PowerShell).
# Downloads the prebuilt binary for the latest release and installs it to
# %LOCALAPPDATA%\Programs\sessionwiki (override with $env:SESSIONWIKI_BIN_DIR).
# Usage (PowerShell):
#   irm https://raw.githubusercontent.com/youdie006/sessionwiki/main/scripts/install.ps1 | iex
#
# Note: WSL users are on Linux - use scripts/install.sh instead (it picks the
# Linux binary). This script is for native Windows PowerShell.
$ErrorActionPreference = "Stop"

$repo = "youdie006/sessionwiki"
$arch = $env:PROCESSOR_ARCHITECTURE
if ($arch -ne "AMD64") {
    Write-Error "unsupported Windows arch: $arch (only x86_64 prebuilt binaries are published; build with 'cargo install sessionwiki')"
}
$target = "x86_64-pc-windows-msvc"
$binDir = if ($env:SESSIONWIKI_BIN_DIR) { $env:SESSIONWIKI_BIN_DIR } else { Join-Path $env:LOCALAPPDATA "Programs\sessionwiki" }

$tag = (Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest").tag_name
if (-not $tag) { Write-Error "could not determine the latest release tag" }

$asset = "sessionwiki-$tag-$target.zip"
$base = "https://github.com/$repo/releases/download/$tag"
$tmp = Join-Path $env:TEMP ("sessionwiki-" + [System.Guid]::NewGuid().ToString())
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
try {
    $zip = Join-Path $tmp $asset
    Write-Host "downloading $asset ..."
    Invoke-WebRequest "$base/$asset" -OutFile $zip

    # Verify the published checksum. As with the shell installer this only
    # catches a corrupted download, not a malicious release (the hash is served
    # from the same release); real integrity is HTTPS + the maintainer's account.
    try {
        $shaFile = "$zip.sha256"
        Invoke-WebRequest "$base/$asset.sha256" -OutFile $shaFile
        $expected = ((Get-Content $shaFile) -split '\s+')[0].ToLower()
        $actual = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
        if ($expected -and ($actual -ne $expected)) {
            Write-Error "checksum mismatch for $asset (expected $expected, got $actual)"
        }
    } catch {
        Write-Warning "could not verify checksum for ${asset}: $_"
    }

    Expand-Archive -Path $zip -DestinationPath $tmp -Force
    New-Item -ItemType Directory -Force -Path $binDir | Out-Null
    Copy-Item (Join-Path $tmp "sessionwiki-$tag-$target\sessionwiki.exe") (Join-Path $binDir "sessionwiki.exe") -Force
    Write-Host "installed sessionwiki $tag to $binDir\sessionwiki.exe"

    # Add to the user PATH if missing (takes effect in new shells).
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($userPath -notlike "*$binDir*") {
        [Environment]::SetEnvironmentVariable("Path", "$userPath;$binDir", "User")
        Write-Host "added $binDir to your user PATH - open a new terminal to use 'sessionwiki'"
    }
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
