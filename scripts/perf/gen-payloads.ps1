# Generate deterministic throughput payloads for the cross-terminal benchmark.
# Output: <dir>\ascii.txt (~16 MB plain text), <dir>\sgr.txt (~6 MB color-heavy),
#         <dir>\unicode.txt (~4 MB CJK/emoji/box-drawing mix).
# Sizes are chosen so even a slow terminal (sub-1 MB/s) finishes an iteration in
# bounded time; the runner adapts iteration count when a payload is slow.
# Deterministic content (no RNG) so every run and every terminal sees identical bytes.
param(
    [string]$Dir = ''
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\payload-contract.ps1"
. "$PSScriptRoot\json-io.ps1"
if (-not $Dir) {
    $Dir = Join-Path $PSScriptRoot '..\..\target\perf-payloads'
}
$Dir = [IO.Path]::GetFullPath($Dir)
$payloadRoot = if ([IO.Directory]::Exists($Dir)) {
    Open-KettlePerfPersistenceRoot -Directory $Dir
} else {
    New-KettlePerfPersistenceRoot `
        -ParentDirectory ([IO.Path]::GetDirectoryName($Dir)) `
        -LeafName ([IO.Path]::GetFileName($Dir))
}

$asciiPath = Join-Path $Dir 'ascii.txt'
$sgrPath = Join-Path $Dir 'sgr.txt'
$unicodePath = Join-Path $Dir 'unicode.txt'

try {
if (-not (Test-KettlePerfPayloadFile -Path $asciiPath -Name ascii)) {
    $sb = [System.Text.StringBuilder]::new(20MB)
    $line = '0123456789 the quick brown fox jumps over the lazy dog ABCDEFGHIJKLMNOPQRSTUVWXYZ ./usr/lib/x86_64 -rw-r--r-- 1 root 4096'
    for ($i = 0; $i -lt 128000; $i++) {
        [void]$sb.Append(('{0:d8} ' -f $i))
        [void]$sb.Append($line)
        [void]$sb.Append("`n")
    }
    Write-KettlePerfUtf8File -Path $asciiPath -Text $sb.ToString() `
        -MaximumBytes 32MB -Root $payloadRoot
}

if (-not (Test-KettlePerfPayloadFile -Path $sgrPath -Name sgr)) {
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
    Write-KettlePerfUtf8File -Path $sgrPath -Text $sb.ToString() `
        -MaximumBytes 16MB -Root $payloadRoot
}

if (-not (Test-KettlePerfPayloadFile -Path $unicodePath -Name unicode)) {
    $sb = [System.Text.StringBuilder]::new(6MB)
    $row = '日本語テキスト 中文测试 한국어 ─━│┃┌┐└┘├┤ αβγδε ∑∏∫√ ▲►▼◄ 🚀🔥💧🌍 ABC abc 123 '
    for ($i = 0; $i -lt 30000; $i++) {
        [void]$sb.Append(('{0:d6} ' -f $i))
        [void]$sb.Append($row)
        [void]$sb.Append("`n")
    }
    Write-KettlePerfUtf8File -Path $unicodePath -Text $sb.ToString() `
        -MaximumBytes 16MB -Root $payloadRoot
}

foreach ($name in $KettlePerfPayloadContracts.Keys) {
    $path = Join-Path $Dir $KettlePerfPayloadContracts[$name].file
    if (-not (Test-KettlePerfPayloadFile -Path $path -Name $name)) {
        throw "Generated payload does not match its byte/hash contract: $path"
    }
}

Get-ChildItem $Dir | ForEach-Object { '{0,-12} {1,8:n1} MB' -f $_.Name, ($_.Length / 1MB) }
} finally {
    Close-KettlePerfPersistenceRoot $payloadRoot
}
