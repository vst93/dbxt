# dbxt Installer for Windows
# Usage:
#   irm https://raw.githubusercontent.com/vst93/dbxt/master/cmd/install.ps1 | iex
#
# Options (via environment variables):
#   $env:DBXT_INSTALL_DIR=path   Custom install directory
#   $env:DBXT_FORCE_INSTALL=1    Reinstall / upgrade even if up to date
#   $env:DBXT_SKIP_GITHUB=1      Skip GitHub direct download, use mirrors only
#   $env:DBXT_LANG=zh            Set language (en/zh)
#   $env:DBXT_PREVIEW=1          Install the latest pre-release version

$ErrorActionPreference = "Stop"

# Set console encoding to UTF-8
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$OutputEncoding = [System.Text.Encoding]::UTF8

# UTF-8 helper: Chinese strings are carried as byte arrays so the script
# survives `irm | iex` and CDN responses that do not declare a charset.
function U($bytes) {
    [System.Text.Encoding]::UTF8.GetString([byte[]]$bytes)
}

# Translation function: t("English", "中文")
function t($en, $zh) {
    if ($Lang -eq "zh") { return $zh } else { return $en }
}

$Z = @{
    LANG_NAME        = U(228,184,173,230,150,135)
    VERSION          = U(231,137,136,230,156,172)
    PLATFORM         = U(229,185,179,229,143,176)
    DOWNLOAD         = U(228,184,139,232,189,189)
    INSTALL          = U(229,174,137,232,163,133)
    VERIFY           = U(230,160,161,233,170,140)
    DONE             = U(229,174,137,232,163,133,229,174,140,230,136,144,33)
    CANCELLED        = U(229,183,178,229,143,150,230,182,136)
    FAILED           = U(229,164,177,232,180,165)
    LATEST_VER       = U(230,156,128,230,150,176,231,137,136,230,156,172,58,32)
    PREVIEW_VER      = U(233,162,132,232,167,136,231,137,136,230,156,172,58,32)
    DL_FAILED        = U(228,184,139,232,189,189,229,164,177,232,180,165)
    ALL_DL_FAILED    = U(230,137,128,230,156,137,228,184,139,232,189,189,229,157,135,229,164,177,232,180,165)
    GITHUB_FAIL      = U(71,105,116,72,117,98,32,229,164,177,232,180,165,44,229,176,157,232,175,149,233,149,156,229,131,143,46,46,46)
    TRY_MIRROR       = U(229,176,157,232,175,149,233,149,156,229,131,143,58,32)
    DOWNLOADED       = U(229,183,178,228,184,139,232,189,189)
    SHA_OK           = U(230,160,161,233,170,140,32,79,75)
    SHA_MISMATCH     = U(230,160,161,233,170,140,228,184,141,229,140,185,233,133,141)
    NO_CHECKSUM      = U(230,151,160,230,179,149,232,142,183,229,143,150,230,160,161,233,170,140,228,191,161,230,129,175,239,188,140,230,150,135,228,187,182,229,174,140,230,149,180,230,128,167,230,156,170,231,159,165)
    PREVIEW_NONE     = U(230,156,170,230,137,190,229,136,176,233,162,132,232,167,136,231,137,136,230,156,172)
    CONTINUE         = U(230,152,175,229,144,166,231,187,167,231,187,173,63)
    ADDED_PATH       = U(229,183,178,230,183,187,229,138,160,229,136,176,32,80,65,84,72,40,233,156,128,233,135,141,229,144,175,231,187,136,231,171,175,231,148,159,230,149,136,41)
    EXTRACTING       = U(232,167,163,229,142,139,228,184,173)
    NO_BINARY        = U(229,142,139,231,188,169,229,140,133,228,184,173,230,156,170,230,137,190,229,136,176,231,168,139,229,186,143)
    INSTALLED        = U(229,183,178,229,174,137,232,163,133)
    FORCE_INSTALL    = U(229,188,186,229,136,182,229,174,137,232,163,133)
    TRYING           = U(229,176,157,232,175,149)
    TRYING_DIRECT    = U(229,176,157,232,175,149,231,155,180,232,191,158,46,46,46)
    CONNECTING       = U(232,191,158,230,142,165,228,184,173,46,46,46)
    UP_TO_DATE       = U(229,183,178,230,152,175,230,156,128,230,150,176,231,137,136,230,156,172)
    UPGRADING        = U(229,141,135,231,186,167)
    INSTALL_TO       = U(229,174,137,232,163,133,229,136,176)
    SHA_TOOL_MISSING = U(230,151,160,32,83,72,65,50,53,54,32,229,183,165,229,133,183,239,188,140,232,183,179,232,191,135)
    UNSUPPORTED_ARCH = U(228,184,141,230,148,175,230,140,129,231,154,132,230,158,182,230,158,132)
    ARM64_NOTE       = U(87,105,110,100,111,119,115,32,65,82,77,54,52,239,188,154,230,154,130,230,151,160,229,142,159,231,148,159,230,158,132,229,187,186,239,188,140,229,176,134,229,174,137,232,163,133,32,120,54,52,32,231,137,136,230,156,172,239,188,136,231,148,177,231,179,187,231,187,159,230,168,161,230,139,159,232,191,144,232,161,140,239,188,137)
}

