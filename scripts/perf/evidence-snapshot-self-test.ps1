# GUI-free hostile-input tests for immutable performance-evidence snapshots.
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. "$PSScriptRoot\evidence-snapshot.ps1"
. "$PSScriptRoot\json-io.ps1"
. "$PSScriptRoot\vtebench-dat.ps1"

function Assert-KettlePerfEvidenceTest {
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

function Invoke-KettlePerfExpectedEvidenceFailure {
    param(
        [Parameter(Mandatory)]
        [string]$Description,
        [Parameter(Mandatory)]
        [scriptblock]$Action
    )

    $failed = $false
    try {
        & $Action
    } catch {
        $failed = $true
    }
    if (-not $failed) {
        throw "Expected evidence-snapshot failure was accepted: $Description"
    }
}

function Write-KettlePerfEvidenceTestUtf8 {
    param(
        [Parameter(Mandatory)]
        [string]$Path,
        [Parameter(Mandatory)]
        [string]$Text
    )

    [IO.File]::WriteAllText(
        $Path,
        $Text,
        [Text.UTF8Encoding]::new($false, $true)
    )
}

if (-not $script:KettlePerfEvidenceIsWindows) {
    Write-Output 'evidence-snapshot self-test: SKIP (Windows required)'
    return
}

$scratch = Join-Path ([IO.Path]::GetTempPath()) (
    'kettle-evidence-snapshot-' + [Guid]::NewGuid().ToString('N')
)
$literalRoot = Join-Path $scratch 'evidence[root]'
$renamedRoot = Join-Path $scratch 'renamed-root'
$emptyRoot = Join-Path $scratch 'empty-root'
$emptyMoved = Join-Path $scratch 'empty-moved'
$bulkRoot = Join-Path $scratch 'bulk-root'
$bulkMoved = Join-Path $scratch 'bulk-moved'
$junctionTarget = Join-Path $scratch 'junction-target'
$rootJunction = Join-Path $scratch 'root-junction'
$fileJunction = Join-Path $literalRoot 'linked.json'
$fileSymlink = Join-Path $literalRoot 'symlink.json'
$fileSymlinkTarget = Join-Path $scratch 'symlink-target.json'
$snapshot = $null
$emptySnapshot = $null
$limitedSnapshot = $null
$bulkSnapshot = $null
$rootJunctionCreated = $false
$fileJunctionCreated = $false
$fileSymlinkCreated = $false

[void][IO.Directory]::CreateDirectory($literalRoot)
[void][IO.Directory]::CreateDirectory($emptyRoot)
[void][IO.Directory]::CreateDirectory($bulkRoot)
[void][IO.Directory]::CreateDirectory($junctionTarget)
Write-KettlePerfEvidenceTestUtf8 `
    -Path (Join-Path $literalRoot 'good.json') `
    -Text '{"ok":true,"nested":{"value":1}}'
Write-KettlePerfEvidenceTestUtf8 `
    -Path (Join-Path $literalRoot 'duplicate.json') `
    -Text '{"a":1,"\u0061":2}'
Write-KettlePerfEvidenceTestUtf8 `
    -Path (Join-Path $literalRoot 'case.json') `
    -Text '{"a":1,"A":2}'
Write-KettlePerfEvidenceTestUtf8 `
    -Path (Join-Path $literalRoot 'deep.json') `
    -Text '{"a":{"b":{"c":1}}}'
Write-KettlePerfEvidenceTestUtf8 `
    -Path (Join-Path $literalRoot 'nodes.json') `
    -Text '{"a":1,"b":2}'
Write-KettlePerfEvidenceTestUtf8 `
    -Path (Join-Path $literalRoot 'oversize.json') `
    -Text '{"value":123456789}'
Write-KettlePerfEvidenceTestUtf8 `
    -Path (Join-Path $literalRoot 'invalid.json') `
    -Text '{"a":1} trailing'
Write-KettlePerfEvidenceTestUtf8 `
    -Path (Join-Path $literalRoot 'vtebench-kettle.dat') `
    -Text "bench_a bench_b`n1 2`n3 _`n"
[IO.File]::WriteAllBytes(
    (Join-Path $literalRoot 'bom.json'),
    [byte[]]@(0xef, 0xbb, 0xbf, 0x7b, 0x7d)
)
[IO.File]::WriteAllBytes(
    (Join-Path $literalRoot 'invalid-utf8.json'),
    [byte[]]@(
        0x7b, 0x22, 0x78, 0x22, 0x3a,
        0x22, 0xc3, 0x28, 0x22, 0x7d
    )
)
Write-KettlePerfEvidenceTestUtf8 `
    -Path $fileSymlinkTarget -Text '{"external":true}'
Write-KettlePerfEvidenceTestUtf8 `
    -Path (Join-Path $bulkRoot 'one.json') `
    -Text '{"generation":1}'
Write-KettlePerfEvidenceTestUtf8 `
    -Path (Join-Path $bulkRoot 'two.json') `
    -Text '{"generation":1}'
[IO.File]::WriteAllText(
    (Join-Path $bulkRoot 'ignored.txt'),
    'not JSON',
    [Text.UTF8Encoding]::new($false, $true)
)

try {
    try {
        New-Item -ItemType Junction -Path $rootJunction `
            -Target $junctionTarget -ErrorAction Stop | Out-Null
        $rootJunctionCreated = $true
    } catch {
        Write-Warning (
            'SKIP evidence root-junction regression: ' +
            $_.Exception.Message
        )
    }
    try {
        New-Item -ItemType Junction -Path $fileJunction `
            -Target $junctionTarget -ErrorAction Stop | Out-Null
        $fileJunctionCreated = $true
    } catch {
        Write-Warning (
            'SKIP evidence child-junction regression: ' +
            $_.Exception.Message
        )
    }
    try {
        New-Item -ItemType SymbolicLink -Path $fileSymlink `
            -Target $fileSymlinkTarget -ErrorAction Stop | Out-Null
        $fileSymlinkCreated = $true
    } catch {
        Write-Warning (
            'SKIP evidence file-symlink regression: ' +
            $_.Exception.Message
        )
    }

    if ($rootJunctionCreated) {
        Invoke-KettlePerfExpectedEvidenceFailure `
            -Description 'reparse-point evidence root' `
            -Action {
                $reparseSnapshot = Open-KettlePerfEvidenceSnapshot `
                    -Directory $rootJunction
                Close-KettlePerfEvidenceSnapshot $reparseSnapshot
            }
    }

    $bulkSnapshot = Open-KettlePerfEvidenceSnapshot `
        -Directory $bulkRoot -MaximumFiles 2 -MaximumTotalBytes 1KB
    $bulkNames = Get-KettlePerfEvidenceLeafNames `
        -Snapshot $bulkSnapshot -Extension '.json' -MaximumNames 2
    Assert-KettlePerfEvidenceTest (
        $bulkNames.Count -eq 2 -and
        $bulkNames[0] -ceq 'one.json' -and
        $bulkNames[1] -ceq 'two.json'
    ) 'Bounded bulk enumeration did not return the exact sorted JSON set'
    Invoke-KettlePerfExpectedEvidenceFailure `
        -Description 'bulk enumeration file-count bound' `
        -Action {
            Get-KettlePerfEvidenceLeafNames `
                -Snapshot $bulkSnapshot -Extension '.json' `
                -MaximumNames 1
        }
    $bulkEntries = Read-KettlePerfEvidenceJsonSet `
        -Snapshot $bulkSnapshot -LeafNames $bulkNames `
        -MaximumBytes 1KB -MaximumDepth 4 -MaximumTotalNodes 4
    Assert-KettlePerfEvidenceTest (
        $bulkEntries.Count -eq 2 -and
        $bulkEntries[0].value.generation -eq 1 -and
        $bulkEntries[1].value.generation -eq 1 -and
        $bulkSnapshot.native.OpenFileCount -eq 2
    ) 'Bulk evidence capture did not retain one coherent file set'

    $bulkOne = Join-Path $bulkRoot 'one.json'
    $bulkOneMoved = Join-Path $bulkRoot 'one-moved.json'
    $bulkReplacement = Join-Path $bulkRoot 'replacement.tmp'
    Write-KettlePerfEvidenceTestUtf8 `
        -Path $bulkReplacement -Text '{"generation":2}'
    Invoke-KettlePerfExpectedEvidenceFailure `
        -Description 'mutation of a bulk-held evidence file' `
        -Action {
            [IO.File]::WriteAllText(
                $bulkOne,
                '{"generation":2}',
                [Text.UTF8Encoding]::new($false, $true)
            )
        }
    Invoke-KettlePerfExpectedEvidenceFailure `
        -Description 'rename of a bulk-held evidence file' `
        -Action {
            [IO.File]::Move($bulkOne, $bulkOneMoved)
        }
    Invoke-KettlePerfExpectedEvidenceFailure `
        -Description 'replacement of a bulk-held evidence file' `
        -Action {
            [IO.File]::Replace($bulkReplacement, $bulkOne, $null)
        }
    Invoke-KettlePerfExpectedEvidenceFailure `
        -Description 'rename of a bulk-held evidence root' `
        -Action {
            [IO.Directory]::Move($bulkRoot, $bulkMoved)
        }
    $bulkNamesAfter = Get-KettlePerfEvidenceLeafNames `
        -Snapshot $bulkSnapshot -Extension '.json' -MaximumNames 2
    Assert-KettlePerfEvidenceTest (
        @(Compare-Object $bulkNames $bulkNamesAfter).Count -eq 0 -and
        $bulkEntries[0].value.generation -eq 1 -and
        $bulkEntries[1].value.generation -eq 1 -and
        (Test-Path -LiteralPath $bulkOne -PathType Leaf) -and
        -not (Test-Path -LiteralPath $bulkOneMoved) -and
        -not (Test-Path -LiteralPath $bulkMoved)
    ) 'Bulk snapshot allowed a root/file swap or mixed generations'
    Close-KettlePerfEvidenceSnapshot $bulkSnapshot
    $bulkSnapshot = $null
    if (Test-Path -LiteralPath $bulkReplacement) {
        [IO.File]::Delete($bulkReplacement)
    }

    $snapshot = Open-KettlePerfEvidenceSnapshot `
        -Directory $literalRoot
    Assert-KettlePerfEvidenceTest (
        [StringComparer]::OrdinalIgnoreCase.Equals(
            $snapshot.root_path,
            [IO.Path]::GetFullPath($literalRoot)
        )
    ) 'Wildcard-like literal root was not opened literally'

    $datEntry = Read-KettlePerfEvidenceText `
        -Snapshot $snapshot -LeafName 'vtebench-kettle.dat' -Required
    $dat = Read-KettlePerfVtebenchDatText `
        -Text $datEntry.text -ExpectedColumns 2 `
        -Source $datEntry.path
    Assert-KettlePerfEvidenceTest (
        $dat.SampleRows -eq 2 -and
        $dat.Samples.bench_a.Count -eq 2 -and
        $dat.Samples.bench_b.Count -eq 1 -and
        $dat.Samples.bench_a[1] -eq 3
    ) 'Held vtebench DAT text did not parse linearly'
    Invoke-KettlePerfExpectedEvidenceFailure `
        -Description 'vtebench DAT row bound' `
        -Action {
            Read-KettlePerfVtebenchDatText `
                -Text $datEntry.text -ExpectedColumns 2 `
                -MaximumRows 1 -Source $datEntry.path
        }

    Invoke-KettlePerfExpectedEvidenceFailure `
        -Description 'wildcard leaf name' `
        -Action {
            Read-KettlePerfEvidenceJson `
                -Snapshot $snapshot -LeafName '*.json'
        }
    Invoke-KettlePerfExpectedEvidenceFailure `
        -Description 'bracket-wildcard leaf name' `
        -Action {
            Read-KettlePerfEvidenceJson `
                -Snapshot $snapshot -LeafName '[good].json'
        }

    if ($fileJunctionCreated) {
        Invoke-KettlePerfExpectedEvidenceFailure `
            -Description 'reparse-point directory child' `
            -Action {
                Read-KettlePerfEvidenceJson `
                    -Snapshot $snapshot -LeafName 'linked.json' -Required
            }
    }
    if ($fileSymlinkCreated) {
        Invoke-KettlePerfExpectedEvidenceFailure `
            -Description 'reparse-point file child' `
            -Action {
                Read-KettlePerfEvidenceJson `
                    -Snapshot $snapshot -LeafName 'symlink.json' -Required
            }
    }

    foreach ($invalidLeaf in @(
        'bom.json',
        'invalid-utf8.json',
        'duplicate.json',
        'case.json',
        'invalid.json'
    )) {
        Invoke-KettlePerfExpectedEvidenceFailure `
            -Description "invalid evidence $invalidLeaf" `
            -Action {
                Read-KettlePerfEvidenceJson `
                    -Snapshot $snapshot -LeafName $invalidLeaf -Required
            }
    }

    Invoke-KettlePerfExpectedEvidenceFailure `
        -Description 'JSON depth bound' `
        -Action {
            Read-KettlePerfEvidenceJson `
                -Snapshot $snapshot -LeafName 'deep.json' `
                -MaximumDepth 3 -Required
        }
    $deep = Read-KettlePerfEvidenceJson `
        -Snapshot $snapshot -LeafName 'deep.json' `
        -MaximumDepth 4 -Required
    Assert-KettlePerfEvidenceTest (
        $deep.json_depth -eq 4 -and
        $deep.value.a.b.c -eq 1
    ) 'JSON depth accounting or cached retry is invalid'

    Invoke-KettlePerfExpectedEvidenceFailure `
        -Description 'JSON node bound' `
        -Action {
            Read-KettlePerfEvidenceJson `
                -Snapshot $snapshot -LeafName 'nodes.json' `
                -MaximumNodes 2 -Required
        }
    $nodes = Read-KettlePerfEvidenceJson `
        -Snapshot $snapshot -LeafName 'nodes.json' `
        -MaximumNodes 3 -Required
    Assert-KettlePerfEvidenceTest (
        $nodes.json_nodes -eq 3
    ) 'JSON node accounting or cached retry is invalid'

    $oversizePath = Join-Path $literalRoot 'oversize.json'
    $oversizeLength = (Get-Item -LiteralPath $oversizePath).Length
    Invoke-KettlePerfExpectedEvidenceFailure `
        -Description 'per-file byte bound' `
        -Action {
            Read-KettlePerfEvidenceJson `
                -Snapshot $snapshot -LeafName 'oversize.json' `
                -MaximumBytes ($oversizeLength - 1) -Required
        }
    $oversize = Read-KettlePerfEvidenceJson `
        -Snapshot $snapshot -LeafName 'oversize.json' `
        -MaximumBytes $oversizeLength -Required
    Assert-KettlePerfEvidenceTest (
        $oversize.bytes -eq $oversizeLength
    ) 'Bounded evidence retry did not retain the exact byte count'

    $goodPath = Join-Path $literalRoot 'good.json'
    $first = Read-KettlePerfEvidenceJson `
        -Snapshot $snapshot -LeafName 'good.json' -Required
    Invoke-KettlePerfExpectedEvidenceFailure `
        -Description 'mutation of a held evidence file' `
        -Action {
            [IO.File]::WriteAllText($goodPath, '{"changed":true}')
        }
    Invoke-KettlePerfExpectedEvidenceFailure `
        -Description 'deletion of a held evidence file' `
        -Action {
            [IO.File]::Delete($goodPath)
        }
    Invoke-KettlePerfExpectedEvidenceFailure `
        -Description 'atomic replacement of a held evidence file' `
        -Action {
            Write-KettlePerfJsonFile -Path $goodPath `
                -InputObject ([ordered]@{ changed = $true })
        }
    Invoke-KettlePerfExpectedEvidenceFailure `
        -Description 'rename of a held evidence root' `
        -Action {
            [IO.Directory]::Move($literalRoot, $renamedRoot)
        }
    $second = Read-KettlePerfEvidenceJson `
        -Snapshot $snapshot -LeafName 'good.json' -Required
    Assert-KettlePerfEvidenceTest (
        [object]::ReferenceEquals($first, $second) -and
        $second.sha256 -ceq $first.sha256 -and
        $second.value.ok -eq $true
    ) 'Repeated evidence read did not return the identical cached entry'

    $scorePath = Join-Path $literalRoot 'score.json'
    Write-KettlePerfJsonFile -Path $scorePath `
        -InputObject ([ordered]@{ generation = 1 })
    Write-KettlePerfJsonFile -Path $scorePath `
        -InputObject ([ordered]@{ generation = 2 })
    $publishedScore = Get-Content -Raw -LiteralPath $scorePath |
        ConvertFrom-Json
    Assert-KettlePerfEvidenceTest (
        $publishedScore.generation -eq 2
    ) 'Held evidence root blocked atomic direct-child publication'

    $missingFirst = Read-KettlePerfEvidenceJson `
        -Snapshot $snapshot -LeafName 'missing.json'
    Assert-KettlePerfEvidenceTest (
        $null -eq $missingFirst
    ) 'Optional missing evidence did not return null'
    $missingCreated = $false
    try {
        Write-KettlePerfEvidenceTestUtf8 `
            -Path (Join-Path $literalRoot 'missing.json') `
            -Text '{"appeared":true}'
        $missingCreated = $true
    } catch [IO.IOException] {
        # A filesystem may make the retained directory read lock stronger than
        # NTFS. Either behavior is safe; cached absence is tested when creation
        # is supported.
    }
    if ($missingCreated) {
        $missingSecond = Read-KettlePerfEvidenceJson `
            -Snapshot $snapshot -LeafName 'missing.json'
        Assert-KettlePerfEvidenceTest (
            $null -eq $missingSecond
        ) 'A cached missing leaf observed a later filesystem entry'
    }

    $limitedSnapshot = Open-KettlePerfEvidenceSnapshot `
        -Directory $literalRoot -MaximumFiles 1
    $null = Read-KettlePerfEvidenceJson `
        -Snapshot $limitedSnapshot -LeafName 'good.json' -Required
    Invoke-KettlePerfExpectedEvidenceFailure `
        -Description 'snapshot leaf-count bound' `
        -Action {
            Read-KettlePerfEvidenceJson `
                -Snapshot $limitedSnapshot -LeafName 'nodes.json' -Required
        }
    Close-KettlePerfEvidenceSnapshot $limitedSnapshot
    $limitedSnapshot = $null

    $emptySnapshot = Open-KettlePerfEvidenceSnapshot `
        -Directory $emptyRoot
    Invoke-KettlePerfExpectedEvidenceFailure `
        -Description 'deletion of a held empty evidence root' `
        -Action {
            [IO.Directory]::Delete($emptyRoot, $false)
        }
    Invoke-KettlePerfExpectedEvidenceFailure `
        -Description 'rename of a held empty evidence root' `
        -Action {
            [IO.Directory]::Move($emptyRoot, $emptyMoved)
        }
    Close-KettlePerfEvidenceSnapshot $emptySnapshot
    $emptySnapshot = $null
    [IO.Directory]::Move($emptyRoot, $emptyMoved)
    [IO.Directory]::Move($emptyMoved, $emptyRoot)

    Close-KettlePerfEvidenceSnapshot $snapshot
    $snapshot = $null
    Write-KettlePerfEvidenceTestUtf8 `
        -Path $goodPath -Text '{"changed":true}'
    Assert-KettlePerfEvidenceTest (
        (Get-Content -Raw -LiteralPath $goodPath) -eq '{"changed":true}'
    ) 'Closing the snapshot did not release the evidence file lock'
} finally {
    Close-KettlePerfEvidenceSnapshot $bulkSnapshot
    Close-KettlePerfEvidenceSnapshot $limitedSnapshot
    Close-KettlePerfEvidenceSnapshot $emptySnapshot
    Close-KettlePerfEvidenceSnapshot $snapshot

    if ($fileSymlinkCreated -and (Test-Path -LiteralPath $fileSymlink)) {
        [IO.File]::Delete($fileSymlink)
    }
    if ($fileJunctionCreated -and (Test-Path -LiteralPath $fileJunction)) {
        [IO.Directory]::Delete($fileJunction, $false)
    }
    if ($rootJunctionCreated -and (Test-Path -LiteralPath $rootJunction)) {
        [IO.Directory]::Delete($rootJunction, $false)
    }
    $scratchFull = [IO.Path]::GetFullPath($scratch)
    $temporaryRoot = [IO.Path]::GetFullPath(
        [IO.Path]::GetTempPath()
    )
    if (-not $scratchFull.StartsWith(
        $temporaryRoot,
        [StringComparison]::OrdinalIgnoreCase
    )) {
        throw 'Refusing unsafe evidence-snapshot test cleanup'
    }
    if ([IO.Directory]::Exists($scratchFull)) {
        [IO.Directory]::Delete($scratchFull, $true)
    }
}

Write-Output (
    'evidence-snapshot self-test: PASS ' +
    "($($PSVersionTable.PSVersion))"
)
