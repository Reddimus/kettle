#requires -Version 5.1
<#
.SYNOPSIS
    kettle - Windows user-install (no admin / UAC required).

.DESCRIPTION
    Cycle 733: Windows equivalent of `scripts/install.sh`. Drops
    everything into per-user paths so kettle shows up in Windows
    Search / Start menu - no system-wide changes, no admin.

      %LOCALAPPDATA%\Programs\kettle\kettle.exe            <- the binary
      %LOCALAPPDATA%\Programs\kettle\kettle.ico            <- icon
      %LOCALAPPDATA%\Programs\kettle\shell-integration\    <- OSC 133 snippets
      %APPDATA%\Microsoft\Windows\Start Menu\Programs\kettle.lnk
          ^ Start menu shortcut (so Win-key -> "kettle" finds it)
      HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\kettle
          ^ Add/Remove Programs entry pointing back at this script

    Two layouts supported:
    - Extracted release .zip: `scripts/install.ps1` lives next to
      `kettle.exe`, ico, LICENSE, README, CHANGELOG, shell-integration/.
    - In-tree repo: this script at `scripts/install.ps1`; binary at
      `target/release/kettle.exe` (built by `cargo build --release -p
      kettle` or `just release`).

    User PATH update is on by default so `kettle.exe` is callable from
    any shell after a restart of that shell. Pass `-NoPath` to skip.

.PARAMETER Uninstall
    Reverse everything this script did. Removes the install dir, the
    Start menu shortcut, the Add/Remove Programs registry entry, and
    the PATH addition (only if the entry is exactly this install dir).

.PARAMETER NoPath
    Skip the user PATH update. kettle.exe will still be launchable
    from the Start menu shortcut, but you'll need the full
    `%LOCALAPPDATA%\Programs\kettle\kettle.exe` path from the shell.

.PARAMETER RefreshIntegration
    Refresh the managed Start menu shortcut and Add/Remove Programs metadata
    without copying files. Used internally after an authenticated self-update.

.PARAMETER Prefix
    Override the install location. Default: `%LOCALAPPDATA%\Programs\kettle`.
    For a portable install on a USB stick, pass e.g.
    `-Prefix "D:\PortableApps\kettle"` - the script doesn't write to
    the registry or PATH when Prefix is non-default (the assumption is
    a portable install means "no system traces").

.EXAMPLE
    .\install.ps1
    # Default install. Drops kettle into %LOCALAPPDATA%\Programs\kettle,
    # creates Start menu shortcut, adds to user PATH, registers in
    # Add/Remove Programs.

.EXAMPLE
    .\install.ps1 -Uninstall
    # Reverses everything.

.EXAMPLE
    .\install.ps1 -NoPath
    # Default install minus the PATH addition.

.NOTES
    Runs in user scope (HKCU + %LOCALAPPDATA%); no UAC prompt. The
    Start menu shortcut + Add/Remove Programs entry are per-user too,
    so a different Windows user on the same machine doesn't see your
    kettle install.
#>

[CmdletBinding()]
param(
    [switch] $Uninstall,
    [switch] $NoPath,
    [switch] $WithShellIntegration,
    [switch] $RefreshIntegration,
    [string] $Prefix = (Join-Path $env:LOCALAPPDATA "Programs\kettle"),
    [Parameter(DontShow = $true)]
    [string] $IntegrationTestRoot
)

$ErrorActionPreference = 'Stop'

# Detect the layout: extracted-zip mode keeps `kettle.exe` next to
# this script; in-repo mode has it under `target/release/`.
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
$prefixMarker = Join-Path $scriptDir ".kettle-install-prefix"
$prefixFromMarker = $false
if (-not $PSBoundParameters.ContainsKey('Prefix') -and (Test-Path $prefixMarker)) {
    $savedPrefix = (Get-Content $prefixMarker -Raw -ErrorAction SilentlyContinue).Trim()
    if ($savedPrefix) {
        $Prefix = $savedPrefix
        $prefixFromMarker = $true
    }
}
$zipModeExe = Join-Path $scriptDir "kettle.exe"
$repoModeExe = Join-Path (Split-Path -Parent $scriptDir) "target\release\kettle.exe"

