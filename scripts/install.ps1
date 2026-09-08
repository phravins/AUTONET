# AutoNet installer, for Windows.
#
#   irm https://raw.githubusercontent.com/phravins/AUTONET/main/scripts/install.ps1 | iex
#
# Linux and macOS have their own: scripts/install.sh. This is the same installer
# in PowerShell, under the same rules: no package manager required, nothing
# extracted before its checksum is verified, no administrator rights, and no
# existing install overwritten without asking.
#
# Environment (the same three names install.sh uses):
#
#   $env:AUTONET_VERSION     = '0.1.0'   install this version, not the latest
#   $env:AUTONET_INSTALL_DIR = 'C:\bin'  install here instead of the default
#   $env:AUTONET_FORCE       = '1'       overwrite an existing install silently

$ErrorActionPreference = 'Stop'

$Repo     = 'phravins/AUTONET'
$Releases = "https://github.com/$Repo/releases"

function Say  { param([string]$m) Write-Host $m }
function Info { param([string]$m) Write-Host "  $m" }

# --- what am I running on? ---------------------------------------------------
#
# release.yml builds one Windows target, x86_64-pc-windows-msvc. On ARM64 that
# binary still runs, under the x64 emulation Windows on ARM provides, so it is
# offered rather than refused -- but said out loud, because "works, emulated" is
# worth knowing. 32-bit Windows has no build at all.
function Get-Target {
    # On 32-bit PowerShell hosted by 64-bit Windows, PROCESSOR_ARCHITECTURE
    # reads x86 and the real answer is in PROCESSOR_ARCHITEW6432.
    $arch = $env:PROCESSOR_ARCHITEW6432
    if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }

    if ($arch -eq 'ARM64') {
        Info 'note       ARM64 detected; installing the x86_64 build, which Windows runs emulated'
    } elseif ($arch -ne 'AMD64') {
        throw "unsupported architecture: $arch. AutoNet ships an x86_64 Windows build only."
    }
    return 'x86_64-pc-windows-msvc'
}