$REPO_OWNER = "vst93"
$REPO_NAME = "dbxt"
$BINARY_NAME = "dbxt"
$REPO_URL = "https://github.com/$REPO_OWNER/$REPO_NAME"
$API_URL = "https://api.github.com/repos/$REPO_OWNER/$REPO_NAME/releases/latest"

$GITHUB_MIRRORS = @(
    "https://ghfast.top",
    "https://mirror.ghproxy.com",
    "https://gh-proxy.com",
    "https://gh-proxy.net"
)

$InstallDir = if ($env:DBXT_INSTALL_DIR) { $env:DBXT_INSTALL_DIR } elseif ($env:INSTALL_DIR) { $env:INSTALL_DIR } else { $null }
$Force = ($env:DBXT_FORCE_INSTALL -eq '1') -or ($env:FORCE_INSTALL -eq '1')
$SkipGitHub = ($env:DBXT_SKIP_GITHUB -eq '1') -or ($env:SKIP_GITHUB -eq '1')
$Preview = $env:DBXT_PREVIEW -eq '1'
$Lang = $env:DBXT_LANG

# Timeout settings (seconds)
$CONNECT_TIMEOUT = 10
$DOWNLOAD_TIMEOUT = 300

# ──────────────────────────────────────────────────────────────────────────────
# Language
# ──────────────────────────────────────────────────────────────────────────────

function Select-Language {
    if ($Lang -match "^(en|zh)$") { return }
    # Follow dbxt itself: an explicit DBXT_LANG wins, then the locale.
    $locale = if ($env:LC_ALL) { $env:LC_ALL } else { $env:LANG }
    if ($locale -match "^(zh|cn)") { $script:Lang = "zh"; return }
    if ($locale -match "^en") { $script:Lang = "en"; return }
    # Non-interactive (piped, CI): never block on Read-Host; default to English,
    # exactly like the bash installer does.
    if ([Console]::IsInputRedirected) { $script:Lang = "en"; return }

    Write-Host "  [1] English  [2] $($Z.LANG_NAME) (default: 1): " -NoNewline
    $choice = Read-Host
    switch ($choice) {
        "2" { $script:Lang = "zh" }
        "cn" { $script:Lang = "zh" }
        "zh" { $script:Lang = "zh" }
        default { $script:Lang = "en" }
    }
}

# ──────────────────────────────────────────────────────────────────────────────
# Logging
# ──────────────────────────────────────────────────────────────────────────────

function Log-Info($msg) { Write-Host "  [OK] $msg" -ForegroundColor Green }
function Log-Warn($msg) { Write-Host "  [!] $msg" -ForegroundColor Yellow }
function Log-Error($msg) { Write-Host "  [X] $msg" -ForegroundColor Red }
function Log-Step($msg) { Write-Host "  >> $msg" -ForegroundColor Cyan }

# ──────────────────────────────────────────────────────────────────────────────
# Platform
# ──────────────────────────────────────────────────────────────────────────────

function Get-Platform {
    $arch = $env:PROCESSOR_ARCHITECTURE
    switch ($arch) {
        "AMD64" { return "amd64" }
        "ARM64" {
            # Only an x64 build is published; Windows runs it under emulation.
            Log-Warn "$($Z.ARM64_NOTE)"
            return "amd64"
        }
        default {
            Log-Error "$(t 'Unsupported architecture' $Z.UNSUPPORTED_ARCH): $arch"
            exit 1
        }
    }
}