if (Test-Path $zipModeExe) {
    $sourceMode = 'zip'
    $sourceDir = $scriptDir
    $sourceExe = $zipModeExe
} elseif (Test-Path $repoModeExe) {
    $sourceMode = 'repo'
    $sourceDir = Split-Path -Parent $scriptDir   # repo root
    $sourceExe = $repoModeExe
} else {
    $sourceMode = $null
}

$integrationTest = -not [string]::IsNullOrWhiteSpace($IntegrationTestRoot)
if ($integrationTest) {
    # The Windows installer smoke uses isolated filesystem and registry roots to
    # exercise the real default-install path without touching the developer's
    # installed app, Start menu, PATH, or Add/Remove Programs entry.
    $IntegrationTestRoot = [System.IO.Path]::GetFullPath($IntegrationTestRoot)
    $testDefaultPrefix = Join-Path $IntegrationTestRoot "Programs\kettle"
    if (-not $PSBoundParameters.ContainsKey('Prefix') -and -not $prefixFromMarker) {
        $Prefix = $testDefaultPrefix
    }
    $startMenuDir = Join-Path $IntegrationTestRoot "Start Menu\Programs"
    $uninstallKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\kettle-installer-smoke-$PID"
    $profilePath = Join-Path $IntegrationTestRoot "WindowsPowerShell\profile.ps1"
    $portable = ($Prefix -ne $testDefaultPrefix)
    $NoPath = $true
} else {
    $startMenuDir = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs"
    $uninstallKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\kettle"
    $profilePath = $PROFILE
    $portable = ($Prefix -ne (Join-Path $env:LOCALAPPDATA "Programs\kettle"))
}
$shortcutPath = Join-Path $startMenuDir "kettle.lnk"

function Update-UserPath {
    param([string] $Dir, [switch] $Remove)
    $current = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($null -eq $current) { $current = '' }
    # Split + filter exact-match (case-insensitive) so we don't strip a
    # superstring entry by accident.
    $parts = $current -split ';' | Where-Object { $_ -ne '' }
    $without = $parts | Where-Object { $_ -ne $Dir }
    if ($Remove) {
        if ($without.Count -eq $parts.Count) { return $false }  # nothing to remove
        $new = ($without -join ';')
    } else {
        if ($without.Count -ne $parts.Count) { return $false }  # already present
        $new = (@($without) + $Dir) -join ';'
    }
    [Environment]::SetEnvironmentVariable("Path", $new, "User")
    return $true
}

if ($Uninstall) {
    Write-Output "Removing kettle..."
    if (Test-Path $Prefix) {
        Remove-Item -Recurse -Force $Prefix
        Write-Output "  removed $Prefix"
    } else {
        Write-Output "  install dir already absent: $Prefix"
    }
    if (-not $portable) {
        if (Test-Path $shortcutPath) {
            Remove-Item -Force $shortcutPath
            Write-Output "  removed Start menu shortcut"
        }
        if (Test-Path $uninstallKey) {
            Remove-Item -Recurse -Force $uninstallKey
            Write-Output "  removed Add/Remove Programs entry"
        }
        if (Update-UserPath -Dir $Prefix -Remove) {
            Write-Output "  removed $Prefix from user PATH"
        }
        # Cycle 736: also strip any -WithShellIntegration block we
        # appended to $PROFILE. Portable installs never add this block, so their
        # uninstall path must not remove integration owned by a default install.
        if (Test-Path $profilePath) {
            $content = Get-Content $profilePath -Raw -ErrorAction SilentlyContinue
            $beginMarker = '# >>> kettle shell-integration (managed by install.ps1)'
            $endMarker   = '# <<< kettle shell-integration (managed by install.ps1)'
            if ($content -and $content.Contains($beginMarker) -and $content.Contains($endMarker)) {
                $startIdx = $content.IndexOf($beginMarker)
                $endIdx   = $content.IndexOf($endMarker, $startIdx) + $endMarker.Length
                $before = $content.Substring(0, $startIdx).TrimEnd()
                $after  = $content.Substring($endIdx).TrimStart()
                $newContent = if ($before -and $after) { "$before`r`n`r`n$after`r`n" }
                              elseif ($before) { "$before`r`n" }
                              elseif ($after) { $after }
                              else { '' }
                Set-Content -Path $profilePath -Value $newContent -NoNewline
                Write-Output "  removed kettle.ps1 snippet from `$PROFILE"
            }
        }
    }
    Write-Output ""
    Write-Output "Uninstall complete. (Restart any open shells for PATH changes to take effect.)"
    return
}

