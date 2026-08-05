# Every harness script must be pure ASCII.
#
# These scripts run under two engines: PowerShell 7, which reads a file without
# a BOM as UTF-8, and Windows PowerShell 5.1, which reads the same file as the
# system ANSI code page. A non-ASCII character therefore decodes differently
# depending on who is running it -- and for the punctuation that actually shows
# up in prose, the difference is not cosmetic. U+2014 EM DASH is `E2 80 94` in
# UTF-8, which Windows-1252 renders as three characters, the last of which is a
# double quote. Inside a comment that is merely ugly. Inside a quoted string it
# closes the string, and the parser then fails somewhere else entirely: a real
# instance reported `Unexpected token 'if'` two words later, and a missing
# string terminator forty lines below that.
#
# Nothing in a benchmark harness needs a character outside ASCII, so the
# simplest contract that cannot be got wrong is to forbid all of them rather
# than to reason per-file about whether a given one sits inside a string.
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = $PSScriptRoot
$scripts = @(
    Get-ChildItem -LiteralPath $root -Filter '*.ps1' -File |
        Sort-Object -Property Name
)
if ($scripts.Count -lt 10) {
    throw (
        "ASCII contract self-test found only $($scripts.Count) harness " +
        'scripts; expected the whole suite. Check $PSScriptRoot resolution ' +
        'before trusting a pass.'
    )
}

$offenders = [Collections.Generic.List[string]]::new()
foreach ($script in $scripts) {
    $bytes = [IO.File]::ReadAllBytes($script.FullName)
    if ($bytes.Length -ge 3 -and
        $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF) {
        $offenders.Add(
            "$($script.Name): starts with a UTF-8 BOM; the evidence files " +
            'this suite writes are BOM-free by contract and the scripts ' +
            'match them'
        )
        continue
    }
    $line = 1
    for ($i = 0; $i -lt $bytes.Length; $i++) {
        if ($bytes[$i] -eq 0x0A) {
            $line++
            continue
        }
        if ($bytes[$i] -gt 0x7F) {
            $offenders.Add(
                "$($script.Name):${line}: byte 0x$(
                    '{0:X2}' -f $bytes[$i]
                ) is outside ASCII; Windows PowerShell 5.1 will decode it " +
                'as the ANSI code page and may terminate a string early'
            )
            break
        }
    }
}

if ($offenders.Count -gt 0) {
    throw (
        "Harness scripts must be pure ASCII:`n  " +
        ($offenders -join "`n  ")
    )
}

# The check has to be able to fail. Prove it against a byte sequence built at
# runtime, so a future edit that turns the scan into a no-op is caught here
# rather than by CI on some later branch.
$probe = Join-Path ([IO.Path]::GetTempPath()) (
    'kettle-ascii-probe-' + [guid]::NewGuid().ToString('N') + '.ps1'
)
try {
    $emDash = [byte[]]@(0x23, 0x20, 0xE2, 0x80, 0x94, 0x0A)
    [IO.File]::WriteAllBytes($probe, $emDash)
    $probeBytes = [IO.File]::ReadAllBytes($probe)
    $flagged = $false
    foreach ($byte in $probeBytes) {
        if ($byte -gt 0x7F) { $flagged = $true; break }
    }
    if (-not $flagged) {
        throw 'ASCII contract self-test cannot detect a non-ASCII byte'
    }
    # And the specific hazard: this is what 5.1 sees in an em dash. The third
    # byte, 0x94, is a RIGHT DOUBLE QUOTATION MARK in Windows-1252, and the
    # PowerShell tokenizer accepts the smart quotes as string delimiters just
    # like the ASCII one -- which is how a dash inside a string closes it.
    $ansi = [Text.Encoding]::GetEncoding(1252).GetString($probeBytes)
    $quoteChars = @('"', [char]0x201C, [char]0x201D)
    if (-not ($quoteChars | Where-Object { $ansi.Contains($_) })) {
        throw (
            'The em-dash hazard this test exists for did not reproduce: ' +
            'Windows-1252 no longer decodes U+2014 to a quote character'
        )
    }
} finally {
    Remove-Item -LiteralPath $probe -Force -ErrorAction SilentlyContinue
}

Write-Output (
    "ASCII contract self-test: PASS ($($scripts.Count) harness scripts, " +
    'no BOMs, no bytes above 0x7F)'
)
