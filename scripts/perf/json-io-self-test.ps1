$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. "$PSScriptRoot\json-io.ps1"

function Assert-KettlePerfJsonIoTest {
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

function Invoke-KettlePerfJsonIoExpectedFailure {
    param(
        [Parameter(Mandatory)]
        [scriptblock]$Action,
        [Parameter(Mandatory)]
        [string]$Description
    )
    $failed = $false
    try {
        & $Action
    } catch {
        $failed = $true
    }
    Assert-KettlePerfJsonIoTest $failed `
        "JSON persistence accepted $Description"
}

$scratch = Join-Path ([IO.Path]::GetTempPath()) (
    'kettle-json-io-' + [Guid]::NewGuid().ToString('N')
)
$parent = Join-Path $scratch 'parent'
$outside = Join-Path $scratch 'outside'
[void][IO.Directory]::CreateDirectory($parent)
[void][IO.Directory]::CreateDirectory($outside)
$root = $null
try {
    $root = New-KettlePerfPersistenceRoot `
        -ParentDirectory $parent -LeafName 'run'
    $resultPath = Join-Path $root.RootPath 'result.json'
    Write-KettlePerfJsonFile -Path $resultPath `
        -InputObject ([ordered]@{ generation = 1 }) -Root $root
    Write-KettlePerfJsonFile -Path $resultPath `
        -InputObject ([ordered]@{ generation = 2 }) -Root $root
    $value = Get-Content -Raw -LiteralPath $resultPath |
        ConvertFrom-Json -ErrorAction Stop
    Assert-KettlePerfJsonIoTest ($value.generation -eq 2) `
        'Atomic replacement did not publish the complete second document'

    $held = [IO.FileStream]::new(
        $resultPath,
        [IO.FileMode]::Open,
        [IO.FileAccess]::Read,
        [IO.FileShare]::Read
    )
    try {
        Invoke-KettlePerfJsonIoExpectedFailure `
            -Description 'replacement of a no-delete held output leaf' `
            -Action {
                Write-KettlePerfJsonFile -Path $resultPath `
                    -InputObject ([ordered]@{ generation = 3 }) -Root $root
            }
    } finally {
        $held.Dispose()
    }
    $retained = Get-Content -Raw -LiteralPath $resultPath |
        ConvertFrom-Json -ErrorAction Stop
    Assert-KettlePerfJsonIoTest ($retained.generation -eq 2) `
        'A failed atomic replacement changed the held output'

    $moved = Join-Path $parent 'run-moved'
    Invoke-KettlePerfJsonIoExpectedFailure `
        -Description 'rename of a retained output root' `
        -Action {
            [IO.Directory]::Move($root.RootPath, $moved)
        }
    Invoke-KettlePerfJsonIoExpectedFailure `
        -Description 'creation over an existing empty-or-populated run label' `
        -Action {
            $duplicate = New-KettlePerfPersistenceRoot `
                -ParentDirectory $parent -LeafName 'run'
            Close-KettlePerfPersistenceRoot $duplicate
        }

    $otherRootPath = Join-Path $parent 'other'
    [void][IO.Directory]::CreateDirectory($otherRootPath)
    Invoke-KettlePerfJsonIoExpectedFailure `
        -Description 'publication outside the supplied retained root' `
        -Action {
            Write-KettlePerfJsonFile `
                -Path (Join-Path $otherRootPath 'outside.json') `
                -InputObject ([ordered]@{ unsafe = $true }) -Root $root
        }

    $junctionPath = Join-Path $parent 'junction-root'
    $junction = New-Item -ItemType Junction -Path $junctionPath `
        -Target $outside -ErrorAction Stop
    try {
        Invoke-KettlePerfJsonIoExpectedFailure `
            -Description 'an output-root junction' `
            -Action {
                $junctionRoot = Open-KettlePerfPersistenceRoot `
                    -Directory $junctionPath
                Close-KettlePerfPersistenceRoot $junctionRoot
            }
    } finally {
        $junction.Delete()
    }

    $sentinel = Join-Path $outside 'sentinel.txt'
    [IO.File]::WriteAllText(
        $sentinel,
        'retain me',
        [Text.UTF8Encoding]::new($false)
    )
    $leafJunction = Join-Path $root.RootPath 'redirect.json'
    $leafLink = New-Item -ItemType Junction -Path $leafJunction `
        -Target $outside -ErrorAction Stop
    try {
        Invoke-KettlePerfJsonIoExpectedFailure `
            -Description 'a preplaced directory-junction output leaf' `
            -Action {
                Write-KettlePerfJsonFile -Path $leafJunction `
                    -InputObject ([ordered]@{ unsafe = $true }) -Root $root
            }
        Assert-KettlePerfJsonIoTest (
            [IO.File]::ReadAllText($sentinel) -ceq 'retain me'
        ) 'A preplaced output junction redirected a write outside the root'
    } finally {
        $leafLink.Delete()
    }

    $hardLinkPath = Join-Path $root.RootPath 'hard-link.json'
    [void](New-Item -ItemType HardLink -Path $hardLinkPath `
        -Target $sentinel -ErrorAction Stop)
    Write-KettlePerfJsonFile -Path $hardLinkPath `
        -InputObject ([ordered]@{ safe = $true }) -Root $root
    Assert-KettlePerfJsonIoTest (
        [IO.File]::ReadAllText($sentinel) -ceq 'retain me'
    ) 'A preplaced hard link redirected output outside the root'
    $hardLinkValue = Get-Content -Raw -LiteralPath $hardLinkPath |
        ConvertFrom-Json -ErrorAction Stop
    Assert-KettlePerfJsonIoTest ($hardLinkValue.safe -eq $true) `
        'Atomic publication did not replace a preplaced hard-link entry'

    $linkPath = Join-Path $root.RootPath 'file-link.json'
    $fileSymlinkAvailable = $false
    try {
        $fileLink = New-Item -ItemType SymbolicLink -Path $linkPath `
            -Target $sentinel -ErrorAction Stop
        $fileSymlinkAvailable = $true
        Write-KettlePerfJsonFile -Path $linkPath `
            -InputObject ([ordered]@{ safe = $true }) -Root $root
        $linkItem = Get-Item -LiteralPath $linkPath -Force
        Assert-KettlePerfJsonIoTest (
            ($linkItem.Attributes -band
                [IO.FileAttributes]::ReparsePoint) -eq 0
        ) 'Atomic publication retained a preplaced file symlink'
        Assert-KettlePerfJsonIoTest (
            [IO.File]::ReadAllText($sentinel) -ceq 'retain me'
        ) 'A preplaced file symlink redirected output outside the root'
    } catch [System.UnauthorizedAccessException] {
        Write-Warning (
            'File-symlink publication case skipped because this session ' +
            'cannot create symbolic links.'
        )
    } finally {
        if (
            $fileSymlinkAvailable -and
            (Test-Path -LiteralPath $linkPath)
        ) {
            [IO.File]::Delete($linkPath)
        }
    }

    $transients = @(
        Get-ChildItem -LiteralPath $root.RootPath -Force |
            Where-Object { $_.Name -like '.*.tmp' }
    )
    Assert-KettlePerfJsonIoTest ($transients.Count -eq 0) `
        'Failed or successful publication left a transient file'

    Close-KettlePerfPersistenceRoot $root
    $root = $null
    [IO.Directory]::Move((Join-Path $parent 'run'), $moved)
    Assert-KettlePerfJsonIoTest ([IO.Directory]::Exists($moved)) `
        'Closing the output-root lease did not release rename protection'
} finally {
    Close-KettlePerfPersistenceRoot $root
    if ([IO.Directory]::Exists($scratch)) {
        [IO.Directory]::Delete($scratch, $true)
    }
}

Write-Host 'JSON persistence self-test: PASS'