if ($null -eq $sourceMode) {
    Write-Error @"
Could not find kettle.exe to install.

Looked for:
  $zipModeExe   (extracted release .zip layout)
  $repoModeExe  (in-tree repo layout - run `cargo build --release -p kettle` first)

If you grabbed the release zip, make sure you extracted it AND ran
install.ps1 from inside the extracted folder. If you cloned the repo,
build the release binary first:

    cargo build --release -p kettle
    .\scripts\install.ps1
"@
    exit 1
}

$consoleLauncher = if ($sourceMode -eq 'zip') {
    Join-Path $sourceDir "kettle.com"
} else {
    Join-Path $sourceDir "target\release\kettle-console.exe"
}
if (-not (Test-Path -LiteralPath $consoleLauncher -PathType Leaf)) {
    Write-Error "Could not find the required kettle console launcher: $consoleLauncher"
    exit 1
}

if ($RefreshIntegration) {
    if ($portable) {
        Write-Output "Portable install: no Windows integration to refresh."
        return
    }
    $installedExe = Join-Path $Prefix "kettle.exe"
    $installedIcon = Join-Path $Prefix "kettle.ico"
    $installMarker = Join-Path $Prefix ".kettle-install.json"
    if (-not (Test-Path -LiteralPath $installedExe -PathType Leaf)) {
        Write-Error "Cannot refresh integration: $installedExe is missing."
        exit 1
    }
    $installedVersion = "unknown"
    if (Test-Path -LiteralPath $installMarker -PathType Leaf) {
        try {
            $marker = Get-Content -LiteralPath $installMarker -Raw | ConvertFrom-Json
            if ($marker.version -match '^[0-9]+\.[0-9]+\.[0-9]+$') {
                $installedVersion = $marker.version
            }
        } catch {}
    }

    New-Item -ItemType Directory -Force -Path $startMenuDir | Out-Null
    if (Test-Path -LiteralPath $shortcutPath) {
        Remove-Item -LiteralPath $shortcutPath -Force
    }
    $ws = New-Object -ComObject WScript.Shell
    $lnk = $ws.CreateShortcut($shortcutPath)
    $lnk.TargetPath = $installedExe
    $lnk.Arguments = ''
    $lnk.WorkingDirectory = $Prefix
    $lnk.IconLocation = $installedIcon
    $lnk.Description = "Fast, GPU-accelerated terminal emulator"
    $lnk.Save()

    New-Item -Path $uninstallKey -Force | Out-Null
    Set-ItemProperty -Path $uninstallKey -Name "DisplayName" -Value "kettle"
    Set-ItemProperty -Path $uninstallKey -Name "DisplayVersion" -Value $installedVersion
    Set-ItemProperty -Path $uninstallKey -Name "Publisher" -Value "kettle contributors"
    Set-ItemProperty -Path $uninstallKey -Name "InstallLocation" -Value $Prefix
    Set-ItemProperty -Path $uninstallKey -Name "DisplayIcon" -Value $installedIcon
    Set-ItemProperty -Path $uninstallKey -Name "URLInfoAbout" -Value "https://github.com/Reddimus/kettle"
    Set-ItemProperty -Path $uninstallKey -Name "NoModify" -Value 1 -Type DWord
    Set-ItemProperty -Path $uninstallKey -Name "NoRepair" -Value 1 -Type DWord
    $uninstallCmd = "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$(Join-Path $Prefix 'install.ps1')`" -Uninstall"
    Set-ItemProperty -Path $uninstallKey -Name "UninstallString" -Value $uninstallCmd
    if (-not $integrationTest) {
        try { & (Join-Path $env:SystemRoot 'System32\ie4uinit.exe') -show 2>$null } catch {}
    }
    Write-Output "Refreshed kettle Windows integration for version $installedVersion."
    return
}

