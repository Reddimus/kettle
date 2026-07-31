# GUI-free security and lifecycle tests for comparator campaign acquisition.
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

. "$PSScriptRoot\setup-comparator-campaign.ps1"

function Assert-KettlePerfComparatorSetupTest {
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

function Invoke-KettlePerfComparatorSetupExpectedFailure {
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
    Assert-KettlePerfComparatorSetupTest $failed (
        "Expected comparator setup failure was accepted: $Description"
    )
}

function Get-KettlePerfComparatorSetupTestByteArray {
    param(
        [Parameter(Mandatory)]
        [string]$Text
    )

    return [Text.UTF8Encoding]::new($false, $true).GetBytes($Text)
}

function Write-KettlePerfComparatorSetupTestByteArray {
    param(
        [Parameter(Mandatory)]
        [string]$Path,
        [Parameter(Mandatory)]
        [byte[]]$Bytes
    )

    $parent = [IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($Path))
    if (-not [IO.Directory]::Exists($parent)) {
        [void][IO.Directory]::CreateDirectory($parent)
    }
    [IO.File]::WriteAllBytes($Path, $Bytes)
}

function New-KettlePerfComparatorSetupTestZip {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Creates only a random invocation-owned test archive.'
    )]
    param(
        [Parameter(Mandatory)]
        [string]$Path,
        [Parameter(Mandatory)]
        [Collections.IDictionary]$Entries,
        [Collections.IDictionary]$ExternalAttributes
    )

    Add-Type -AssemblyName System.IO.Compression -ErrorAction Stop
    $stream = [IO.FileStream]::new(
        $Path,
        [IO.FileMode]::CreateNew,
        [IO.FileAccess]::Write,
        [IO.FileShare]::None
    )
    $archive = $null
    try {
        $archive = [IO.Compression.ZipArchive]::new(
            $stream,
            [IO.Compression.ZipArchiveMode]::Create,
            $true
        )
        foreach ($name in $Entries.Keys) {
            $entry = $archive.CreateEntry(
                [string]$name,
                [IO.Compression.CompressionLevel]::Optimal
            )
            if (
                $null -ne $ExternalAttributes -and
                $ExternalAttributes.Contains([string]$name)
            ) {
                $entry.ExternalAttributes = [int](
                    $ExternalAttributes[[string]$name]
                )
            }
            $entryStream = $entry.Open()
            try {
                $bytes = [byte[]]$Entries[[string]$name]
                $entryStream.Write($bytes, 0, $bytes.Length)
            } finally {
                $entryStream.Dispose()
            }
        }
    } finally {
        if ($null -ne $archive) {
            $archive.Dispose()
        }
        $stream.Dispose()
    }
}

function Get-KettlePerfComparatorSetupTestEvidence {
    param(
        [Parameter(Mandatory)]
        [string]$Path
    )

    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    return [pscustomobject][ordered]@{
        bytes = [long]$item.Length
        sha256 = (
            Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256
        ).Hash.ToLowerInvariant()
    }
}

