[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$Repository = "tupini07/copilot-session-tui"
$AssetName = "copilot-session-tui-x86_64-pc-windows-msvc.zip"
$InstallDir = if ($env:CST_INSTALL_DIR) {
    [IO.Path]::GetFullPath($env:CST_INSTALL_DIR)
} else {
    Join-Path $env:LOCALAPPDATA "Programs\copilot-session-tui\bin"
}
$SkipShellInit = $env:CST_NO_SHELL_INIT -eq "1"
$SkipPathUpdate = $env:CST_NO_PATH_UPDATE -eq "1"

if (-not [Environment]::Is64BitOperatingSystem) {
    throw "CST currently publishes a Windows x64 binary only."
}

$headers = @{
    Accept = "application/vnd.github+json"
    "User-Agent" = "copilot-session-tui-installer"
    "X-GitHub-Api-Version" = "2022-11-28"
}
$release = Invoke-RestMethod `
    -Uri "https://api.github.com/repos/$Repository/releases/latest" `
    -Headers $headers
$asset = $release.assets | Where-Object name -EQ $AssetName | Select-Object -First 1
if (-not $asset) {
    throw "Release $($release.tag_name) does not contain $AssetName."
}

$temporary = Join-Path ([IO.Path]::GetTempPath()) "cst-install-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $temporary | Out-Null
try {
    $archive = Join-Path $temporary $AssetName
    Invoke-WebRequest `
        -Uri $asset.browser_download_url `
        -Headers $headers `
        -OutFile $archive `
        -UseBasicParsing

    if ($asset.digest -and $asset.digest.StartsWith("sha256:")) {
        $expected = $asset.digest.Substring("sha256:".Length)
        $actual = (Get-FileHash -Algorithm SHA256 -Path $archive).Hash.ToLowerInvariant()
        if ($actual -ne $expected.ToLowerInvariant()) {
            throw "SHA-256 mismatch for $AssetName."
        }
    } else {
        Write-Warning "GitHub did not publish a digest for this release asset."
    }

    $expanded = Join-Path $temporary "expanded"
    Expand-Archive -Path $archive -DestinationPath $expanded
    $source = Join-Path $expanded "copilot-session-tui.exe"
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
        throw "$AssetName did not contain copilot-session-tui.exe."
    }

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    $destination = Join-Path $InstallDir "copilot-session-tui.exe"
    $staged = "$destination.installing.$PID"
    try {
        Copy-Item -LiteralPath $source -Destination $staged -Force
        Move-Item -LiteralPath $staged -Destination $destination -Force
    } finally {
        Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
    }

    if (-not $SkipPathUpdate) {
        $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
        $pathEntries = @($userPath -split ";" | Where-Object { $_ })
        if (-not ($pathEntries | Where-Object {
            $_.Trim().TrimEnd("\") -ieq $InstallDir.TrimEnd("\")
        })) {
            $newPath = (@($pathEntries) + $InstallDir) -join ";"
            [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
        }
    }
    if (-not (($env:Path -split ";") | Where-Object {
        $_ -and $_.Trim().TrimEnd("\") -ieq $InstallDir.TrimEnd("\")
    })) {
        $env:Path = "$InstallDir;$env:Path"
    }

    if (-not $SkipShellInit) {
        $profilePath = if ($env:CST_PROFILE) {
            [IO.Path]::GetFullPath($env:CST_PROFILE)
        } else {
            $PROFILE.CurrentUserAllHosts
        }
        $profileDir = Split-Path -Parent $profilePath
        if ($profileDir) {
            New-Item -ItemType Directory -Path $profileDir -Force | Out-Null
        }
        $startMarker = "# >>> copilot-session-tui >>>"
        $endMarker = "# <<< copilot-session-tui <<<"
        $existing = if (Test-Path -LiteralPath $profilePath) {
            [string](Get-Content -LiteralPath $profilePath -Raw)
        } else {
            ""
        }
        if (-not $existing.Contains($startMarker)) {
            $block = @"

$startMarker
if (Get-Command copilot-session-tui -ErrorAction SilentlyContinue) {
    Invoke-Expression (& copilot-session-tui init powershell | Out-String)
}
$endMarker
"@
            Add-Content -LiteralPath $profilePath -Value $block
        }
    }

    $version = & $destination --version
    Write-Host "Installed $version to $destination"
    if ($SkipShellInit) {
        Write-Host "Shell integration skipped because CST_NO_SHELL_INIT=1."
    } else {
        Write-Host "PowerShell integration is configured. Restart PowerShell, then run: cst"
    }
} finally {
    Remove-Item -LiteralPath $temporary -Recurse -Force -ErrorAction SilentlyContinue
}
