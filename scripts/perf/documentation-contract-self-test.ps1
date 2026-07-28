# Drift guard for release commands copied into user-facing documentation.
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
$paths = [string[]]@(
    (Join-Path $repoRoot 'scripts\perf\README.md'),
    (Join-Path $repoRoot 'docs\PERFORMANCE.md'),
    (Join-Path $repoRoot 'docs\TESTING.md')
)
$command = 'pwsh -NoLogo -NoProfile -File scripts/perf/score.ps1'

foreach ($path in $paths) {
    $text = [IO.File]::ReadAllText($path)
    $offset = 0
    $found = 0
    while ($true) {
        $index = $text.IndexOf(
            $command,
            $offset,
            [StringComparison]::Ordinal
        )
        if ($index -lt 0) {
            break
        }
        $found += 1
        $length = [Math]::Min(512, $text.Length - $index)
        $window = $text.Substring($index, $length)
        if ($window -notmatch '(?m)^\s*-Mode release\s*`\s*$') {
            throw (
                'Documented release scorer command omits -Mode release: {0}' -f
                    $path
            )
        }
        $offset = $index + $command.Length
    }
    if ($found -eq 0) {
        throw "Documented release scorer command is missing: $path"
    }
}

Write-Output 'Performance documentation release-mode contract passed.'