function Set-KettlePerfComparatorSetupTestTreeContract {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Creates and removes only a random test tree.'
    )]
    param(
        [Parameter(Mandatory)]
        $Terminal,
        [Parameter(Mandatory)]
        [Collections.IDictionary]$Files,
        [Parameter(Mandatory)]
        [string]$Scratch
    )

    $treeRoot = Join-Path $Scratch (
        'expected-' + [string]$Terminal.name
    )
    [void][IO.Directory]::CreateDirectory($treeRoot)
    try {
        foreach ($relative in $Files.Keys) {
            $path = Join-Path $treeRoot (
                ([string]$relative).Replace('/', '\')
            )
            Write-KettlePerfComparatorSetupTestByteArray `
                -Path $path -Bytes ([byte[]]$Files[$relative])
        }
        $aggregate = Get-KettlePerfComparatorSetupTreeAggregate `
            -StagingRoot $treeRoot
        $Terminal.source.asset.staged_file_count = (
            [int]$aggregate.staged_file_count
        )
        $Terminal.source.asset.staged_total_bytes = (
            [long]$aggregate.staged_total_bytes
        )
        $Terminal.source.asset.staged_tree_sha256 = (
            [string]$aggregate.staged_tree_sha256
        )
    } finally {
        if ([IO.Directory]::Exists($treeRoot)) {
            [IO.Directory]::Delete($treeRoot, $true)
        }
    }
}

function New-KettlePerfComparatorSetupTestCampaign {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Creates only an invocation-owned test fixture.'
    )]
    param(
        [Parameter(Mandatory)]
        [string]$TemplatePath,
        [Parameter(Mandatory)]
        [string]$SourceRoot,
        [Parameter(Mandatory)]
        [string]$AssetRoot,
        [Parameter(Mandatory)]
        [string]$Scratch,
        [ValidatePattern(
            '^windows-x86_64-[0-9]{8}T[0-9]{6}Z-[0-9a-f]{16}$'
        )]
        [string]$CampaignId = (
            'windows-x86_64-20260727T020000Z-f0123456789abcde'
        )
    )

    $campaign = (
        [IO.File]::ReadAllText($TemplatePath) | ConvertFrom-Json
    )
    $campaign.campaign_id = $CampaignId
    $campaign.selection.started_at_utc = '2026-07-27T02:00:00Z'
    $campaign.selection.completed_at_utc = '2026-07-27T02:01:00Z'

    $payloads = [ordered]@{
        alacritty = [ordered]@{
            'alacritty.exe' = (
                Get-KettlePerfComparatorSetupTestByteArray `
                    'MZ alacritty comparator fixture'
            )
        }
        wezterm = [ordered]@{
            'wezterm-gui.exe' = (
                Get-KettlePerfComparatorSetupTestByteArray `
                    'MZ wezterm comparator fixture'
            )
            'resources/helper.dat' = (
                Get-KettlePerfComparatorSetupTestByteArray `
                    'wezterm adjacent resource'
            )
        }
        rio = [ordered]@{
            'rio.exe' = (
                Get-KettlePerfComparatorSetupTestByteArray `
                    'MZ rio comparator fixture'
            )
        }
        tabby = [ordered]@{
            'Tabby.exe' = (
                Get-KettlePerfComparatorSetupTestByteArray `
                    'MZ tabby comparator fixture'
            )
            'resources/helper.dat' = (
                Get-KettlePerfComparatorSetupTestByteArray `
                    'tabby adjacent resource'
            )
        }
        wt = [ordered]@{
            'WindowsTerminal.exe' = (
                Get-KettlePerfComparatorSetupTestByteArray `
                    'MZ Windows Terminal comparator fixture'
            )
            'defaults.json' = (
                Get-KettlePerfComparatorSetupTestByteArray `
                    '{"profiles":[]}'
            )
        }
    }

    foreach ($terminal in $campaign.terminals) {
        $files = $payloads[[string]$terminal.name]
        $executableBytes = [byte[]]$files[
            [string]$terminal.executable.leaf
        ]
        $executableHash = Get-KettlePerfComparatorSetupSha256 `
            -Bytes $executableBytes
        $terminal.executable.bytes = [long]$executableBytes.Length
        $terminal.executable.sha256 = $executableHash
        Set-KettlePerfComparatorSetupTestTreeContract `
            -Terminal $terminal -Files $files -Scratch $Scratch

        $assetPath = Join-Path $AssetRoot (
            [string]$terminal.source.asset.name
        )
        if ($terminal.source.asset.kind -ceq 'direct-executable') {
            Write-KettlePerfComparatorSetupTestByteArray `
                -Path $assetPath -Bytes $executableBytes
        } else {
            $archiveEntries = [ordered]@{}
            $prefix = Get-KettlePerfComparatorSetupZipPrefix (
                [string]$terminal.source.asset.executable_entry
            )
            foreach ($relative in $files.Keys) {
                $archiveEntries["$prefix$relative"] = [byte[]]$files[$relative]
            }
            New-KettlePerfComparatorSetupTestZip `
                -Path $assetPath -Entries $archiveEntries
        }
        $assetEvidence = Get-KettlePerfComparatorSetupTestEvidence $assetPath
        $terminal.source.asset.bytes = [long]$assetEvidence.bytes
        $terminal.source.asset.sha256 = [string]$assetEvidence.sha256
    }

    $campaignDirectory = Join-Path $SourceRoot $campaign.campaign_id
    [void][IO.Directory]::CreateDirectory($campaignDirectory)
    $campaignPath = Join-Path $campaignDirectory 'campaign.json'
    [IO.File]::WriteAllText(
        $campaignPath,
        ($campaign | ConvertTo-Json -Depth 12),
        [Text.UTF8Encoding]::new($false, $true)
    )
    return [pscustomobject][ordered]@{
        campaign = $campaign
        path = $campaignPath
        payloads = $payloads
    }
}

