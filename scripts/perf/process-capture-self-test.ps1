$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. "$PSScriptRoot\process-capture.ps1"

function Assert-ProcessCaptureTest {
    param(
        [Parameter(Mandatory)]
        [bool]$Condition,
        [Parameter(Mandatory)]
        [string]$Message
    )
    if (-not $Condition) {
        throw $Message
    }
}

$shell = (Get-Process -Id $PID -ErrorAction Stop).Path
$common = @('-NoLogo', '-NoProfile', '-NonInteractive', '-Command')
$result = Invoke-KettlePerfBoundedProcess -FilePath $shell `
    -ArgumentList ($common + @(
        '[Console]::Out.Write("ok"); [Console]::Error.Write("note"); exit 7'
    )) -TimeoutMs 5000 -MaxStdoutBytes 64 -MaxStderrBytes 64
Assert-ProcessCaptureTest ($result.ExitCode -eq 7) `
    'bounded capture lost the process exit code'
Assert-ProcessCaptureTest ($result.StandardOutput -eq 'ok') `
    'bounded capture changed standard output'
Assert-ProcessCaptureTest ($result.StandardError -eq 'note') `
    'bounded capture changed standard error'

$timedOut = $false
try {
    Invoke-KettlePerfBoundedProcess -FilePath $shell `
        -ArgumentList ($common + @('Start-Sleep -Seconds 10')) `
        -TimeoutMs 100 -MaxStdoutBytes 64 -MaxStderrBytes 64 | Out-Null
} catch [TimeoutException] {
    $timedOut = $true
}
Assert-ProcessCaptureTest $timedOut `
    'bounded capture accepted a process after its timeout'

$overflowed = $false
try {
    Invoke-KettlePerfBoundedProcess -FilePath $shell `
        -ArgumentList ($common + @('[Console]::Out.Write(("x" * 4096))')) `
        -TimeoutMs 5000 -MaxStdoutBytes 64 -MaxStderrBytes 64 | Out-Null
} catch [IO.InvalidDataException] {
    $overflowed = $true
}
Assert-ProcessCaptureTest $overflowed `
    'bounded capture accepted standard output beyond its byte limit'

$stderrOverflowed = $false
try {
    Invoke-KettlePerfBoundedProcess -FilePath $shell `
        -ArgumentList ($common + @('[Console]::Error.Write(("x" * 4096))')) `
        -TimeoutMs 5000 -MaxStdoutBytes 64 -MaxStderrBytes 64 | Out-Null
} catch [IO.InvalidDataException] {
    $stderrOverflowed = $true
}
Assert-ProcessCaptureTest $stderrOverflowed `
    'bounded capture accepted standard error beyond its byte limit'

$invalidUtf8 = $false
try {
    Invoke-KettlePerfBoundedProcess -FilePath $shell `
        -ArgumentList ($common + @(
            '$s=[Console]::OpenStandardOutput();$b=[byte[]](255);$s.Write($b,0,1)'
        )) -TimeoutMs 5000 -MaxStdoutBytes 64 -MaxStderrBytes 64 | Out-Null
} catch {
    $invalidUtf8 = $_.Exception.ToString().Contains(
        'DecoderFallbackException'
    )
}
Assert-ProcessCaptureTest $invalidUtf8 `
    'bounded capture accepted invalid UTF-8'

$argumentScript = Join-Path ([IO.Path]::GetTempPath()) (
    'kettle-process-args-' + [Guid]::NewGuid().ToString('N') + '.ps1'
)
try {
    [IO.File]::WriteAllText(
        $argumentScript,
        (
            'param([string]$First,[string]$Second)' +
            '[Console]::Out.Write(("{0}:{1}" -f $First,$Second))'
        ),
        [Text.UTF8Encoding]::new($false)
    )
    $argumentResult = Invoke-KettlePerfBoundedProcess -FilePath $shell `
        -ArgumentList @(
            '-NoLogo', '-NoProfile', '-NonInteractive',
            '-File', $argumentScript, '', 'has space'
        ) -TimeoutMs 5000 -MaxStdoutBytes 64 -MaxStderrBytes 65536
    Assert-ProcessCaptureTest (
        $argumentResult.ExitCode -eq 0 -and
        $argumentResult.StandardOutput -eq ':has space'
    ) 'bounded capture changed an empty or quoted argument'
} finally {
    [IO.File]::Delete($argumentScript)
}