# ──────────────────────────────────────────────────────────────────────────────
# Version
# ──────────────────────────────────────────────────────────────────────────────

function Get-LatestVersion {
    $latest_asset = "$BINARY_NAME-windows-amd64.zip"
    $latest_url = "$REPO_URL/releases/latest/download/$latest_asset"

    Log-Info "$(t 'Trying' $Z.TRYING) GitHub..."
    $try_urls = @($latest_url)
    foreach ($mirror in $GITHUB_MIRRORS) { $try_urls += "$mirror/$latest_url" }

    foreach ($url in $try_urls) {
        try {
            $request = [System.Net.WebRequest]::Create($url)
            $request.Method = "HEAD"
            $request.AllowAutoRedirect = $false
            $request.Timeout = 15000
            $response = $request.GetResponse()
            $location = $response.Headers["Location"]
            $response.Close()

            if ($location -and $location -match '/download/v?([0-9]+\.[0-9]+[0-9.]*)') {
                $version = $matches[1] -replace '^v', ''
                Log-Info "$(t 'Latest version: ' $Z.LATEST_VER)$version"
                return $version
            }
        } catch {
            continue
        }
    }

    Log-Info "Trying API..."
    try {
        $response = Invoke-WebRequest -Uri $API_URL -UseBasicParsing -TimeoutSec 15
        $json = $response.Content | ConvertFrom-Json
        $version = $json.tag_name -replace '^v', ''
        Log-Info "$(t 'Latest version: ' $Z.LATEST_VER)$version"
        return $version
    } catch {
        Log-Error "$(t 'Failed' $Z.FAILED)"
        exit 1
    }
}

function Get-PreviewVersion {
    Log-Info "Trying API for pre-release..."
    try {
        $releasesUrl = "https://api.github.com/repos/$REPO_OWNER/$REPO_NAME/releases?per_page=10"
        $response = Invoke-WebRequest -Uri $releasesUrl -UseBasicParsing -TimeoutSec 15
        $json = $response.Content | ConvertFrom-Json
        $preview = $json | Where-Object { $_.prerelease -eq $true } | Select-Object -First 1
        if ($preview -and $preview.tag_name) {
            $version = $preview.tag_name
            Log-Info "$(t 'Preview version: ' $Z.PREVIEW_VER)$version"
            return $version
        }
        Log-Error "$(t 'No pre-release version found' $Z.PREVIEW_NONE)"
        exit 1
    } catch {
        Log-Error "$(t 'Failed' $Z.FAILED)"
        exit 1
    }
}

# ──────────────────────────────────────────────────────────────────────────────
# Download
# ──────────────────────────────────────────────────────────────────────────────

function Format-FileSize($bytes) {
    if ($bytes -ge 1MB) { return "{0:N1} MB" -f ($bytes / 1MB) }
    if ($bytes -ge 1KB) { return "{0:N1} KB" -f ($bytes / 1KB) }
    return "$bytes B"
}

function Download-File($url, $output) {
    $filename = Split-Path $url -Leaf
    Write-Host "  $filename" -ForegroundColor DarkGray
    Remove-Item -Path $output -Force -ErrorAction SilentlyContinue

    try {
        if (Get-Command curl.exe -ErrorAction SilentlyContinue) {
            # curl.exe ships with Windows 10 1803+ and streams large files well.
            & curl.exe -fSL --connect-timeout $CONNECT_TIMEOUT --max-time $DOWNLOAD_TIMEOUT --retry 2 -o $output $url 2>$null
            if ($LASTEXITCODE -ne 0) { throw "curl exited with $LASTEXITCODE" }
        } else {
            (New-Object System.Net.WebClient).DownloadFile($url, $output)
        }

        if ((Test-Path $output) -and ((Get-Item $output).Length -gt 0)) {
            $sizeText = Format-FileSize (Get-Item $output).Length
            Log-Info "$(t 'Downloaded' $Z.DOWNLOADED): $filename ($sizeText)"
            return $true
        }
    } catch {
        Log-Warn "Download failed: $($_.Exception.Message)"
    }
    return $false
}

function Download-WithMirrors($url, $output) {
    if (-not $SkipGitHub) {
        Log-Info "$(t 'Trying direct...' $Z.TRYING_DIRECT)"
        if (Download-File $url $output) { return $true }
    }

    foreach ($mirror in $GITHUB_MIRRORS) {
        $mirror_url = "$mirror/$url"
        Log-Warn "$(t 'Trying mirror: ' $Z.TRY_MIRROR)$mirror"
        if (Download-File $mirror_url $output) { return $true }
    }

    Log-Error "$(t 'All downloads failed' $Z.ALL_DL_FAILED)"
    return $false
}