Write-Output "Installing kettle (source: $sourceMode mode, from $sourceDir)"
Write-Output ""

New-Item -ItemType Directory -Force -Path $Prefix | Out-Null
Copy-Item -Force $sourceExe (Join-Path $Prefix "kettle.exe")
Write-Output "  installed kettle.exe -> $Prefix"

# Windows resolves .com before .exe for a bare `kettle` command. The console
# launcher waits for CLI operations and starts GUI invocations asynchronously.
Copy-Item -Force $consoleLauncher (Join-Path $Prefix "kettle.com")
Write-Output "  installed kettle.com console launcher"

# Icon: zip mode ships kettle.ico next to the .exe; repo mode pulls
# from packaging/windows/.
$icoSrc = if ($sourceMode -eq 'zip') {
    Join-Path $sourceDir "kettle.ico"
} else {
    Join-Path $sourceDir "packaging\windows\kettle.ico"
}
if (Test-Path $icoSrc) {
    Copy-Item -Force $icoSrc (Join-Path $Prefix "kettle.ico")
    Write-Output "  installed kettle.ico"
}

# Bundle the supporting files so the install dir is self-contained.
foreach ($extra in @('LICENSE', 'NOTICE', 'README.md', 'CHANGELOG.md')) {
    $src = Join-Path $sourceDir $extra
    if (Test-Path $src) {
        Copy-Item -Force $src (Join-Path $Prefix $extra)
    }
}

# Shell-integration snippets: both layouts have them at
# `shell-integration/kettle.{bash,zsh,fish,ps1}` relative to the
# source root.
$shellIntegrationSrc = Join-Path $sourceDir "shell-integration"
if (Test-Path $shellIntegrationSrc) {
    $shellIntegrationDst = Join-Path $Prefix "shell-integration"
    if (Test-Path $shellIntegrationDst) {
        Remove-Item -Recurse -Force $shellIntegrationDst
    }
    Copy-Item -Recurse -Force $shellIntegrationSrc $shellIntegrationDst
    Write-Output "  installed shell-integration\ (bash, zsh, fish, ps1)"
}

# Copy this script too so the Add/Remove Programs UninstallString
# resolves even after the user moves on from the source dir.
Copy-Item -Force $MyInvocation.MyCommand.Definition (Join-Path $Prefix "install.ps1")
# Persist the effective prefix next to the saved helper. This lets
# `$Prefix\install.ps1 -Uninstall` work for portable/custom-prefix installs
# without requiring the user to repeat `-Prefix`; release zip/source-tree copies
# do not have this marker, so they keep the normal default-prefix behavior.
Set-Content -Path (Join-Path $Prefix ".kettle-install-prefix") -Value $Prefix -NoNewline

# Capture the installed version once for both the ownership marker and the
# Add/Remove Programs entry. Start-Process is required because kettle.exe uses
# the Windows GUI subsystem and PowerShell otherwise does not wait for stdout.
$exeForVersion = Join-Path $Prefix "kettle.exe"
$versionTmp = Join-Path $env:TEMP "kettle-install-ver-$PID.txt"
Remove-Item -ErrorAction SilentlyContinue $versionTmp
try {
    Start-Process -FilePath $exeForVersion -ArgumentList '--version' `
        -NoNewWindow -Wait -RedirectStandardOutput $versionTmp `
        -ErrorAction Stop
} catch {}
$kettleVersion = if (Test-Path $versionTmp) {
    $line = (Get-Content $versionTmp -Raw -ErrorAction SilentlyContinue)
    $m = if ($line) { [regex]::Match($line, '^kettle ([0-9.]+)') } else { $null }
    if ($m -and $m.Success) { $m.Groups[1].Value } else { "unknown" }
} else { "unknown" }
Remove-Item -ErrorAction SilentlyContinue $versionTmp