$parsed = ConvertFrom-KettlePerfBoundedJson `
    -Json '{"ok":true,"items":[1,2,3]}' -MaximumDepth 4 -MaximumTokens 16
Assert-ProcessCaptureTest (
    $parsed.ok -eq $true -and @($parsed.items).Count -eq 3
) 'bounded JSON parsing changed a valid object'

$tooDeep = $false
try {
    ConvertFrom-KettlePerfBoundedJson -Json '[[[0]]]' `
        -MaximumDepth 2 -MaximumTokens 16 | Out-Null
} catch {
    $tooDeep = $true
}
Assert-ProcessCaptureTest $tooDeep `
    'bounded JSON parsing accepted excessive nesting'

$duplicateProperty = $false
try {
    ConvertFrom-KettlePerfBoundedJson -Json '{"ok":true,"OK":false}' `
        -MaximumDepth 4 -MaximumTokens 16 | Out-Null
} catch {
    $duplicateProperty = $true
}
Assert-ProcessCaptureTest $duplicateProperty `
    'bounded JSON parsing accepted case-colliding properties'

$escapedProperty = $false
try {
    ConvertFrom-KettlePerfBoundedJson -Json '{"\u006fK":true}' `
        -MaximumDepth 4 -MaximumTokens 16 | Out-Null
} catch {
    $escapedProperty = $true
}
Assert-ProcessCaptureTest $escapedProperty `
    'bounded JSON parsing accepted an escaped property name'

$scratch = Join-Path ([IO.Path]::GetTempPath()) (
    'kettle-process-capture-' + [Guid]::NewGuid().ToString('N')
)
[void][IO.Directory]::CreateDirectory($scratch)
$pidPath = Join-Path $scratch 'child.pid'
try {
    $childScript = 'Start-Sleep -Seconds 30'
    $childEncoded = [Convert]::ToBase64String(
        [Text.Encoding]::Unicode.GetBytes($childScript)
    )
    $shellEncoded = [Convert]::ToBase64String(
        [Text.Encoding]::UTF8.GetBytes($shell)
    )
    $pidPathEncoded = [Convert]::ToBase64String(
        [Text.Encoding]::UTF8.GetBytes($pidPath)
    )
    $parentScript = @'
$shell=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('__SHELL__'))
$pidPath=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('__PIDPATH__'))
$child=Start-Process -FilePath $shell -ArgumentList @(
    '-NoLogo','-NoProfile','-NonInteractive','-EncodedCommand','__CHILD__'
) -PassThru
[IO.File]::WriteAllText($pidPath,[string]$child.Id)
Start-Sleep -Seconds 30
'@
    $parentScript = $parentScript.Replace('__SHELL__', $shellEncoded)
    $parentScript = $parentScript.Replace('__PIDPATH__', $pidPathEncoded)
    $parentScript = $parentScript.Replace('__CHILD__', $childEncoded)
    $parentEncoded = [Convert]::ToBase64String(
        [Text.Encoding]::Unicode.GetBytes($parentScript)
    )
    try {
        Invoke-KettlePerfBoundedProcess -FilePath $shell `
            -ArgumentList @(
                '-NoLogo', '-NoProfile', '-NonInteractive',
                '-EncodedCommand', $parentEncoded
            ) -TimeoutMs 12000 -MaxStdoutBytes 64 -MaxStderrBytes 65536 |
            Out-Null
        throw 'descendant cleanup fixture unexpectedly completed'
    } catch [TimeoutException] {
        # Expected: closing the helper job must terminate both processes.
    }
    Assert-ProcessCaptureTest ([IO.File]::Exists($pidPath)) `
        'descendant cleanup fixture did not record its child'
    $childPid = [int][IO.File]::ReadAllText($pidPath)
    Start-Sleep -Milliseconds 250
    $childAlive = $null -ne (
        Get-Process -Id $childPid -ErrorAction SilentlyContinue
    )
    Assert-ProcessCaptureTest (-not $childAlive) `
        'bounded capture left a descendant process alive'
} finally {
    if ([IO.Directory]::Exists($scratch)) {
        [IO.Directory]::Delete($scratch, $true)
    }
}

Write-Host 'process capture self-test: PASS'