$tempParent = [IO.Path]::GetFullPath(
    [IO.Path]::GetTempPath()
).TrimEnd('\', '/')
$testLeaf = (
    'kcs-' + [Guid]::NewGuid().ToString('N').Substring(0, 16)
)
$testRoot = Join-Path $tempParent $testLeaf
if (
    [IO.Directory]::Exists($testRoot) -or
    [IO.File]::Exists($testRoot)
) {
    throw 'Comparator setup test root already exists'
}
[void][IO.Directory]::CreateDirectory($testRoot)

try {
    $sourceRoot = Join-Path $testRoot 'source-campaigns'
    $assetRoot = Join-Path $testRoot 'fixture-assets'
    $scratch = Join-Path $testRoot 'scratch'
    $benchRoot = Join-Path (Join-Path $testRoot 'install') 'KettleBench'
    [void][IO.Directory]::CreateDirectory($sourceRoot)
    [void][IO.Directory]::CreateDirectory($assetRoot)
    [void][IO.Directory]::CreateDirectory($scratch)
    [void][IO.Directory]::CreateDirectory(
        [IO.Path]::GetDirectoryName($benchRoot)
    )

    $template = Join-Path $PSScriptRoot (
        'campaigns\windows-x86_64-20260727T012800Z-' +
        'd76cbf4b8173c691\campaign.json'
    )
    $fixture = New-KettlePerfComparatorSetupTestCampaign `
        -TemplatePath $template -SourceRoot $sourceRoot `
        -AssetRoot $assetRoot -Scratch $scratch
    $fetches = [Collections.Generic.List[string]]::new()
    $fetch = {
        param($Entry, [string]$Destination)

        [void]$fetches.Add([string]$Entry.name)
        $source = Join-Path $assetRoot (
            [string]$Entry.source.asset.name
        )
        if (-not [IO.File]::Exists($source)) {
            throw "Fixture comparator asset is missing: $source"
        }
        [IO.File]::Copy($source, $Destination, $false)
        if (-not [IO.File]::Exists($Destination)) {
            throw "Fixture comparator fetch did not create: $Destination"
        }
    }.GetNewClosure()
    $signature = {
        param($Entry, [string]$Path)

        [void]$Path
        return [pscustomobject][ordered]@{
            status = [string]$Entry.executable.authenticode_status
            signer_cert_sha256 = $Entry.executable.signer_cert_sha256
        }
    }

    $cleanupVictim = Join-Path $testRoot 'cleanup-victim'
    [void][IO.Directory]::CreateDirectory($cleanupVictim)
    $cleanupSentinel = Join-Path $cleanupVictim 'sentinel.txt'
    [IO.File]::WriteAllText($cleanupSentinel, 'retain me')
    $cleanupProbeState = [pscustomobject]@{
        temporary_parent = $null
        reparse_child = $null
    }
    $cleanupSwapProbe = {
        param([string]$TemporaryParent)

        $cleanupProbeState.temporary_parent = $TemporaryParent
        $cleanupProbeState.reparse_child = Join-Path (
            $TemporaryParent
        ) 'swapped-child'
        [void](New-Item -ItemType Junction `
            -Path $cleanupProbeState.reparse_child `
            -Target $cleanupVictim -ErrorAction Stop)
    }.GetNewClosure()
    $setupWarnings = @()
    $installed = Invoke-KettlePerfComparatorCampaignSetupCore `
        -CampaignPath $fixture.path -CampaignSourceRoot $sourceRoot `
        -KettleBenchRoot $benchRoot -FetchAsset $fetch `
        -SignatureProbe $signature `
        -BeforeTemporaryParentDelete $cleanupSwapProbe `
        -WarningVariable +setupWarnings
    Assert-KettlePerfComparatorSetupTest (
        $installed.schema -ceq
            'kettle-comparator-campaign-setup-v1' -and
        $installed.reused -eq $false -and
        $fetches.Count -eq 5 -and
        [IO.Directory]::Exists($installed.campaign_root)
    ) 'Comparator setup did not create one complete campaign'
    $temporaryTrees = @(
        [IO.Directory]::EnumerateDirectories(
            $benchRoot,
            '.campaign-setup-*',
            [IO.SearchOption]::TopDirectoryOnly
        )
    )
    Assert-KettlePerfComparatorSetupTest (
        $temporaryTrees.Count -eq 1 -and
        [StringComparer]::OrdinalIgnoreCase.Equals(
            $temporaryTrees[0],
            [string]$cleanupProbeState.temporary_parent
        ) -and
        [IO.Directory]::Exists([string]$cleanupProbeState.reparse_child) -and
        (
            [IO.File]::GetAttributes(
                [string]$cleanupProbeState.reparse_child
            ) -band [IO.FileAttributes]::ReparsePoint
        ) -ne 0 -and
        [IO.File]::ReadAllText($cleanupSentinel) -ceq 'retain me' -and
        ($setupWarnings -join "`n") -like (
            '*' + [string]$cleanupProbeState.temporary_parent + '*'
        )
    ) (
        'Comparator setup cleanup traversed or removed a swapped reparse ' +
        'child instead of retaining the exact temporary parent'
    )
    [IO.Directory]::Delete(
        [string]$cleanupProbeState.reparse_child,
        $false
    )
    [IO.Directory]::Delete(
        [string]$cleanupProbeState.temporary_parent,
        $false
    )
    Assert-KettlePerfComparatorSetupTest (
        [IO.File]::ReadAllText($cleanupSentinel) -ceq 'retain me' -and
        @(
            [IO.Directory]::EnumerateDirectories(
                $benchRoot,
                '.campaign-setup-*',
                [IO.SearchOption]::TopDirectoryOnly
            )
        ).Count -eq 0
    ) 'Comparator setup reparse regression cleanup modified external data'
    $emptyTemporaryParent = Join-Path $benchRoot (
        '.campaign-setup-' + [Guid]::NewGuid().ToString('N')
    )
    [void][IO.Directory]::CreateDirectory($emptyTemporaryParent)
    Remove-KettlePerfComparatorSetupEmptyParent `
        -Path $emptyTemporaryParent -ExpectedParent $benchRoot
    Assert-KettlePerfComparatorSetupTest (
        -not [IO.Directory]::Exists($emptyTemporaryParent) -and
        -not [IO.File]::Exists($emptyTemporaryParent)
    ) 'Comparator setup did not remove its exact empty temporary parent'

    $verified = Assert-KettlePerfComparatorCampaignInstallation `
        -CampaignRoot $installed.campaign_root `
        -CampaignsRoot $installed.campaigns_root `
        -ExpectedCampaignSha256 $installed.campaign.campaign_file.sha256 `
        -SignatureProbe $signature
    Assert-KettlePerfComparatorSetupTest (
        $verified.campaign_id -ceq $fixture.campaign.campaign_id
    ) 'Comparator full installation verification returned another campaign'

    $offlineFetch = {
        throw 'Offline reuse unexpectedly invoked the fetch boundary'
    }
    $reused = Invoke-KettlePerfComparatorCampaignSetupCore `
        -CampaignPath $fixture.path -CampaignSourceRoot $sourceRoot `
        -KettleBenchRoot $benchRoot -Offline -FetchAsset $offlineFetch `
        -SignatureProbe $signature
    Assert-KettlePerfComparatorSetupTest (
        $reused.reused -eq $true -and
        $reused.campaign.campaign_file.sha256 -ceq
            $installed.campaign.campaign_file.sha256
    ) 'Offline setup did not reuse the fully verified campaign'

    $weztermRoot = Get-KettlePerfComparatorSetupStagingDirectory `
        -CampaignRoot $installed.campaign_root `
        -Entry $installed.campaign.terminals[1]
    $weztermHelper = Join-Path $weztermRoot 'resources\helper.dat'
    $originalHelper = [IO.File]::ReadAllBytes($weztermHelper)
    [IO.File]::WriteAllBytes(
        $weztermHelper,
        (
            Get-KettlePerfComparatorSetupTestByteArray `
                'tampered adjacent resource'
        )
    )
    try {
        Invoke-KettlePerfComparatorSetupExpectedFailure `
            -Description 'tampered adjacent resource' -Action {
                [void](Invoke-KettlePerfComparatorCampaignSetupCore `
                    -CampaignPath $fixture.path `
                    -CampaignSourceRoot $sourceRoot `
                    -KettleBenchRoot $benchRoot -Offline `
                    -SignatureProbe $signature)
            }
    } finally {
        [IO.File]::WriteAllBytes($weztermHelper, $originalHelper)
        [Array]::Clear($originalHelper, 0, $originalHelper.Length)
    }

    $extraPath = Join-Path $weztermRoot 'resources\extra.dat'
    [IO.File]::WriteAllText($extraPath, 'extra')
    try {
        Invoke-KettlePerfComparatorSetupExpectedFailure `
            -Description 'extra staged file' -Action {
                [void](Invoke-KettlePerfComparatorCampaignSetupCore `
                    -CampaignPath $fixture.path `
                    -CampaignSourceRoot $sourceRoot `
                    -KettleBenchRoot $benchRoot -Offline `
                    -SignatureProbe $signature)
            }
    } finally {
        [IO.File]::Delete($extraPath)
    }

    $weztermExecutable = Join-Path $weztermRoot 'wezterm-gui.exe'
    $hardlinkBytes = [IO.File]::ReadAllBytes($weztermHelper)
    [IO.File]::Delete($weztermHelper)
    [void](New-Item -ItemType HardLink -Path $weztermHelper `
        -Target $weztermExecutable -ErrorAction Stop)
    try {
        Invoke-KettlePerfComparatorSetupExpectedFailure `
            -Description 'hard-linked staged file' -Action {
                [void](Invoke-KettlePerfComparatorCampaignSetupCore `
                    -CampaignPath $fixture.path `
                    -CampaignSourceRoot $sourceRoot `
                    -KettleBenchRoot $benchRoot -Offline `
                    -SignatureProbe $signature)
            }
    } finally {
        [IO.File]::Delete($weztermHelper)
        [IO.File]::WriteAllBytes($weztermHelper, $hardlinkBytes)
        [Array]::Clear($hardlinkBytes, 0, $hardlinkBytes.Length)
    }

    $missingRoot = Join-Path (
        Join-Path $testRoot 'offline-missing'
    ) 'KettleBench'
    [void][IO.Directory]::CreateDirectory(
        [IO.Path]::GetDirectoryName($missingRoot)
    )
    Invoke-KettlePerfComparatorSetupExpectedFailure `
        -Description 'offline campaign absent' -Action {
            [void](Invoke-KettlePerfComparatorCampaignSetupCore `
                -CampaignPath $fixture.path `
                -CampaignSourceRoot $sourceRoot `
                -KettleBenchRoot $missingRoot -Offline `
                -SignatureProbe $signature)
        }

    Invoke-KettlePerfComparatorSetupExpectedFailure `
        -Description 'non-HTTPS upstream asset' -Action {
            [void](Assert-KettlePerfComparatorSetupOfficialUri `
                -Uri 'http://github.com/example/release.exe')
        }
    Invoke-KettlePerfComparatorSetupExpectedFailure `
        -Description 'unapproved redirect host' -Action {
            [void](Assert-KettlePerfComparatorSetupOfficialUri `
                -Uri 'https://example.com/release.exe' -Redirect)
        }

    $badZip = Join-Path $scratch 'traversal.zip'
    New-KettlePerfComparatorSetupTestZip -Path $badZip -Entries ([ordered]@{
        'Tabby.exe' = (
            Get-KettlePerfComparatorSetupTestByteArray `
                'MZ traversal fixture'
        )
        '../escaped.txt' = (
            Get-KettlePerfComparatorSetupTestByteArray 'escape'
        )
    })
    $badDestination = Join-Path $scratch 'traversal-output'
    [void][IO.Directory]::CreateDirectory($badDestination)
    Invoke-KettlePerfComparatorSetupExpectedFailure `
        -Description 'ZIP traversal entry' -Action {
            Expand-KettlePerfComparatorSetupZip `
                -ArchivePath $badZip -Destination $badDestination `
                -ExecutableEntry 'Tabby.exe'
        }
    Assert-KettlePerfComparatorSetupTest (
        @([IO.Directory]::EnumerateFileSystemEntries(
            $badDestination
        )).Count -eq 0 -and
        -not [IO.File]::Exists((Join-Path $scratch 'escaped.txt'))
    ) 'ZIP traversal validation wrote output before rejecting the archive'

    $linkZip = Join-Path $scratch 'symlink.zip'
    [int]$linkBits = -1577123840
    New-KettlePerfComparatorSetupTestZip -Path $linkZip `
        -Entries ([ordered]@{
            'Tabby.exe' = (
                Get-KettlePerfComparatorSetupTestByteArray 'MZ link fixture'
            )
            'link' = (
                Get-KettlePerfComparatorSetupTestByteArray 'Tabby.exe'
            )
        }) -ExternalAttributes ([ordered]@{
            link = $linkBits
        })
    $linkDestination = Join-Path $scratch 'symlink-output'
    [void][IO.Directory]::CreateDirectory($linkDestination)
    Invoke-KettlePerfComparatorSetupExpectedFailure `
        -Description 'ZIP symbolic-link entry' -Action {
            Expand-KettlePerfComparatorSetupZip `
                -ArchivePath $linkZip -Destination $linkDestination `
                -ExecutableEntry 'Tabby.exe'
        }
    Assert-KettlePerfComparatorSetupTest (
        @([IO.Directory]::EnumerateFileSystemEntries(
            $linkDestination
        )).Count -eq 0
    ) 'ZIP link validation wrote output before rejecting the archive'

    $tabby = $installed.campaign.terminals[3]
    $tabbyPath = Join-Path (
        Get-KettlePerfComparatorSetupStagingDirectory `
            -CampaignRoot $installed.campaign_root -Entry $tabby
    ) 'Tabby.exe'
    Invoke-KettlePerfComparatorSetupExpectedFailure `
        -Description 'signature identity mismatch' -Action {
            [void](Assert-KettlePerfComparatorSetupSignature `
                -Entry $tabby -Path $tabbyPath -SignatureProbe {
                    [pscustomobject]@{
                        status = 'NotSigned'
                        signer_cert_sha256 = $null
                    }
                })
        }

    $failureSourceRoot = Join-Path $testRoot 'failure-source-campaigns'
    $failureAssetRoot = Join-Path $testRoot 'failure-fixture-assets'
    [void][IO.Directory]::CreateDirectory($failureSourceRoot)
    [void][IO.Directory]::CreateDirectory($failureAssetRoot)
    $failureFixture = New-KettlePerfComparatorSetupTestCampaign `
        -TemplatePath $template -SourceRoot $failureSourceRoot `
        -AssetRoot $failureAssetRoot -Scratch $scratch `
        -CampaignId 'windows-x86_64-20260727T020000Z-a0123456789abcde'
    $failureWarnings = @()
    $acquisitionFailed = $false
    try {
        [void](Invoke-KettlePerfComparatorCampaignSetupCore `
            -CampaignPath $failureFixture.path `
            -CampaignSourceRoot $failureSourceRoot `
            -KettleBenchRoot $benchRoot -FetchAsset {
                throw 'intentional comparator acquisition failure'
            } -SignatureProbe $signature `
            -WarningVariable +failureWarnings)
    } catch {
        $acquisitionFailed = (
            $_.Exception.Message -like (
                '*intentional comparator acquisition failure*'
            )
        )
    }
    $failureTemporaryTrees = @(
        [IO.Directory]::EnumerateDirectories(
            $benchRoot,
            '.campaign-setup-*',
            [IO.SearchOption]::TopDirectoryOnly
        )
    )
    Assert-KettlePerfComparatorSetupTest (
        $acquisitionFailed -and
        $failureTemporaryTrees.Count -eq 1 -and
        [IO.Path]::GetFileName($failureTemporaryTrees[0]) -cmatch
            '^\.campaign-setup-[0-9a-f]{32}$' -and
        ($failureWarnings -join "`n") -like (
            '*retained; no recursive cleanup*' +
            $failureTemporaryTrees[0] + '*'
        )
    ) (
        'Failed comparator acquisition did not retain and identify its ' +
        'bounded random temporary tree'
    )

    Write-Output 'setup-comparator-campaign-self-test: PASS'
} finally {
    $fullTestRoot = [IO.Path]::GetFullPath($testRoot)
    $expectedPrefix = [IO.Path]::GetFullPath(
        (Join-Path $tempParent 'kcs-')
    )
    if (
        -not $fullTestRoot.StartsWith(
            $expectedPrefix,
            [StringComparison]::OrdinalIgnoreCase
        ) -or
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            [IO.Path]::GetDirectoryName($fullTestRoot),
            $tempParent
        )
    ) {
        throw 'Refusing unsafe comparator setup test cleanup'
    }
    if ([IO.Directory]::Exists($fullTestRoot)) {
        [IO.Directory]::Delete($fullTestRoot, $true)
    }
}
