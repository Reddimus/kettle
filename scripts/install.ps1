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
    [string] $Prefix = (Join-Path $env:LOCALAPPDATA "Programs\kettle")
)

$ErrorActionPreference = 'Stop'

# Detect the layout: extracted-zip mode keeps `kettle.exe` next to
# this script; in-repo mode has it under `target/release/`.
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
$prefixMarker = Join-Path $scriptDir ".kettle-install-prefix"
if (-not $PSBoundParameters.ContainsKey('Prefix') -and (Test-Path $prefixMarker)) {
    $savedPrefix = (Get-Content $prefixMarker -Raw -ErrorAction SilentlyContinue).Trim()
    if ($savedPrefix) {
        $Prefix = $savedPrefix
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

$startMenuDir = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs"
$shortcutPath = Join-Path $startMenuDir "kettle.lnk"
$uninstallKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\kettle"
$portable = ($Prefix -ne (Join-Path $env:LOCALAPPDATA "Programs\kettle"))

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
    if (Test-Path $shortcutPath) {
        Remove-Item -Force $shortcutPath
        Write-Output "  removed Start menu shortcut"
    }
    if (Test-Path $uninstallKey) {
        Remove-Item -Recurse -Force $uninstallKey
        Write-Output "  removed Add/Remove Programs entry"
    }
    if (-not $portable) {
        if (Update-UserPath -Dir $Prefix -Remove) {
            Write-Output "  removed $Prefix from user PATH"
        }
    }
    # Cycle 736: also strip any -WithShellIntegration block we
    # appended to $PROFILE. The install path wraps the snippet
    # between explicit BEGIN/END marker lines (same pattern oh-my-posh,
    # conda init, nvm, etc. use) so the uninstall can find + remove
    # the exact block we added without touching surrounding user
    # customization. Leaves the user's $PROFILE intact except for
    # the marker-delimited region.
    if (Test-Path $PROFILE) {
        $content = Get-Content $PROFILE -Raw -ErrorAction SilentlyContinue
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
            Set-Content -Path $PROFILE -Value $newContent -NoNewline
            Write-Output "  removed kettle.ps1 snippet from `$PROFILE"
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

Write-Output "Installing kettle (source: $sourceMode mode, from $sourceDir)"
Write-Output ""

New-Item -ItemType Directory -Force -Path $Prefix | Out-Null
Copy-Item -Force $sourceExe (Join-Path $Prefix "kettle.exe")
Write-Output "  installed kettle.exe -> $Prefix"

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
$lnk = $ws.CreateShortcut($shortcutPath)
$lnk.TargetPath = Join-Path $Prefix "kettle.exe"
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
try { & (Join-Path $env:SystemRoot 'System32\ie4uinit.exe') -show 2>$null } catch {}

# Add/Remove Programs entry. Per-user (HKCU); no admin required.
New-Item -Path $uninstallKey -Force | Out-Null
$exeForVersion = Join-Path $Prefix "kettle.exe"
# Cycle 734: kettle.exe is now SUBSYSTEM:WINDOWS, so `& kettle.exe
# --version` returns nothing under PowerShell (PS doesn't wait for
# GUI processes). Use Start-Process + redirect to a temp file to
# reliably capture the version even under SUBSYSTEM:WINDOWS. Falls
# back to "unknown" if the call fails (cycle-734 install tested
# this path).
$versionTmp = Join-Path $env:TEMP "kettle-install-ver.txt"
Remove-Item -ErrorAction SilentlyContinue $versionTmp
try {
    Start-Process -FilePath $exeForVersion -ArgumentList '--version' `
        -NoNewWindow -Wait -RedirectStandardOutput $versionTmp `
        -ErrorAction Stop
} catch {}
$kettleVersion = if (Test-Path $versionTmp) {
    $line = (Get-Content $versionTmp -Raw -ErrorAction SilentlyContinue)
    if ($line) {
        $m = [regex]::Match($line, '^kettle ([0-9.]+)')
        if ($m.Success) { $m.Groups[1].Value } else { "unknown" }
    } else { "unknown" }
} else { "unknown" }
Remove-Item -ErrorAction SilentlyContinue $versionTmp
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
        if (-not (Test-Path $PROFILE)) {
            $profileDir = Split-Path $PROFILE -Parent
            if (-not (Test-Path $profileDir)) {
                New-Item -ItemType Directory -Force -Path $profileDir | Out-Null
            }
            New-Item -ItemType File -Force -Path $PROFILE | Out-Null
        }
        # Wrap the snippet in distinctive BEGIN/END markers so the
        # uninstall path can find + remove the exact block we added
        # (oh-my-posh / conda init / nvm pattern). Re-run safety: if
        # the marker already exists in $PROFILE, skip the append.
        $beginMarker = '# >>> kettle shell-integration (managed by install.ps1)'
        $endMarker   = '# <<< kettle shell-integration (managed by install.ps1)'
        $current = Get-Content $PROFILE -Raw -ErrorAction SilentlyContinue
        if ($current -and $current.Contains($beginMarker)) {
            Write-Output "  -WithShellIntegration: snippet already in `$PROFILE (no change)"
        } else {
            $snippet = Get-Content $snippetSrc -Raw
            # Prepend a blank-line separator for readability if the
            # profile already has content.
            if ($current -and $current.Trim().Length -gt 0) {
                Add-Content $PROFILE "`r`n"
            }
            Add-Content $PROFILE $beginMarker
            Add-Content $PROFILE $snippet
            Add-Content $PROFILE $endMarker
            Write-Output "  -WithShellIntegration: appended kettle.ps1 to `$PROFILE ($PROFILE)"
            Write-Output "    (open a fresh PowerShell session to pick up the prompt marks)"
        }
    }
}

Write-Output ""
Write-Output "Install complete."
Write-Output ""
Write-Output "Try:"
Write-Output "  - Press Win, type 'kettle', hit Enter."
Write-Output "  - Or from a fresh shell: kettle.exe --version"
if (-not $WithShellIntegration) {
    Write-Output ""
    Write-Output "Tip: re-run with -WithShellIntegration to enable OSC 133"
    Write-Output "  prompt marks in PowerShell (Ctrl+Up / Ctrl+Down to jump"
    Write-Output "  between prompts inside kettle)."
}
Write-Output ""
Write-Output "To uninstall later: appwiz.cpl (Add/Remove Programs) or"
Write-Output "  powershell -File `"$Prefix\install.ps1`" -Uninstall"
