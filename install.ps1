# Installs Lucida on Windows: downloads the release binary, verifies its
# published checksum, and puts it somewhere on your PATH.
#
#   irm https://raw.githubusercontent.com/Artificial-Humanity/Lucida/main/install.ps1 | iex
#
# Or read it first and run it yourself, which is the better habit:
#
#   irm .../install.ps1 -OutFile install.ps1; notepad install.ps1; ./install.ps1
#
# Everything is in a function called on the last line, for the same reason as
# install.sh: a transfer that drops halfway must do nothing rather than execute
# the half that arrived.
#
# Settings:
#   $env:LUCIDA_INSTALL_DIR   where to put it (default: %LOCALAPPDATA%\Programs\lucida)
#   $env:LUCIDA_VERSION       a tag to pin, e.g. v0.9.0 (default: the latest release)
#   $env:GITHUB_TOKEN         used if set, purely to avoid the unauthenticated
#                             rate limit — no scopes are needed for public releases

$ErrorActionPreference = 'Stop'

function Install-Lucida {
    $repo = 'Artificial-Humanity/Lucida'
    $releasesPage = "https://github.com/$repo/releases/latest"

    if ([Environment]::Is64BitOperatingSystem -eq $false) {
        throw "no release binary is published for 32-bit Windows. See $releasesPage"
    }

    $tag = $env:LUCIDA_VERSION
    if (-not $tag) {
        $api = "https://api.github.com/repos/$repo/releases/latest"
        $headers = @{ 'User-Agent' = 'lucida-install' }
        if ($env:GITHUB_TOKEN) { $headers['Authorization'] = "Bearer $env:GITHUB_TOKEN" }
        try {
            # PowerShell parses JSON natively, so unlike the shell script this
            # needs no hand-rolled extraction.
            $tag = (Invoke-RestMethod -Uri $api -Headers $headers).tag_name
        } catch {
            throw "could not reach $api — if this is a rate limit it clears within the hour. See $releasesPage"
        }
    }

    $version = $tag -replace '^v', ''
    $asset = "lucida-$version-x86_64-windows.exe"
    $base = "https://github.com/$repo/releases/download/$tag"

    $dir = $env:LUCIDA_INSTALL_DIR
    if (-not $dir) { $dir = Join-Path $env:LOCALAPPDATA 'Programs\lucida' }

    Write-Host "Installing lucida $version to $dir"

    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ([System.IO.Path]::GetRandomFileName())
    New-Item -ItemType Directory -Path $tmp -Force | Out-Null
    try {
        $binary = Join-Path $tmp $asset
        Invoke-WebRequest -Uri "$base/$asset" -OutFile $binary
        Invoke-WebRequest -Uri "$base/$asset.sha256" -OutFile "$binary.sha256"

        # Verified, not merely downloaded — the easiest install path must not
        # also be the least checked one.
        $expected = ((Get-Content "$binary.sha256" -Raw).Trim() -split '\s+')[0]
        $actual = (Get-FileHash -Path $binary -Algorithm SHA256).Hash
        if ($actual -ine $expected) {
            throw "the download does not match its published checksum, so it was not installed.`n  expected $expected`n  got      $actual"
        }
        Write-Host 'Checksum verified.'

        New-Item -ItemType Directory -Path $dir -Force | Out-Null
        Move-Item -Path $binary -Destination (Join-Path $dir 'lucida.exe') -Force
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }

    $installed = Join-Path $dir 'lucida.exe'
    Write-Host ''
    Write-Host "$(& $installed --version) installed at $installed"

    $onPath = ($env:PATH -split ';') -contains $dir
    if ($onPath) {
        Write-Host 'Run `lucida --help` to get started.'
    } else {
        # Said rather than done: editing a user's persistent PATH from a piped
        # script is a larger liberty than installing the binary they asked for.
        Write-Warning "$dir is not on your PATH. Add it for this session:"
        Write-Host ""
        Write-Host "  `$env:PATH = `"$dir;`$env:PATH`""
        Write-Host ""
        Write-Host "Or permanently:"
        Write-Host ""
        Write-Host "  [Environment]::SetEnvironmentVariable('PATH', `"$dir;`" + [Environment]::GetEnvironmentVariable('PATH','User'), 'User')"
    }
}

Install-Lucida
