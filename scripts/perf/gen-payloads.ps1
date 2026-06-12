# Generate deterministic throughput payloads for the cross-terminal benchmark.
# Output: <dir>\ascii.txt (~16 MB plain text), <dir>\sgr.txt (~6 MB color-heavy),
#         <dir>\unicode.txt (~4 MB CJK/emoji/box-drawing mix).
# Sizes are chosen so even a slow terminal (sub-1 MB/s) finishes an iteration in
# bounded time; the runner adapts iteration count when a payload is slow.
# Deterministic content (no RNG) so every run and every terminal sees identical bytes.
param(
    [string]$Dir = "$PSScriptRoot\..\..\target\perf-payloads"
)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $Dir | Out-Null

$asciiPath = Join-Path $Dir 'ascii.txt'
$sgrPath = Join-Path $Dir 'sgr.txt'
$unicodePath = Join-Path $Dir 'unicode.txt'

function Test-PayloadStale([string]$Path, [long]$Target) {
    -not (Test-Path $Path) -or [Math]::Abs((Get-Item $Path).Length - $Target) -gt ($Target * 0.2)
}

if (Test-PayloadStale $asciiPath 17MB) {
    $sb = [System.Text.StringBuilder]::new(20MB)
    $line = '0123456789 the quick brown fox jumps over the lazy dog ABCDEFGHIJKLMNOPQRSTUVWXYZ ./usr/lib/x86_64 -rw-r--r-- 1 root 4096'
    for ($i = 0; $i -lt 128000; $i++) {
        [void]$sb.Append(('{0:d8} ' -f $i))
        [void]$sb.Append($line)
        [void]$sb.Append("`n")
    }
    [System.IO.File]::WriteAllText($asciiPath, $sb.ToString(), [System.Text.UTF8Encoding]::new($false))
}

if (Test-PayloadStale $sgrPath 6.1MB) {
    $e = [char]27
    $sb = [System.Text.StringBuilder]::new(10MB)
    for ($i = 0; $i -lt 48000; $i++) {
        for ($c = 0; $c -lt 8; $c++) {
            $fg = 30 + (($i + $c) % 8)
            $bg = 100 + (($i * 3 + $c) % 8)
            [void]$sb.Append("$e[$fg;${bg}m word$c ")
        }
        [void]$sb.Append("$e[0m`n")
    }
    [System.IO.File]::WriteAllText($sgrPath, $sb.ToString(), [System.Text.UTF8Encoding]::new($false))
}

if (Test-PayloadStale $unicodePath 4.3MB) {
    $sb = [System.Text.StringBuilder]::new(6MB)
    $row = '日本語テキスト 中文测试 한국어 ─━│┃┌┐└┘├┤ αβγδε ∑∏∫√ ▲►▼◄ 🚀🔥💧🌍 ABC abc 123 '
    for ($i = 0; $i -lt 30000; $i++) {
        [void]$sb.Append(('{0:d6} ' -f $i))
        [void]$sb.Append($row)
        [void]$sb.Append("`n")
    }
    [System.IO.File]::WriteAllText($unicodePath, $sb.ToString(), [System.Text.UTF8Encoding]::new($false))
}

Get-ChildItem $Dir | ForEach-Object { '{0,-12} {1,8:n1} MB' -f $_.Name, ($_.Length / 1MB) }