# ──────────────────────────────────────────────────────────────────────────────
# Verification
# ──────────────────────────────────────────────────────────────────────────────

function Get-SHA256($file) {
    try {
        return (Get-FileHash -Path $file -Algorithm SHA256).Hash.ToLower()
    } catch {
        return $null
    }
}

function Get-RemoteSHA256($url) {
    $sha_file = Join-Path $env:TEMP "dbxt-sha256-$(Get-Random).txt"
    try {
        if (Download-WithMirrors "$url.sha256" $sha_file) {
            $content = Get-Content -Path $sha_file -Raw
            if ($content -match '([a-fA-F0-9]{64})') {
                return $matches[1].ToLower()
            }
        }
    } catch {} finally {
        Remove-Item -Path $sha_file -Force -ErrorAction SilentlyContinue
    }
    return $null
}

function Verify-SHA256($file, $expected) {
    $actual = Get-SHA256 $file
    if (-not $actual) {
        Log-Warn "$(t 'No SHA256 tool, skipping' $Z.SHA_TOOL_MISSING)"
        return $true
    }
    if ($actual -ne $expected) {
        Log-Error "$(t 'SHA256 mismatch' $Z.SHA_MISMATCH)"
        Log-Error "Expected: $expected"
        Log-Error "Actual:   $actual"
        return $false
    }
    Log-Info "$(t 'SHA256 OK' $Z.SHA_OK)"
    return $true
}

# ──────────────────────────────────────────────────────────────────────────────
# Installation
# ──────────────────────────────────────────────────────────────────────────────

function Get-DefaultInstallDir {
    if ($InstallDir) { return $InstallDir }
    # Mirror the Unix installer's `~/.local/bin`.
    return Join-Path $env:USERPROFILE ".local\bin"
}

function Add-ToPath($dir) {
    $currentPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if (-not $currentPath) { $currentPath = "" }
    if (($currentPath -split ";") -contains $dir) { return }

    # A locked-down machine can refuse the registry write; the install itself has
    # already succeeded, so this must never be fatal.
    try {
        [Environment]::SetEnvironmentVariable("Path", ($currentPath.TrimEnd(';') + ";" + $dir), "User")
        $env:Path = "$env:Path;$dir"
        Log-Warn "$(t 'Added to PATH (restart terminal to take effect)' $Z.ADDED_PATH)"
    } catch {
        Log-Warn "Could not update PATH automatically — add $dir manually."
    }
}

