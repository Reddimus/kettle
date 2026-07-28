#requires -Version 5.1
<#
.SYNOPSIS
    Verify packaging/windows/kettle.ico is a well-formed multi-resolution icon.

.DESCRIPTION
    Parses the ICONDIR header directly rather than shelling out to `file` or
    `xxd`, so the check needs nothing beyond PowerShell 5.1. Bytes 0-3 must be
    the ICO magic (00 00 01 00), and the little-endian image count at offset 4
    must be at least MinimumResolutions. Mirrors ci.yml's Windows-only
    "Packaging smoke - Windows .ico" step, which uses a `file`-based check with
    the same floor.

    This lives in a script rather than inline in the Justfile because a plain
    `just` recipe runs each line in its own shell, so variables assigned on one
    line are gone by the next.
#>
[CmdletBinding()]
param(
    # The release recipe bakes in six sizes; four is the floor CI also uses.
    [int] $MinimumResolutions = 4
)

$ErrorActionPreference = 'Stop'

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$icon = Join-Path $repo 'packaging/windows/kettle.ico'

if (-not (Test-Path -LiteralPath $icon -PathType Leaf)) {
    Write-Error "missing icon: $icon"
    exit 1
}

$bytes = [System.IO.File]::ReadAllBytes($icon)

# ICONDIR is six bytes: reserved (0), type (1 = icon), then the image count.
if ($bytes.Length -lt 6) {
    Write-Error "$icon is $($bytes.Length) bytes, too short to hold an ICONDIR header"
    exit 1
}

if ($bytes[0] -ne 0 -or $bytes[1] -ne 0 -or $bytes[2] -ne 1 -or $bytes[3] -ne 0) {
    $magic = ($bytes[0..3] | ForEach-Object { '{0:x2}' -f $_ }) -join ' '
    Write-Error "$icon is not a well-formed .ico (ICONDIR magic was $magic, expected 00 00 01 00)"
    exit 1
}

$count = $bytes[4] -bor ($bytes[5] -shl 8)
if ($count -lt $MinimumResolutions) {
    Write-Error "$icon has only $count resolution(s), expected at least $MinimumResolutions"
    exit 1
}

Write-Output "kettle.ico OK ($count resolutions)"