# Explicit ownership marker consumed by `kettle update`. Package-manager,
# cargo-install, and manually copied binaries do not carry this marker and are
# therefore never overwritten by the self-updater.
$installChannel = if ($sourceMode -eq 'zip') { 'stable' } else { 'local-dev' }
$installMarker = [ordered]@{
    schema = 1
    product = "kettle"
    managed_by = "kettle-installer"
    channel = $installChannel
    target = "x86_64-pc-windows-msvc"
    version = $kettleVersion
} | ConvertTo-Json
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)
[System.IO.File]::WriteAllText((Join-Path $Prefix ".kettle-install.json"), $installMarker + "`n", $utf8NoBom)
Write-Output "  wrote authenticated-update ownership marker ($installChannel)"

# Portable mode short-circuits the system-touching steps.
if ($portable) {
    Write-Output ""
    Write-Output "Portable install complete at $Prefix"
    Write-Output "  - no Start menu shortcut (portable mode)"
    Write-Output "  - no PATH update (portable mode)"
    Write-Output "  - no Add/Remove Programs entry (portable mode)"
    Write-Output "Launch with: $Prefix\kettle.exe"
    return
}

# Start menu shortcut - via WScript.Shell COM (built into Windows;
# no external dependency). The shortcut lives under %APPDATA% so
# Windows Search indexes it without admin.
New-Item -ItemType Directory -Force -Path $startMenuDir | Out-Null
$ws = New-Object -ComObject WScript.Shell
# CreateShortcut opens an existing .lnk and preserves every property the caller
# does not overwrite. Replace our managed shortcut so an older launcher's
# arguments (for example, a PowerShell dev-record wrapper) cannot survive an
# upgrade and be passed to kettle.exe.
if (Test-Path -LiteralPath $shortcutPath) {
    Remove-Item -LiteralPath $shortcutPath -Force
}
$lnk = $ws.CreateShortcut($shortcutPath)
$lnk.TargetPath = Join-Path $Prefix "kettle.exe"
$lnk.Arguments = ''
$lnk.WorkingDirectory = $Prefix
$lnk.IconLocation = Join-Path $Prefix "kettle.ico"
$lnk.Description = "Fast, GPU-accelerated terminal emulator"
$lnk.Save()
Write-Output "  created Start menu shortcut: $shortcutPath"

# Cycle 918: refresh the Windows icon cache. Explorer caches launcher icons by
# path, and an in-place `kettle.ico` overwrite raises no change notification, so
# a re-install with a CHANGED icon (e.g. the Catppuccin Mocha re-theme) would
# otherwise keep showing the stale bitmap in Start / search / taskbar until the
# cache rebuilds on its own. `ie4uinit -show` is the light, non-admin refresh;
# wrapped so a failure (older/newer Windows flag differences) never aborts the
# install. A full rebuild (clear %LOCALAPPDATA%\IconCache.db + restart Explorer)
# is only needed in the rare case this doesn't take.
if (-not $integrationTest) {
    try { & (Join-Path $env:SystemRoot 'System32\ie4uinit.exe') -show 2>$null } catch {}
}

# Add/Remove Programs entry. Per-user (HKCU); no admin required.
New-Item -Path $uninstallKey -Force | Out-Null
Set-ItemProperty -Path $uninstallKey -Name "DisplayName" -Value "kettle"
Set-ItemProperty -Path $uninstallKey -Name "DisplayVersion" -Value $kettleVersion
Set-ItemProperty -Path $uninstallKey -Name "Publisher" -Value "kettle contributors"
Set-ItemProperty -Path $uninstallKey -Name "InstallLocation" -Value $Prefix
Set-ItemProperty -Path $uninstallKey -Name "DisplayIcon" -Value (Join-Path $Prefix "kettle.ico")
Set-ItemProperty -Path $uninstallKey -Name "URLInfoAbout" -Value "https://github.com/Reddimus/kettle"
Set-ItemProperty -Path $uninstallKey -Name "NoModify" -Value 1 -Type DWord
Set-ItemProperty -Path $uninstallKey -Name "NoRepair" -Value 1 -Type DWord
$uninstallCmd = "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$(Join-Path $Prefix 'install.ps1')`" -Uninstall"
Set-ItemProperty -Path $uninstallKey -Name "UninstallString" -Value $uninstallCmd
Write-Output "  registered in Add/Remove Programs (HKCU)"