# The installed version, or $null. `--version` is bounded by a 5s timeout so an
# older binary without the flag cannot leave the installer hanging.
function Get-InstalledVersion($exe) {
    if (-not (Test-Path $exe)) { return $null }
    $outFile = Join-Path $env:TEMP "dbxt-version-$(Get-Random).txt"
    $errFile = "$outFile.err"
    try {
        $p = Start-Process -FilePath $exe -ArgumentList "--version" -NoNewWindow -PassThru `
            -RedirectStandardOutput $outFile -RedirectStandardError $errFile
        if (-not $p.WaitForExit(5000)) {
            $p.Kill()
            return $null
        }
        $out = Get-Content -Path $outFile -Raw -ErrorAction SilentlyContinue
        if ($out -and ($out -match '([0-9]+\.[0-9]+\.[0-9]+[0-9A-Za-z.+-]*)')) {
            return $matches[1]
        }
    } catch {
        return $null
    } finally {
        Remove-Item -Path $outFile, $errFile -Force -ErrorAction SilentlyContinue
    }
    return $null
}

function Install-Binary($zipFile, $installDir) {
    $extractDir = Join-Path $env:TEMP "dbxt-extract-$(Get-Random)"

    try {
        Log-Info "$(t 'Extracting' $Z.EXTRACTING)"
        Expand-Archive -Path $zipFile -DestinationPath $extractDir -Force

        $exePath = Join-Path $extractDir "$BINARY_NAME.exe"
        if (-not (Test-Path $exePath)) {
            Log-Error "$(t 'Binary not found in archive' $Z.NO_BINARY)"
            exit 1
        }

        if (-not (Test-Path $installDir)) {
            New-Item -ItemType Directory -Path $installDir -Force | Out-Null
        }

        $destPath = Join-Path $installDir "$BINARY_NAME.exe"
        Copy-Item -Path $exePath -Destination $destPath -Force

        Log-Info "$(t 'Installed' $Z.INSTALLED): $destPath"
        # Best effort: report the version. A launch failure (missing VC++ runtime,
        # SmartScreen, an AV lock on the freshly written exe) must not abort an
        # install that already succeeded.
        try { & $destPath --version 2>$null } catch {
            Log-Warn "Could not run the installed binary: $($_.Exception.Message)"
        }

        Add-ToPath $installDir
    } finally {
        Remove-Item -Path $extractDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# ──────────────────────────────────────────────────────────────────────────────
# Main
# ──────────────────────────────────────────────────────────────────────────────

function Main {
    Select-Language

    Write-Host ""
    Write-Host "  >> dbxt Installer" -ForegroundColor Cyan
    Write-Host ""

    Log-Step "$(t 'Version' $Z.VERSION)"
    if ($Preview) {
        $version = Get-PreviewVersion
    } else {
        $version = Get-LatestVersion
    }

    Log-Step "$(t 'Platform' $Z.PLATFORM)"
    $arch = Get-Platform
    Log-Info "Platform: windows-$arch"

    Log-Step "$(t 'Install' $Z.INSTALL)"
    $installPath = Get-DefaultInstallDir
    Log-Info "$(t 'Install to' $Z.INSTALL_TO): $installPath"

    # install / upgrade / already up to date
    $current = Get-InstalledVersion (Join-Path $installPath "$BINARY_NAME.exe")
    if ($current -and ($current -eq $version) -and (-not $Force)) {
        Log-Info "$(t 'Already up to date' $Z.UP_TO_DATE): $BINARY_NAME $current"
        Write-Host ""
        Write-Host "  [OK] $(t 'Done!' $Z.DONE)" -ForegroundColor Green
        Write-Host ""
        return
    }
    if ($current) {
        Log-Info "$(t 'Upgrading' $Z.UPGRADING) $current -> $version"
    } else {
        Log-Info "Installing $BINARY_NAME $version"
    }

    Log-Step "$(t 'Download' $Z.DOWNLOAD)"
    $filename = "$BINARY_NAME-windows-$arch.zip"
    $downloadUrl = "$REPO_URL/releases/download/$version/$filename"
    Write-Host "  URL: $downloadUrl" -ForegroundColor DarkGray

    $tempDir = Join-Path $env:TEMP "dbxt-install-$(Get-Random)"
    New-Item -ItemType Directory -Path $tempDir -Force | Out-Null
    $zipFile = Join-Path $tempDir $filename

    if (-not (Download-WithMirrors $downloadUrl $zipFile)) {
        Log-Error "$(t 'Download failed' $Z.DL_FAILED)"
        exit 1
    }

    Log-Step "$(t 'Verify' $Z.VERIFY)"
    $expectedSha = Get-RemoteSHA256 $downloadUrl
    if ($expectedSha) {
        if (-not (Verify-SHA256 $zipFile $expectedSha)) {
            if (-not $Force) {
                $reply = Read-Host "$(t 'SHA256 mismatch' $Z.SHA_MISMATCH). $(t 'Continue?' $Z.CONTINUE) (y/N)"
                if ($reply -notmatch "^[yY]") {
                    Log-Error "$(t 'Cancelled' $Z.CANCELLED)"
                    exit 1
                }
            } else {
                Log-Warn "$(t 'Force install' $Z.FORCE_INSTALL)"
            }
        }
    } else {
        Log-Warn "$(t 'Cannot fetch checksum, file integrity unknown' $Z.NO_CHECKSUM)"
        if (-not $Force) {
            $reply = Read-Host "$(t 'Continue?' $Z.CONTINUE) (y/N)"
            if ($reply -notmatch "^[yY]") {
                Log-Error "$(t 'Cancelled' $Z.CANCELLED)"
                exit 1
            }
        }
    }

    Log-Step "$(t 'Install' $Z.INSTALL)"
    Install-Binary $zipFile $installPath

    Remove-Item -Path $tempDir -Recurse -Force -ErrorAction SilentlyContinue

    Write-Host ""
    Write-Host "  [OK] $(t 'Done!' $Z.DONE)" -ForegroundColor Green
    Write-Host ""
}

Main