# --- downloading -------------------------------------------------------------
#
# Windows PowerShell 5.1 negotiates TLS 1.0 by default on older builds, which
# GitHub refuses outright.
function Initialize-Tls {
    try {
        [Net.ServicePointManager]::SecurityProtocol =
            [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    } catch {
        # PowerShell 7 manages this itself and the property may not be settable.
    }
}

function Get-File {
    param([string]$Url, [string]$Path)
    try {
        Invoke-WebRequest -Uri $Url -OutFile $Path -UseBasicParsing
    } catch {
        throw "could not download $Url`n       $($_.Exception.Message)"
    }
}

# --- SmartScreen -------------------------------------------------------------
#
# The mirror of install.sh's Gatekeeper note, conditional for the same reason.
# Mark of the Web is what SmartScreen acts on, and it is applied by browsers and
# the Attachment Execution Service, not by Invoke-WebRequest -- so on this path
# there is usually nothing to report, and reporting it anyway would be noise in
# front of every Windows user. The stream is checked instead, and Unblock-File
# printed for a person to run rather than run silently here.
function Show-SmartScreenNote {
    param([string]$Zip, [string]$Exe)
    try {
        Get-Item -LiteralPath $Zip -Stream 'Zone.Identifier' -ErrorAction Stop | Out-Null
    } catch {
        return
    }
    Say ''
    Say '  This download carries a Mark of the Web, so SmartScreen will warn'
    Say '  before it runs. AutoNet is not code-signed, so an "unrecognised app"'
    Say '  warning is expected rather than a sign that something is wrong. To'
    Say '  clear the mark:'
    Say ''
    Say "      Unblock-File '$Exe'"
}

# --- where to put it ---------------------------------------------------------
#
# Per-user, under LOCALAPPDATA. Program Files would need administrator rights
# and this installer asks for none, the same rule as install.sh not calling sudo.
function Get-InstallDir {
    if ($env:AUTONET_INSTALL_DIR) { $dir = $env:AUTONET_INSTALL_DIR }
    else { $dir = Join-Path $env:LOCALAPPDATA 'Programs\autonet' }
    if (-not (Test-Path -LiteralPath $dir)) {
        New-Item -ItemType Directory -Path $dir -Force | Out-Null
    }
    return (Resolve-Path -LiteralPath $dir).Path
}

# Read the *user* PATH from the registry, never $env:Path. $env:Path is the
# machine and user values already merged, and writing that back into the user
# scope copies every machine entry into it permanently -- a well-known way for a
# one-line installer to wreck an environment. The value kind is preserved too,
# so a REG_EXPAND_SZ PATH full of %USERPROFILE% keeps working, and entries are
# compared expanded so an existing %LOCALAPPDATA%\... entry is not duplicated.
function Add-ToUserPath {
    param([string]$Dir)

    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
    try {
        $current = $key.GetValue(
            'Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        $entries = @($current -split ';' | Where-Object { $_ })

        foreach ($entry in $entries) {
            $expanded = [Environment]::ExpandEnvironmentVariables($entry).TrimEnd('\')
            if ($expanded -ieq $Dir.TrimEnd('\')) { return $false }
        }

        $kind = [Microsoft.Win32.RegistryValueKind]::ExpandString
        if ($current) { $kind = $key.GetValueKind('Path') }
        $key.SetValue('Path', (($entries + $Dir) -join ';'), $kind)
    } finally {
        $key.Close()
    }
    $env:Path = "$env:Path;$Dir"
    return $true
}

# --- overwriting an existing install -----------------------------------------
function Test-Interactive {
    try { return [Environment]::UserInteractive -and ($null -ne $Host.UI.RawUI) }
    catch { return $false }
}

function Install-Binary {
    param([string]$From, [string]$To)

    if (Test-Path -LiteralPath $To) {
        $existing = 'unknown version'
        try { $existing = (& $To --version 2>$null | Select-Object -First 1) } catch { }

        if ($env:AUTONET_FORCE -eq '1') {
            Info "replacing  $To ($existing)"
        } elseif (Test-Interactive) {
            $reply = Read-Host "  $To already exists ($existing). Replace it? [y/N]"
            if ($reply -notmatch '^\s*(y|yes)\s*$') {
                throw "left $To alone. Nothing was installed."
            }
        } else {
            throw ("$To already exists ($existing), and there is no console to ask on.`n" +
                   "       Set `$env:AUTONET_FORCE = '1' to replace it, or " +
                   "`$env:AUTONET_INSTALL_DIR to somewhere else.")
        }
    }

    # Write beside the target and rename, so a failed copy cannot leave a
    # half-written exe where a working one used to be.
    Copy-Item -LiteralPath $From -Destination "$To.new" -Force
    Move-Item -LiteralPath "$To.new" -Destination $To -Force
}

function Install-AutoNet {
    # Invoke-WebRequest's progress bar costs more than the download does on
    # Windows PowerShell 5.1. Set here rather than globally so the setting does
    # not outlive the install; callees inherit it.
    $ProgressPreference = 'SilentlyContinue'
    Initialize-Tls

    Say 'AutoNet installer'
    $target = Get-Target
    Info "platform   $target"

    # SHA256SUMS does double duty: it is the integrity check, and -- because
    # release.yml puts the version in every filename -- it is also how the
    # version is discovered. GitHub resolves /latest/download/ server-side, so
    # this needs no API call and cannot be rate-limited.
    if ($env:AUTONET_VERSION) {
        $want = $env:AUTONET_VERSION -replace '^v', ''
        $sumsUrl = "$Releases/download/v$want/SHA256SUMS"
    } else {
        $sumsUrl = "$Releases/latest/download/SHA256SUMS"
    }

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ('autonet-' + [Guid]::NewGuid().ToString('n'))
    New-Item -ItemType Directory -Path $tmp -Force | Out-Null
    try {
        $sums = Join-Path $tmp 'SHA256SUMS'
        try {
            Get-File $sumsUrl $sums
        } catch {
            throw ("$_`n       If this is a fresh repository there may be no release yet;" +
                   "`n       check $Releases")
        }

        $pattern = '^([0-9a-fA-F]{64})\s+(autonet-(.+)-' + [Regex]::Escape($target) + '\.zip)$'
        $lines = Get-Content -LiteralPath $sums
        $line = $lines | Where-Object { $_ -match $pattern } | Select-Object -First 1
        if (-not $line) {
            throw ("this release has no build for $target.`n       It contains:`n" +
                   (($lines | ForEach-Object {
                        '         ' + ($_ -replace '^[0-9a-fA-F]+\s+', '') }) -join "`n"))
        }
        $null = $line -match $pattern
        $expected = $Matches[1].ToLower()
        $archive  = $Matches[2]
        $version  = $Matches[3]
        Info "version    $version"

        $zip = Join-Path $tmp $archive
        Get-File "$Releases/download/v$version/$archive" $zip

        $actual = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLower()
        if ($actual -ne $expected) {
            throw ("checksum mismatch for $archive.`n" +
                   "       expected $expected`n" +
                   "       got      $actual`n" +
                   "       Nothing has been extracted or installed. This means the download`n" +
                   "       was corrupted or tampered with; do not retry blindly.")
        }
        Info 'checksum   ok'

        # Only now, verified, is anything unpacked. release.yml's zip is flat:
        # autonet.exe and the licences sit at the top level, no wrapping
        # directory.
        $unpack = Join-Path $tmp 'unpack'
        Expand-Archive -LiteralPath $zip -DestinationPath $unpack -Force
        $exe = Join-Path $unpack 'autonet.exe'
        if (-not (Test-Path -LiteralPath $exe)) {
            throw 'the archive did not contain autonet.exe where expected.'
        }

        $dir = Get-InstallDir
        $dest = Join-Path $dir 'autonet.exe'
        Install-Binary $exe $dest

        Say ''
        Say "Installed $dest ($version)."
        Show-SmartScreenNote $zip $dest

        if (Add-ToUserPath $dir) {
            Say ''
            Say "  $dir has been added to your PATH. Open a new terminal for"
            Say '  that to take effect.'
        }
        Say ''
        Say 'Try:  autonet status'
    } finally {
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# Errors unwind to here rather than calling exit, because under `irm ... | iex`
# an exit would close the user's console rather than end a script.
try {
    Install-AutoNet
} catch {
    Write-Host "error: $_" -ForegroundColor Red
    if ($PSCommandPath) { exit 1 }
}