# User PATH addition. Default-on; -NoPath to skip.
if (-not $NoPath) {
    if (Update-UserPath -Dir $Prefix) {
        Write-Output "  added $Prefix to user PATH"
        Write-Output "    (open a fresh shell to pick it up - already-running shells keep their snapshot)"
    } else {
        Write-Output "  $Prefix already on user PATH (no change)"
    }
}

# Cycle 736: optional opt-in install of the PowerShell shell
# integration snippet (kettle.ps1) into $PROFILE. The recommended
# install path (vs. the bash/zsh/fish "kettle --shell-integration
# powershell >> $PROFILE" one-liner) because that one-liner does NOT
# work under SUBSYSTEM:WINDOWS (cycle 734 trade-off - PS doesn't
# read stdout from GUI processes). Idempotent: the snippet itself
# has an internal $global:__kettle_prompt_installed guard, AND we
# skip the Add-Content if the snippet's signature line is already
# in $PROFILE so re-running install.ps1 -WithShellIntegration is
# a no-op.
if ($WithShellIntegration) {
    $snippetSrc = Join-Path $Prefix "shell-integration\kettle.ps1"
    if (-not (Test-Path $snippetSrc)) {
        Write-Output "  -WithShellIntegration: snippet not found at $snippetSrc (skipping)"
    } else {
        if (-not (Test-Path $profilePath)) {
            $profileDir = Split-Path $profilePath -Parent
            if (-not (Test-Path $profileDir)) {
                New-Item -ItemType Directory -Force -Path $profileDir | Out-Null
            }
            New-Item -ItemType File -Force -Path $profilePath | Out-Null
        }
        # Wrap the snippet in distinctive BEGIN/END markers so the
        # uninstall path can find + remove the exact block we added
        # (oh-my-posh / conda init / nvm pattern). Re-run safety: if
        # the marker already exists in $PROFILE, skip the append.
        $beginMarker = '# >>> kettle shell-integration (managed by install.ps1)'
        $endMarker   = '# <<< kettle shell-integration (managed by install.ps1)'
        $current = Get-Content $profilePath -Raw -ErrorAction SilentlyContinue
        if ($current -and $current.Contains($beginMarker)) {
            Write-Output "  -WithShellIntegration: snippet already in `$PROFILE (no change)"
        } else {
            $snippet = Get-Content $snippetSrc -Raw
            # Prepend a blank-line separator for readability if the
            # profile already has content.
            if ($current -and $current.Trim().Length -gt 0) {
                Add-Content $profilePath "`r`n"
            }
            Add-Content $profilePath $beginMarker
            Add-Content $profilePath $snippet
            Add-Content $profilePath $endMarker
            Write-Output "  -WithShellIntegration: appended kettle.ps1 to `$PROFILE ($profilePath)"
            Write-Output "    (open a fresh PowerShell session to pick up the prompt marks)"
        }
    }
}

Write-Output ""
Write-Output "Install complete."
Write-Output ""
Write-Output "Try:"
Write-Output "  - Press Win, type 'kettle', hit Enter."
Write-Output "  - Or from a fresh shell: kettle --version"
if (-not $WithShellIntegration) {
    Write-Output ""
    Write-Output "Tip: re-run with -WithShellIntegration to enable OSC 133"
    Write-Output "  prompt marks in PowerShell (Ctrl+Up / Ctrl+Down to jump"
    Write-Output "  between prompts inside kettle)."
}
Write-Output ""
Write-Output "To uninstall later: appwiz.cpl (Add/Remove Programs) or"
Write-Output "  powershell -File `"$Prefix\install.ps1`" -Uninstall"
