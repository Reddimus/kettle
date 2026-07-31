# GUI-free tests for the locked throughput GO signal.
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. "$PSScriptRoot\go-signal.ps1"

function Assert-GoSignal {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) {
        throw $Message
    }
}

$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$testRoot = Join-Path $tempRoot (
    'kettle-go-signal-selftest-' + [Guid]::NewGuid().ToString('N')
)
[void][IO.Directory]::CreateDirectory($testRoot)
try {
    $descriptor = New-KettlePerfGoDescriptor -Directory $testRoot
    Assert-GoSignal (
        -not (Test-Path -LiteralPath $descriptor.Path)
    ) 'descriptor pre-created its signal'
    $lock = Publish-KettlePerfGoSignal -Descriptor $descriptor
    try {
        $waitMs = Wait-KettlePerfGoSignal `
            -Path $descriptor.Path -Directory $testRoot `
            -Token $descriptor.Token -TimeoutSeconds 2
        Assert-GoSignal ($waitMs -ge 0.0) 'published signal was not observed'
        $overwriteRejected = $false
        try {
            $writer = [IO.FileStream]::new(
                $descriptor.Path,
                [IO.FileMode]::Open,
                [IO.FileAccess]::Write,
                [IO.FileShare]::ReadWrite
            )
            $writer.Dispose()
        } catch {
            $overwriteRejected = $true
        }
        Assert-GoSignal $overwriteRejected (
            'retained GO signal lock allowed an overwrite'
        )
    } finally {
        Close-KettlePerfGoSignal `
            -Descriptor $descriptor -Lock $lock
    }
    Assert-GoSignal (
        -not (Test-Path -LiteralPath $descriptor.Path)
    ) 'GO signal cleanup left its exact file behind'

    $precreated = New-KettlePerfGoDescriptor -Directory $testRoot
    [IO.File]::WriteAllText(
        $precreated.Path,
        $precreated.Token,
        [Text.UTF8Encoding]::new($false)
    )
    $createNewRejected = $false
    try {
        [void](Publish-KettlePerfGoSignal -Descriptor $precreated)
    } catch {
        $createNewRejected = $true
    }
    Assert-GoSignal $createNewRejected 'pre-created GO signal was accepted'
    [IO.File]::Delete($precreated.Path)

    $tampered = New-KettlePerfGoDescriptor -Directory $testRoot
    [IO.File]::WriteAllText(
        $tampered.Path,
        ('0' * 64),
        [Text.UTF8Encoding]::new($false)
    )
    $tamperRejected = $false
    try {
        [void](Wait-KettlePerfGoSignal `
            -Path $tampered.Path -Directory $testRoot `
            -Token $tampered.Token -TimeoutSeconds 1)
    } catch {
        $tamperRejected = $true
    }
    Assert-GoSignal $tamperRejected 'tampered GO signal was accepted'
    [IO.File]::Delete($tampered.Path)

    $escapeRejected = $false
    try {
        [void](Assert-KettlePerfGoPath `
            -Path (Join-Path $tempRoot (
                'throughput-go-' + ('1' * 32) + '.signal'
            )) -Directory $testRoot)
    } catch {
        $escapeRejected = $true
    }
    Assert-GoSignal $escapeRejected 'GO signal path escape was accepted'
} finally {
    $resolvedTestRoot = [IO.Path]::GetFullPath($testRoot)
    $tempPrefix = $tempRoot.TrimEnd([char[]]@('\', '/')) +
        [IO.Path]::DirectorySeparatorChar
    if (
        -not $resolvedTestRoot.StartsWith(
            $tempPrefix,
            [StringComparison]::OrdinalIgnoreCase
        ) -or
        -not [IO.Path]::GetFileName($resolvedTestRoot).StartsWith(
            'kettle-go-signal-selftest-',
            [StringComparison]::Ordinal
        )
    ) {
        throw 'Refusing unsafe GO self-test cleanup'
    }
    if (Test-Path -LiteralPath $resolvedTestRoot -PathType Container) {
        $item = Get-Item -LiteralPath $resolvedTestRoot -Force
        if (
            ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0
        ) {
            throw 'Refusing GO self-test cleanup through a reparse point'
        }
        [IO.Directory]::Delete($resolvedTestRoot, $true)
    }
}

Write-Host "go-signal self-test: PASS ($($PSVersionTable.PSVersion))"
