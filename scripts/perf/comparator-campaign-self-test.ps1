# GUI-free adversarial tests for the Windows comparator campaign boundary.
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

. "$PSScriptRoot\comparator-campaign.ps1"

function Assert-KettlePerfComparatorCampaignTest {
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

function Write-KettlePerfComparatorCampaignTestUtf8 {
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

function New-KettlePerfComparatorCampaignFixture {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Returns only an in-memory test fixture.'
    )]
    param(
        [Parameter(Mandatory)]
        [string]$Nonce
    )

    $id = "windows-x86_64-20260726T224234Z-$Nonce"
    return [ordered]@{
        schema = 'kettle-windows-comparator-campaign-v1'
        campaign_id = $id
        platform = [ordered]@{
            os = 'windows'
            architecture = 'x86_64'
        }
        selection = [ordered]@{
            policy = 'official-stable-pinned-assets-v1'
            started_at_utc = '2026-07-26T22:42:34Z'
            completed_at_utc = '2026-07-26T22:44:34Z'
        }
        terminals = [object[]]@(
            [ordered]@{
                name = 'alacritty'
                role = 'confirmed'
                version = '0.17.0'
                source = [ordered]@{
                    origin = 'https://github.com/alacritty/alacritty'
                    package = 'Alacritty.Alacritty'
                    release_tag = 'v0.17.0'
                    asset = [ordered]@{
                        kind = 'direct-executable'
                        name = 'Alacritty-v0.17.0-portable.exe'
                        url = (
                            'https://github.com/alacritty/alacritty/' +
                            'releases/download/v0.17.0/' +
                            'Alacritty-v0.17.0-portable.exe'
                        )
                        bytes = 16000000
                        sha256 = ('a' * 64)
                        executable_entry = (
                            'Alacritty-v0.17.0-portable.exe'
                        )
                        staged_file_count = 1
                        staged_total_bytes = 11000000
                        staged_tree_sha256 = ('6' * 64)
                    }
                }
                executable = [ordered]@{
                    leaf = 'alacritty.exe'
                    staging_path = (
                        'staging/alacritty/0.17.0/alacritty.exe'
                    )
                    bytes = 11000000
                    sha256 = ('1' * 64)
                    authenticode_status = 'NotSigned'
                    signer_cert_sha256 = $null
                }
            },
            [ordered]@{
                name = 'wezterm'
                role = 'confirmed'
                version = '20240203-110809-5046fc22'
                source = [ordered]@{
                    origin = 'https://github.com/wezterm/wezterm'
                    package = 'wez.wezterm'
                    release_tag = '20240203-110809-5046fc22'
                    asset = [ordered]@{
                        kind = 'zip'
                        name = (
                            'WezTerm-windows-' +
                            '20240203-110809-5046fc22.zip'
                        )
                        url = (
                            'https://github.com/wezterm/wezterm/releases/' +
                            'download/20240203-110809-5046fc22/' +
                            'WezTerm-windows-' +
                            '20240203-110809-5046fc22.zip'
                        )
                        bytes = 17000000
                        sha256 = ('b' * 64)
                        executable_entry = (
                            'WezTerm-windows-' +
                            '20240203-110809-5046fc22/wezterm-gui.exe'
                        )
                        staged_file_count = 3
                        staged_total_bytes = 22000000
                        staged_tree_sha256 = ('7' * 64)
                    }
                }
                executable = [ordered]@{
                    leaf = 'wezterm-gui.exe'
                    staging_path = (
                        'staging/wezterm/20240203-110809-5046fc22/' +
                        'wezterm-gui.exe'
                    )
                    bytes = 12000000
                    sha256 = ('2' * 64)
                    authenticode_status = 'NotSigned'
                    signer_cert_sha256 = $null
                }
            },
            [ordered]@{
                name = 'rio'
                role = 'confirmed'
                version = '0.4.12'
                source = [ordered]@{
                    origin = 'https://github.com/raphamorim/rio'
                    package = 'raphamorim.rio'
                    release_tag = 'v0.4.12'
                    asset = [ordered]@{
                        kind = 'direct-executable'
                        name = 'rio-portable-x86_64.exe'
                        url = (
                            'https://github.com/raphamorim/rio/releases/' +
                            'download/v0.4.12/rio-portable-x86_64.exe'
                        )
                        bytes = 18000000
                        sha256 = ('c' * 64)
                        executable_entry = 'rio-portable-x86_64.exe'
                        staged_file_count = 1
                        staged_total_bytes = 13000000
                        staged_tree_sha256 = ('8' * 64)
                    }
                }
                executable = [ordered]@{
                    leaf = 'rio.exe'
                    staging_path = 'staging/rio/0.4.12/rio.exe'
                    bytes = 13000000
                    sha256 = ('3' * 64)
                    authenticode_status = 'NotSigned'
                    signer_cert_sha256 = $null
                }
            },
            [ordered]@{
                name = 'tabby'
                role = 'confirmed'
                version = '1.0.235'
                source = [ordered]@{
                    origin = 'https://github.com/Eugeny/tabby'
                    package = 'Eugeny.Tabby'
                    release_tag = 'v1.0.235'
                    asset = [ordered]@{
                        kind = 'zip'
                        name = 'tabby-1.0.235-portable-x64.zip'
                        url = (
                            'https://github.com/Eugeny/tabby/releases/' +
                            'download/v1.0.235/' +
                            'tabby-1.0.235-portable-x64.zip'
                        )
                        bytes = 220000000
                        sha256 = ('d' * 64)
                        executable_entry = 'Tabby.exe'
                        staged_file_count = 10
                        staged_total_bytes = 220000000
                        staged_tree_sha256 = ('9' * 64)
                    }
                }
                executable = [ordered]@{
                    leaf = 'Tabby.exe'
                    staging_path = 'staging/tabby/1.0.235/Tabby.exe'
                    bytes = 210493480
                    sha256 = (
                        '40c0a711c30ee36a168ac435ea726d1a692793bdf' +
                        '43841581a1238954f62eee6'
                    )
                    authenticode_status = 'Valid'
                    signer_cert_sha256 = (
                        'ff9d23bd5e859a29730297c0f7b6a021248c067c' +
                        '951a78e8154cb3a246f0239d'
                    )
                }
            },
            [ordered]@{
                name = 'wt'
                role = 'advisory'
                version = '1.24.11911.0'
                source = [ordered]@{
                    origin = 'https://github.com/microsoft/terminal'
                    package = 'Microsoft.WindowsTerminal'
                    release_tag = 'v1.24.11911.0'
                    asset = [ordered]@{
                        kind = 'zip'
                        name = (
                            'Microsoft.WindowsTerminal_' +
                            '1.24.11911.0_x64.zip'
                        )
                        url = (
                            'https://github.com/microsoft/terminal/releases/' +
                            'download/v1.24.11911.0/' +
                            'Microsoft.WindowsTerminal_' +
                            '1.24.11911.0_x64.zip'
                        )
                        bytes = 19000000
                        sha256 = ('e' * 64)
                        executable_entry = (
                            'terminal-1.24.11911.0/WindowsTerminal.exe'
                        )
                        staged_file_count = 5
                        staged_total_bytes = 20000000
                        staged_tree_sha256 = ('0' * 64)
                    }
                }
                executable = [ordered]@{
                    leaf = 'WindowsTerminal.exe'
                    staging_path = (
                        'staging/wt/1.24.11911.0/WindowsTerminal.exe'
                    )
                    bytes = 15000000
                    sha256 = ('5' * 64)
                    authenticode_status = 'Valid'
                    signer_cert_sha256 = (
                        'd33927e4dda9b91def9f8ed282549a49217ed8cac' +
                        'f54577a690963cbc5eff3ed'
                    )
                }
            }
        )
    }
}

function New-KettlePerfComparatorCampaignTestPath {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Creates only one bounded test scratch directory.'
    )]
    param(
        [Parameter(Mandatory)]
        [Collections.IDictionary]$Fixture
    )

    $directory = Join-Path $script:campaignRoot $Fixture.campaign_id
    [void][IO.Directory]::CreateDirectory($directory)
    return Join-Path $directory 'campaign.json'
}

function Write-KettlePerfComparatorCampaignFixture {
    param(
        [Parameter(Mandatory)]
        [Collections.IDictionary]$Fixture
    )

    $path = New-KettlePerfComparatorCampaignTestPath $Fixture
    Write-KettlePerfComparatorCampaignTestUtf8 `
        -Path $path -Text ($Fixture | ConvertTo-Json -Depth 12)
    return $path
}

function Invoke-KettlePerfComparatorCampaignExpectedFailure {
    param(
        [Parameter(Mandatory)]
        [string]$Description,
        [Parameter(Mandatory)]
        [scriptblock]$Action
    )

    try {
        & $Action
    } catch {
        return
    }
    throw "Comparator campaign accepted invalid case: $Description"
}

function Invoke-KettlePerfComparatorCampaignObjectFailure {
    param(
        [Parameter(Mandatory)]
        [string]$Description,
        [Parameter(Mandatory)]
        [scriptblock]$Mutation
    )

    $script:fixtureSequence++
    $nonce = $script:fixtureSequence.ToString('x16')
    $fixture = New-KettlePerfComparatorCampaignFixture $nonce
    & $Mutation $fixture
    $path = Write-KettlePerfComparatorCampaignFixture $fixture
    Invoke-KettlePerfComparatorCampaignExpectedFailure `
        -Description $Description -Action {
            [void](Read-KettlePerfComparatorCampaign `
                -Path $path -ExpectedCampaignRoot $script:campaignRoot)
        }
}

function New-KettlePerfComparatorTerminalRecord {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'Returns only an in-memory terminal record.'
    )]
    param(
        [Parameter(Mandatory)]
        $Entry
    )

    $executable = Join-Path 'C:\arbitrary-private-install' `
        $Entry.executable.leaf
    $record = [ordered]@{
        name = [string]$Entry.name
        version = [string]$Entry.version
        launcher = $executable
        executable = $executable
        executable_bytes = [long]$Entry.executable.bytes
        executable_sha256 = [string]$Entry.executable.sha256
        authenticode_status = [string]$Entry.executable.authenticode_status
        signer_cert_sha256 = $Entry.executable.signer_cert_sha256
        comparator_role = [string]$Entry.role
        source = New-KettlePerfComparatorTerminalSource -Entry $Entry
    }
    if ($Entry.name -ceq 'wt') {
        $record['launch_mode'] = 'installed-appx-direct-host'
    }
    return $record
}

function Copy-KettlePerfComparatorTerminalRecord {
    param(
        [Parameter(Mandatory)]
        $Record
    )

    return (
        $Record | ConvertTo-Json -Depth 8 | ConvertFrom-Json -ErrorAction Stop
    )
}

$scratch = Join-Path ([IO.Path]::GetTempPath()) (
    'kettle-comparator-campaign-' + [Guid]::NewGuid().ToString('N')
)
$script:campaignRoot = Join-Path $scratch 'campaigns'
$script:stagingRoot = Join-Path $scratch 'local-staging'
$outsideRoot = Join-Path $scratch 'outside'
$script:fixtureSequence = 1
[void][IO.Directory]::CreateDirectory($script:campaignRoot)
[void][IO.Directory]::CreateDirectory($script:stagingRoot)
[void][IO.Directory]::CreateDirectory($outsideRoot)

try {
    $positiveFixture = New-KettlePerfComparatorCampaignFixture `
        '0000000000000001'
    $runtimeExecutable = (Get-Process -Id $PID).Path
    $runtimeItem = Get-Item -LiteralPath $runtimeExecutable -Force
    $runtimeSha256 = (
        Get-FileHash -LiteralPath $runtimeExecutable -Algorithm SHA256
    ).Hash.ToLowerInvariant()
    $runtimeSignature = Get-AuthenticodeSignature `
        -LiteralPath $runtimeExecutable
    $runtimeSignerSha256 = if (
        $null -eq $runtimeSignature.SignerCertificate
    ) {
        $null
    } else {
        Get-KettlePerfComparatorCertificateSha256 `
            -Certificate $runtimeSignature.SignerCertificate
    }
    if (
        [string]$runtimeSignature.Status -notin @('Valid', 'NotSigned')
    ) {
        throw 'Self-test runtime has no supported Authenticode status'
    }
    $positiveFixture.terminals[0].executable.bytes = [long]$runtimeItem.Length
    $positiveFixture.terminals[0].executable.sha256 = $runtimeSha256
    $positiveFixture.terminals[0].executable.authenticode_status = (
        [string]$runtimeSignature.Status
    )
    $positiveFixture.terminals[0].executable.signer_cert_sha256 = (
        $runtimeSignerSha256
    )
    $positiveFixture.terminals[0].source.asset.staged_file_count = 1
    $positiveFixture.terminals[0].source.asset.staged_total_bytes = (
        [long]$runtimeItem.Length
    )
    $positiveFixture.terminals[0].source.asset.staged_tree_sha256 = (
        Get-KettlePerfComparatorStagedTreeSha256 -Files @(
            [pscustomobject]@{
                relative_path = 'alacritty.exe'
                bytes = [long]$runtimeItem.Length
                sha256 = $runtimeSha256
            }
        )
    )
    $positiveStagedPath = Join-Path (
        Join-Path $script:stagingRoot $positiveFixture.campaign_id
    ) (
        $positiveFixture.terminals[0].executable.staging_path.Replace(
            '/',
            [IO.Path]::DirectorySeparatorChar
        )
    )
    [void][IO.Directory]::CreateDirectory(
        [IO.Path]::GetDirectoryName($positiveStagedPath)
    )
    [IO.File]::Copy($runtimeExecutable, $positiveStagedPath, $false)
    $positivePath = Write-KettlePerfComparatorCampaignFixture $positiveFixture
    $campaign = Read-KettlePerfComparatorCampaign `
        -Path $positivePath -ExpectedCampaignRoot $script:campaignRoot
    Assert-KettlePerfComparatorCampaignTest (
        $campaign.schema -ceq 'kettle-windows-comparator-campaign-v1' -and
        $campaign.campaign_id -ceq $positiveFixture.campaign_id -and
        $campaign.platform.os -ceq 'windows' -and
        $campaign.platform.architecture -ceq 'x86_64' -and
        $campaign.selection.policy -ceq
            'official-stable-pinned-assets-v1' -and
        $campaign.terminals.Count -eq 5 -and
        $campaign.terminals[0].name -ceq 'alacritty' -and
        $campaign.terminals[0].source.asset.kind -ceq
            'direct-executable' -and
        $campaign.terminals[0].source.release_tag -ceq 'v0.17.0' -and
        $campaign.terminals[3].role -ceq 'confirmed' -and
        $campaign.terminals[4].role -ceq 'advisory' -and
        $campaign.campaign_file.relative_path -ceq (
            "$($positiveFixture.campaign_id)/campaign.json"
        ) -and
        [long]$campaign.campaign_file.bytes -eq (
            Get-Item -LiteralPath $positivePath
        ).Length -and
        $campaign.campaign_file.sha256 -cmatch '^[0-9a-f]{64}$'
    ) 'positive campaign did not return its normalized contract and identity'
    $expectedCampaignHash = (
        Get-FileHash -LiteralPath $positivePath -Algorithm SHA256
    ).Hash.ToLowerInvariant()
    Assert-KettlePerfComparatorCampaignTest (
        $campaign.campaign_file.sha256 -ceq $expectedCampaignHash -and
        [StringComparer]::OrdinalIgnoreCase.Equals(
            $campaign.campaign_file.path,
            $positivePath
        )
    ) 'campaign file path or byte hash was not preserved exactly'
    $campaignEvidence = Get-KettlePerfComparatorCampaignEvidence `
        -Campaign $campaign
    Assert-KettlePerfComparatorCampaignTest (
        Test-KettlePerfComparatorCampaignEvidence `
            -Campaign $campaign -Evidence $campaignEvidence
    ) 'positive campaign evidence projection was rejected'
    $tamperedCampaignEvidence = $campaignEvidence |
        ConvertTo-Json -Depth 10 |
        ConvertFrom-Json -ErrorAction Stop
    $tamperedCampaignEvidence.terminals[0].source.asset.staged_file_count++
    Assert-KettlePerfComparatorCampaignTest (
        -not (Test-KettlePerfComparatorCampaignEvidence `
            -Campaign $campaign -Evidence $tamperedCampaignEvidence)
    ) 'tampered campaign evidence projection was accepted'

    foreach ($name in @('alacritty', 'wezterm', 'rio', 'tabby', 'wt')) {
        $entry = Get-KettlePerfComparatorCampaignEntry `
            -Campaign $campaign -Name $name
        $record = New-KettlePerfComparatorTerminalRecord $entry
        Assert-KettlePerfComparatorCampaignTest (
            Test-KettlePerfComparatorCampaignTerminalIdentity `
                -Entry $entry -TerminalRecord $record
        ) "positive terminal identity did not match for $name"
        $record.source.runtime_kind = 'tampered'
        Assert-KettlePerfComparatorCampaignTest (
            -not (Test-KettlePerfComparatorCampaignTerminalIdentity `
                -Entry $entry -TerminalRecord $record)
        ) "tampered runtime kind was accepted for $name"
    }

    $firstEntry = Get-KettlePerfComparatorCampaignEntry `
        -Campaign $campaign -Name alacritty
    $firstEntry.source.origin = 'mutated'
    $secondEntry = Get-KettlePerfComparatorCampaignEntry `
        -Campaign $campaign -Name alacritty
    Assert-KettlePerfComparatorCampaignTest (
        $secondEntry.source.origin -ceq
            'https://github.com/alacritty/alacritty'
    ) 'campaign entry lookup shared mutable nested state'
    $campaign.terminals[0].source.origin = 'caller-mutation'
    $freshCampaign = Read-KettlePerfComparatorCampaign `
        -Path $positivePath -ExpectedCampaignRoot $script:campaignRoot
    Assert-KettlePerfComparatorCampaignTest (
        $freshCampaign.terminals[0].source.origin -ceq
            'https://github.com/alacritty/alacritty'
    ) 'campaign reader reused caller-mutated normalized state'
    $campaign = $freshCampaign
    $runtimeEntry = Get-KettlePerfComparatorCampaignEntry `
        -Campaign $campaign -Name alacritty
    $resolvedStagedPath = Resolve-KettlePerfComparatorCampaignExecutable `
        -Campaign $campaign -Entry $runtimeEntry `
        -CampaignRoot $script:campaignRoot -StagingRoot $script:stagingRoot
    Assert-KettlePerfComparatorCampaignTest (
        [StringComparer]::OrdinalIgnoreCase.Equals(
            $resolvedStagedPath,
            $positiveStagedPath
        )
    ) 'runtime verifier did not return the canonical staged executable'
    $runtimeLease = Open-KettlePerfComparatorCampaignExecutableLease `
        -Campaign $campaign -Entry $runtimeEntry `
        -CampaignRoot $script:campaignRoot -StagingRoot $script:stagingRoot
    try {
        Assert-KettlePerfComparatorCampaignTest (
            $runtimeLease.schema -ceq
                'kettle-comparator-executable-lease-v1' -and
            $runtimeLease.closed -eq $false -and
            $runtimeLease.stream.Position -eq 0 -and
            $runtimeLease.sha256 -ceq $runtimeEntry.executable.sha256 -and
            $runtimeLease.files.Count -eq
                $runtimeEntry.source.asset.staged_file_count -and
            $runtimeLease.staged_tree_sha256 -ceq
                $runtimeEntry.source.asset.staged_tree_sha256
        ) 'runtime executable lease did not retain canonical identity'
        Invoke-KettlePerfComparatorCampaignExpectedFailure `
            -Description 'write while comparator executable is leased' `
            -Action {
                $writer = [IO.File]::Open(
                    $positiveStagedPath,
                    [IO.FileMode]::Open,
                    [IO.FileAccess]::Write,
                    [IO.FileShare]::None
                )
                $writer.Dispose()
            }
        $movedStagedPath = "$positiveStagedPath.moved"
        try {
            Invoke-KettlePerfComparatorCampaignExpectedFailure `
                -Description (
                    'replacement while comparator executable is leased'
                ) -Action {
                    [IO.File]::Move(
                        $positiveStagedPath,
                        $movedStagedPath
                    )
                }
        } finally {
            if ([IO.File]::Exists($movedStagedPath)) {
                [IO.File]::Move(
                    $movedStagedPath,
                    $positiveStagedPath
                )
            }
        }
    } finally {
        Close-KettlePerfComparatorCampaignExecutableLease $runtimeLease
    }
    Assert-KettlePerfComparatorCampaignTest (
        $runtimeLease.closed -eq $true -and
        $null -eq $runtimeLease.stream -and
        $null -eq $runtimeLease.files[0].stream
    ) 'runtime executable lease did not close idempotently'
    Close-KettlePerfComparatorCampaignExecutableLease $runtimeLease

    $treeTestRoot = Join-Path $scratch 'tree-lease'
    $treeTestResources = Join-Path $treeTestRoot 'resources'
    [void][IO.Directory]::CreateDirectory($treeTestResources)
    $treeTestMain = Join-Path $treeTestRoot 'main.exe'
    $treeTestSupport = Join-Path $treeTestResources 'support.dat'
    [IO.File]::Copy($runtimeExecutable, $treeTestMain, $false)
    Write-KettlePerfComparatorCampaignTestUtf8 `
        -Path $treeTestSupport -Text 'support payload'
    $treeTestMainItem = Get-Item -LiteralPath $treeTestMain
    $treeTestSupportItem = Get-Item -LiteralPath $treeTestSupport
    $treeTestFiles = [object[]]@(
        [pscustomobject]@{
            relative_path = 'main.exe'
            bytes = [long]$treeTestMainItem.Length
            sha256 = (
                Get-FileHash -LiteralPath $treeTestMain -Algorithm SHA256
            ).Hash.ToLowerInvariant()
        },
        [pscustomobject]@{
            relative_path = 'resources/support.dat'
            bytes = [long]$treeTestSupportItem.Length
            sha256 = (
                Get-FileHash -LiteralPath $treeTestSupport -Algorithm SHA256
            ).Hash.ToLowerInvariant()
        }
    )
    [long]$treeTestTotal = (
        $treeTestMainItem.Length + $treeTestSupportItem.Length
    )
    $treeTestSha = Get-KettlePerfComparatorStagedTreeSha256 `
        -Files $treeTestFiles
    $treeTestLease = Open-KettlePerfComparatorStagedTreeLease `
        -Root $treeTestRoot -ExpectedFileCount 2 `
        -ExpectedTotalBytes $treeTestTotal `
        -ExpectedTreeSha256 $treeTestSha
    try {
        Assert-KettlePerfComparatorCampaignTest (
            $treeTestLease.file_count -eq 2 -and
            $treeTestLease.files[0].relative_path -ceq 'main.exe' -and
            $treeTestLease.files[1].relative_path -ceq
                'resources/support.dat'
        ) 'staged-tree lease did not retain its complete ordered tree'
        Invoke-KettlePerfComparatorCampaignExpectedFailure `
            -Description 'adjacent file write during staged-tree lease' `
            -Action {
                $writer = [IO.File]::Open(
                    $treeTestSupport,
                    [IO.FileMode]::Open,
                    [IO.FileAccess]::Write,
                    [IO.FileShare]::None
                )
                $writer.Dispose()
            }
    } finally {
        Close-KettlePerfComparatorStagedTreeLease $treeTestLease
    }
    Assert-KettlePerfComparatorCampaignTest (
        $treeTestLease.closed -eq $true -and
        $null -eq $treeTestLease.files[0].stream -and
        $null -eq $treeTestLease.files[1].stream
    ) 'staged-tree close did not release every retained file'

    $treeTestExtra = Join-Path $treeTestRoot 'extra.dat'
    Write-KettlePerfComparatorCampaignTestUtf8 `
        -Path $treeTestExtra -Text 'extra'
    Invoke-KettlePerfComparatorCampaignExpectedFailure `
        -Description 'extra staged-tree file' -Action {
            [void](Open-KettlePerfComparatorStagedTreeLease `
                -Root $treeTestRoot -ExpectedFileCount 2 `
                -ExpectedTotalBytes $treeTestTotal `
                -ExpectedTreeSha256 $treeTestSha)
        }
    [IO.File]::Delete($treeTestExtra)
    $treeTestEmptyDirectory = Join-Path $treeTestRoot 'empty-extra'
    [void][IO.Directory]::CreateDirectory($treeTestEmptyDirectory)
    Invoke-KettlePerfComparatorCampaignExpectedFailure `
        -Description 'extra empty staged-tree directory' -Action {
            [void](Open-KettlePerfComparatorStagedTreeLease `
                -Root $treeTestRoot -ExpectedFileCount 2 `
                -ExpectedTotalBytes $treeTestTotal `
                -ExpectedTreeSha256 $treeTestSha)
        }
    [IO.Directory]::Delete($treeTestEmptyDirectory)
    [IO.File]::Delete($treeTestSupport)
    Invoke-KettlePerfComparatorCampaignExpectedFailure `
        -Description 'missing staged-tree file' -Action {
            [void](Open-KettlePerfComparatorStagedTreeLease `
                -Root $treeTestRoot -ExpectedFileCount 2 `
                -ExpectedTotalBytes $treeTestTotal `
                -ExpectedTreeSha256 $treeTestSha)
        }
    [void](New-Item -ItemType HardLink -Path $treeTestSupport `
        -Target $treeTestMain -ErrorAction Stop)
    try {
        Invoke-KettlePerfComparatorCampaignExpectedFailure `
            -Description 'hard-linked staged-tree file' -Action {
                [void](Open-KettlePerfComparatorStagedTreeLease `
                    -Root $treeTestRoot -ExpectedFileCount 2 `
                    -ExpectedTotalBytes $treeTestTotal `
                    -ExpectedTreeSha256 $treeTestSha)
            }
    } finally {
        [IO.File]::Delete($treeTestSupport)
    }
    Write-KettlePerfComparatorCampaignTestUtf8 `
        -Path $treeTestSupport -Text 'tampered support payload'
    Invoke-KettlePerfComparatorCampaignExpectedFailure `
        -Description 'staged-tree aggregate mismatch' -Action {
            [void](Open-KettlePerfComparatorStagedTreeLease `
                -Root $treeTestRoot -ExpectedFileCount 2 `
                -ExpectedTotalBytes $treeTestTotal `
                -ExpectedTreeSha256 $treeTestSha)
        }

    $tamperedBytes = [IO.File]::ReadAllBytes($positiveStagedPath)
    $tamperedBytes[0] = $tamperedBytes[0] -bxor 0xff
    [IO.File]::WriteAllBytes($positiveStagedPath, $tamperedBytes)
    try {
        Invoke-KettlePerfComparatorCampaignExpectedFailure `
            -Description 'tampered staged executable' -Action {
                [void](Resolve-KettlePerfComparatorCampaignExecutable `
                    -Campaign $campaign -Entry $runtimeEntry `
                    -CampaignRoot $script:campaignRoot `
                    -StagingRoot $script:stagingRoot)
            }
    } finally {
        [Array]::Clear($tamperedBytes, 0, $tamperedBytes.Length)
        [IO.File]::Copy($runtimeExecutable, $positiveStagedPath, $true)
    }
    $mutatedRuntimeEntry = Get-KettlePerfComparatorCampaignEntry `
        -Campaign $campaign -Name alacritty
    $mutatedRuntimeEntry.executable.bytes++
    Invoke-KettlePerfComparatorCampaignExpectedFailure `
        -Description 'caller-mutated runtime entry' -Action {
            [void](Resolve-KettlePerfComparatorCampaignExecutable `
                -Campaign $campaign -Entry $mutatedRuntimeEntry `
                -CampaignRoot $script:campaignRoot `
                -StagingRoot $script:stagingRoot)
        }
    $stagedTerminalDirectory = Split-Path -Parent (
        Split-Path -Parent $positiveStagedPath
    )
    $stagedTerminalReal = "$stagedTerminalDirectory-real"
    [IO.Directory]::Move($stagedTerminalDirectory, $stagedTerminalReal)
    try {
        [void](New-Item -ItemType Junction -Path $stagedTerminalDirectory `
            -Target $stagedTerminalReal -ErrorAction Stop)
        Invoke-KettlePerfComparatorCampaignExpectedFailure `
            -Description 'reparse point in staged executable subtree' -Action {
                [void](Resolve-KettlePerfComparatorCampaignExecutable `
                    -Campaign $campaign -Entry $runtimeEntry `
                    -CampaignRoot $script:campaignRoot `
                    -StagingRoot $script:stagingRoot)
            }
    } finally {
        if ([IO.Directory]::Exists($stagedTerminalDirectory)) {
            [IO.Directory]::Delete($stagedTerminalDirectory)
        }
        if ([IO.Directory]::Exists($stagedTerminalReal)) {
            [IO.Directory]::Move(
                $stagedTerminalReal,
                $stagedTerminalDirectory
            )
        }
    }
    $stagedExecutableBackup = "$positiveStagedPath.backup"
    [IO.File]::Move($positiveStagedPath, $stagedExecutableBackup)
    try {
        [void][IO.Directory]::CreateDirectory($positiveStagedPath)
        Invoke-KettlePerfComparatorCampaignExpectedFailure `
            -Description 'directory in place of staged executable' -Action {
                [void](Resolve-KettlePerfComparatorCampaignExecutable `
                    -Campaign $campaign -Entry $runtimeEntry `
                    -CampaignRoot $script:campaignRoot `
                    -StagingRoot $script:stagingRoot)
            }
    } finally {
        if ([IO.Directory]::Exists($positiveStagedPath)) {
            [IO.Directory]::Delete($positiveStagedPath)
        }
        if ([IO.File]::Exists($stagedExecutableBackup)) {
            [IO.File]::Move(
                $stagedExecutableBackup,
                $positiveStagedPath
            )
        }
    }

    # File/path and strict parser boundaries.
    Invoke-KettlePerfComparatorCampaignExpectedFailure `
        -Description 'manifest outside the append-only campaign root' -Action {
            $fixture = New-KettlePerfComparatorCampaignFixture `
                '0000000000000100'
            $directory = Join-Path $outsideRoot $fixture.campaign_id
            [void][IO.Directory]::CreateDirectory($directory)
            $path = Join-Path $directory 'campaign.json'
            Write-KettlePerfComparatorCampaignTestUtf8 `
                $path ($fixture | ConvertTo-Json -Depth 12)
            [void](Read-KettlePerfComparatorCampaign `
                -Path $path -ExpectedCampaignRoot $script:campaignRoot)
        }
    Invoke-KettlePerfComparatorCampaignExpectedFailure `
        -Description 'nested campaign directory' -Action {
            $fixture = New-KettlePerfComparatorCampaignFixture `
                '0000000000000101'
            $directory = Join-Path $script:campaignRoot `
                "nested\$($fixture.campaign_id)"
            [void][IO.Directory]::CreateDirectory($directory)
            $path = Join-Path $directory 'campaign.json'
            Write-KettlePerfComparatorCampaignTestUtf8 `
                $path ($fixture | ConvertTo-Json -Depth 12)
            [void](Read-KettlePerfComparatorCampaign `
                -Path $path -ExpectedCampaignRoot $script:campaignRoot)
        }
    Invoke-KettlePerfComparatorCampaignExpectedFailure `
        -Description 'wrong manifest leaf' -Action {
            $fixture = New-KettlePerfComparatorCampaignFixture `
                '0000000000000102'
            $directory = Join-Path $script:campaignRoot $fixture.campaign_id
            [void][IO.Directory]::CreateDirectory($directory)
            $path = Join-Path $directory 'other.json'
            Write-KettlePerfComparatorCampaignTestUtf8 `
                $path ($fixture | ConvertTo-Json -Depth 12)
            [void](Read-KettlePerfComparatorCampaign `
                -Path $path -ExpectedCampaignRoot $script:campaignRoot)
        }
    Invoke-KettlePerfComparatorCampaignExpectedFailure `
        -Description 'campaign id differs from directory' -Action {
            $fixture = New-KettlePerfComparatorCampaignFixture `
                '0000000000000103'
            $directory = Join-Path $script:campaignRoot (
                'windows-x86_64-20260726T224234Z-0000000000000104'
            )
            [void][IO.Directory]::CreateDirectory($directory)
            $path = Join-Path $directory 'campaign.json'
            Write-KettlePerfComparatorCampaignTestUtf8 `
                $path ($fixture | ConvertTo-Json -Depth 12)
            [void](Read-KettlePerfComparatorCampaign `
                -Path $path -ExpectedCampaignRoot $script:campaignRoot)
        }
    $adsPath = "$positivePath`:alternate"
    Invoke-KettlePerfComparatorCampaignExpectedFailure `
        -Description 'manifest alternate data stream' -Action {
            [void](Read-KettlePerfComparatorCampaign `
                -Path $adsPath -ExpectedCampaignRoot $script:campaignRoot)
        }

    $rawCases = @(
        @{
            description = 'UTF-8 BOM'
            bytes = [byte[]](0xef, 0xbb, 0xbf) + (
                [Text.Encoding]::UTF8.GetBytes('{}')
            )
        },
        @{
            description = 'invalid UTF-8'
            bytes = [byte[]](0x7b, 0x22, 0xff, 0x22, 0x3a, 0x31, 0x7d)
        }
    )
    foreach ($rawCase in $rawCases) {
        $script:fixtureSequence++
        $fixture = New-KettlePerfComparatorCampaignFixture `
            $script:fixtureSequence.ToString('x16')
        $path = New-KettlePerfComparatorCampaignTestPath $fixture
        [IO.File]::WriteAllBytes($path, $rawCase.bytes)
        Invoke-KettlePerfComparatorCampaignExpectedFailure `
            -Description $rawCase.description -Action {
                [void](Read-KettlePerfComparatorCampaign `
                    -Path $path -ExpectedCampaignRoot $script:campaignRoot)
            }
    }

    $script:fixtureSequence++
    $fixture = New-KettlePerfComparatorCampaignFixture `
        $script:fixtureSequence.ToString('x16')
    $path = New-KettlePerfComparatorCampaignTestPath $fixture
    $duplicateJson = (
        '{"schema":"kettle-windows-comparator-campaign-v1",' +
        '"schema":"kettle-windows-comparator-campaign-v1"}'
    )
    Write-KettlePerfComparatorCampaignTestUtf8 $path $duplicateJson
    Invoke-KettlePerfComparatorCampaignExpectedFailure `
        -Description 'duplicate JSON property' -Action {
            [void](Read-KettlePerfComparatorCampaign `
                -Path $path -ExpectedCampaignRoot $script:campaignRoot)
        }

    $script:fixtureSequence++
    $fixture = New-KettlePerfComparatorCampaignFixture `
        $script:fixtureSequence.ToString('x16')
    $path = New-KettlePerfComparatorCampaignTestPath $fixture
    $caseDuplicateJson = (
        '{"schema":"kettle-windows-comparator-campaign-v1",' +
        '"Schema":"kettle-windows-comparator-campaign-v1"}'
    )
    Write-KettlePerfComparatorCampaignTestUtf8 $path $caseDuplicateJson
    Invoke-KettlePerfComparatorCampaignExpectedFailure `
        -Description 'case-ambiguous JSON property' -Action {
            [void](Read-KettlePerfComparatorCampaign `
                -Path $path -ExpectedCampaignRoot $script:campaignRoot)
        }

    $script:fixtureSequence++
    $fixture = New-KettlePerfComparatorCampaignFixture `
        $script:fixtureSequence.ToString('x16')
    $path = Write-KettlePerfComparatorCampaignFixture $fixture
    $escapedTimestampJson = [IO.File]::ReadAllText($path).Replace(
        '2026-07-26T22:42:34Z',
        '2026-07-26T22:42:\u0033\u0034Z'
    )
    Write-KettlePerfComparatorCampaignTestUtf8 $path $escapedTimestampJson
    Invoke-KettlePerfComparatorCampaignExpectedFailure `
        -Description 'escaped RFC3339 timestamp token' -Action {
            [void](Read-KettlePerfComparatorCampaign `
                -Path $path -ExpectedCampaignRoot $script:campaignRoot)
        }

    $script:fixtureSequence++
    $fixture = New-KettlePerfComparatorCampaignFixture `
        $script:fixtureSequence.ToString('x16')
    $path = New-KettlePerfComparatorCampaignTestPath $fixture
    Write-KettlePerfComparatorCampaignTestUtf8 $path '{"schema":'
    Invoke-KettlePerfComparatorCampaignExpectedFailure `
        -Description 'truncated JSON' -Action {
            [void](Read-KettlePerfComparatorCampaign `
                -Path $path -ExpectedCampaignRoot $script:campaignRoot)
        }

    $script:fixtureSequence++
    $fixture = New-KettlePerfComparatorCampaignFixture `
        $script:fixtureSequence.ToString('x16')
    $path = New-KettlePerfComparatorCampaignTestPath $fixture
    Write-KettlePerfComparatorCampaignTestUtf8 $path (
        '{"nested":' + ('[' * 10) + '0' + (']' * 10) + '}'
    )
    Invoke-KettlePerfComparatorCampaignExpectedFailure `
        -Description 'excess JSON depth' -Action {
            [void](Read-KettlePerfComparatorCampaign `
                -Path $path -ExpectedCampaignRoot $script:campaignRoot)
        }

    $script:fixtureSequence++
    $fixture = New-KettlePerfComparatorCampaignFixture `
        $script:fixtureSequence.ToString('x16')
    $path = New-KettlePerfComparatorCampaignTestPath $fixture
    [IO.File]::WriteAllBytes($path, [byte[]](0x20) * 65537)
    Invoke-KettlePerfComparatorCampaignExpectedFailure `
        -Description 'oversized campaign manifest' -Action {
            [void](Read-KettlePerfComparatorCampaign `
                -Path $path -ExpectedCampaignRoot $script:campaignRoot)
        }

    # Exact schema shapes and scalar types.
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'unknown top-level property' {
            param($f)
            $f.unknown = $true
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'missing top-level property' {
            param($f)
            [void]$f.Remove('platform')
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'case-only top-level property' {
            param($f)
            $value = $f.schema
            [void]$f.Remove('schema')
            $f.Schema = $value
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure 'null schema' {
        param($f)
        $f.schema = $null
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure 'wrong schema' {
        param($f)
        $f.schema = 'kettle-windows-comparator-campaign-v2'
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure 'null campaign id' {
        param($f)
        $f.campaign_id = $null
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure 'zero campaign nonce' {
        param($f)
        $f.campaign_id = (
            'windows-x86_64-20260726T224234Z-0000000000000000'
        )
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'campaign id timestamp mismatch' {
            param($f)
            $f.campaign_id = (
                'windows-x86_64-20260726T224235Z-' +
                ($f.campaign_id -split '-')[-1]
            )
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'unknown platform property' {
            param($f)
            $f.platform.extra = 1
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure 'wrong operating system' {
        param($f)
        $f.platform.os = 'linux'
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure 'wrong architecture' {
        param($f)
        $f.platform.architecture = 'aarch64'
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure 'null platform' {
        param($f)
        $f.platform = $null
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'unknown selection property' {
            param($f)
            $f.selection.extra = 1
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'mutable selection policy' {
            param($f)
            $f.selection.policy = 'latest'
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'non-UTC selection timestamp' {
            param($f)
            $f.selection.started_at_utc = '2026-07-26T15:42:34-07:00'
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'fractional selection timestamp' {
            param($f)
            $f.selection.started_at_utc = '2026-07-26T22:42:34.000Z'
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'invalid selection calendar date' {
            param($f)
            $f.selection.started_at_utc = '2026-02-30T22:42:34Z'
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'completion before start' {
            param($f)
            $f.selection.completed_at_utc = '2026-07-26T22:42:33Z'
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'null selection timestamp' {
            param($f)
            $f.selection.completed_at_utc = $null
        }

    # Exact five-peer order, roles, stable versions, and official sources.
    Invoke-KettlePerfComparatorCampaignObjectFailure 'terminal count' {
        param($f)
        $f.terminals = [object[]]@($f.terminals[0..3])
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure 'terminal reorder' {
        param($f)
        $swap = $f.terminals[0]
        $f.terminals[0] = $f.terminals[1]
        $f.terminals[1] = $swap
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure 'duplicate terminal' {
        param($f)
        $f.terminals[1] = $f.terminals[0]
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure 'null terminal' {
        param($f)
        $f.terminals[2] = $null
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'unknown terminal property' {
            param($f)
            $f.terminals[0].extra = 'forbidden'
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure 'wrong confirmed role' {
        param($f)
        $f.terminals[0].role = 'advisory'
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure 'wrong advisory role' {
        param($f)
        $f.terminals[4].role = 'confirmed'
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure 'null role' {
        param($f)
        $f.terminals[0].role = $null
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'prerelease semantic version' {
            param($f)
            $f.terminals[0].version = '0.18.0-rc1'
            $f.terminals[0].source.release_tag = 'v0.18.0-rc1'
            $f.terminals[0].executable.staging_path = (
                'staging/alacritty/0.18.0-rc1/alacritty.exe'
            )
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'noncanonical semantic version' {
            param($f)
            $f.terminals[2].version = '00.4.12'
            $f.terminals[2].source.release_tag = 'v00.4.12'
            $f.terminals[2].executable.staging_path = (
                'staging/rio/00.4.12/rio.exe'
            )
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'mutable WezTerm tag' {
            param($f)
            $f.terminals[1].version = 'nightly'
            $f.terminals[1].source.release_tag = 'nightly'
            $f.terminals[1].executable.staging_path = (
                'staging/wezterm/nightly/wezterm-gui.exe'
            )
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'invalid WezTerm release date' {
            param($f)
            $f.terminals[1].version = '20240231-110809-5046fc22'
            $f.terminals[1].source.release_tag = (
                '20240231-110809-5046fc22'
            )
            $f.terminals[1].executable.staging_path = (
                'staging/wezterm/20240231-110809-5046fc22/' +
                'wezterm-gui.exe'
            )
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'Windows Terminal preview version' {
            param($f)
            $f.terminals[4].version = '1.25.0-preview'
            $f.terminals[4].source.release_tag = 'v1.25.0-preview'
            $f.terminals[4].executable.staging_path = (
                'staging/wt/1.25.0-preview/WindowsTerminal.exe'
            )
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'unknown source property' {
            param($f)
            $f.terminals[0].source.extra = 'forbidden'
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure 'unofficial origin' {
        param($f)
        $f.terminals[0].source.origin = 'https://example.invalid/alacritty'
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure 'wrong package id' {
        param($f)
        $f.terminals[2].source.package = 'Untrusted.Rio'
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure 'mutable release tag' {
        param($f)
        $f.terminals[3].source.release_tag = 'latest'
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure 'null source field' {
        param($f)
        $f.terminals[3].source.package = $null
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure 'null asset' {
        param($f)
        $f.terminals[0].source.asset = $null
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'unknown asset property' {
            param($f)
            $f.terminals[0].source.asset.extra = 'forbidden'
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure 'wrong asset kind' {
        param($f)
        $f.terminals[0].source.asset.kind = 'zip'
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure 'wrong asset name' {
        param($f)
        $f.terminals[0].source.asset.name = 'Alacritty-latest.exe'
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure 'wrong asset origin URL' {
        param($f)
        $f.terminals[0].source.asset.url = (
            'https://example.invalid/Alacritty-v0.17.0-portable.exe'
        )
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure 'wrong asset tag URL' {
        param($f)
        $f.terminals[0].source.asset.url = (
            'https://github.com/alacritty/alacritty/releases/download/' +
            'latest/Alacritty-v0.17.0-portable.exe'
        )
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure 'asset URL query' {
        param($f)
        $f.terminals[0].source.asset.url += '?mutable=1'
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'zero asset size' {
            param($f)
            $f.terminals[0].source.asset.bytes = 0
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'fractional asset size' {
            param($f)
            $f.terminals[0].source.asset.bytes = 1.5
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'string asset size' {
            param($f)
            $f.terminals[0].source.asset.bytes = '16000000'
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'oversized asset size' {
            param($f)
            $f.terminals[0].source.asset.bytes = [long]4294967297
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'uppercase asset SHA-256' {
            param($f)
            $f.terminals[0].source.asset.sha256 = ('A' * 64)
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'short asset SHA-256' {
            param($f)
            $f.terminals[0].source.asset.sha256 = ('a' * 63)
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'asset entry traversal' {
            param($f)
            $f.terminals[1].source.asset.executable_entry = (
                '../wezterm-gui.exe'
            )
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'asset entry alternate stream' {
            param($f)
            $f.terminals[2].source.asset.executable_entry = (
                'rio-portable-x86_64.exe:payload'
            )
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'zero staged file count' {
            param($f)
            $f.terminals[0].source.asset.staged_file_count = 0
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'fractional staged file count' {
            param($f)
            $f.terminals[0].source.asset.staged_file_count = 1.5
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'oversized staged file count' {
            param($f)
            $f.terminals[0].source.asset.staged_file_count = 4097
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'staged total smaller than executable' {
            param($f)
            $f.terminals[0].source.asset.staged_total_bytes = 1
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'string staged total' {
            param($f)
            $f.terminals[0].source.asset.staged_total_bytes = '11000000'
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'uppercase staged-tree SHA-256' {
            param($f)
            $f.terminals[0].source.asset.staged_tree_sha256 = ('A' * 64)
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'short staged-tree SHA-256' {
            param($f)
            $f.terminals[0].source.asset.staged_tree_sha256 = ('a' * 63)
        }

    # Executable leaf, staging path, size, and digest boundaries.
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'unknown executable property' {
            param($f)
            $f.terminals[0].executable.extra = 1
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure 'wrong executable leaf' {
        param($f)
        $f.terminals[0].executable.leaf = 'cmd.exe'
        $f.terminals[0].executable.staging_path = (
            'staging/alacritty/0.17.0/cmd.exe'
        )
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'absolute staging path' {
            param($f)
            $f.terminals[0].executable.staging_path = (
                'C:/staging/alacritty/0.17.0/alacritty.exe'
            )
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'staging traversal' {
            param($f)
            $f.terminals[0].executable.staging_path = (
                'staging/alacritty/../alacritty.exe'
            )
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'staging alternate data stream' {
            param($f)
            $f.terminals[0].executable.staging_path = (
                'staging/alacritty/0.17.0/alacritty.exe:payload'
            )
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'noncanonical staging separators' {
            param($f)
            $f.terminals[0].executable.staging_path = (
                'staging\alacritty\0.17.0\alacritty.exe'
            )
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'wrong terminal staging directory' {
            param($f)
            $f.terminals[0].executable.staging_path = (
                'staging/rio/0.17.0/alacritty.exe'
            )
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'reserved staging component' {
            param($f)
            $f.terminals[0].executable.staging_path = (
                'staging/CON/0.17.0/alacritty.exe'
            )
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'duplicate case-insensitive staging path' {
            param($f)
            $f.terminals[1].name = 'alacritty'
            $f.terminals[1].role = 'confirmed'
            $f.terminals[1].version = '0.17.0'
            $f.terminals[1].source = $f.terminals[0].source
            $f.terminals[1].executable = $f.terminals[0].executable
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure 'zero executable size' {
        param($f)
        $f.terminals[0].executable.bytes = 0
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'fractional executable size' {
            param($f)
            $f.terminals[0].executable.bytes = 1.5
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'string executable size' {
            param($f)
            $f.terminals[0].executable.bytes = '11000000'
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'oversized executable size' {
            param($f)
            $f.terminals[0].executable.bytes = [long]4294967297
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure 'uppercase SHA-256' {
        param($f)
        $f.terminals[0].executable.sha256 = ('A' * 64)
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure 'short SHA-256' {
        param($f)
        $f.terminals[0].executable.sha256 = ('a' * 63)
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure 'null SHA-256' {
        param($f)
        $f.terminals[0].executable.sha256 = $null
    }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'unsupported Authenticode status' {
            param($f)
            $f.terminals[0].executable.authenticode_status = 'UnknownError'
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'case-only Authenticode status' {
            param($f)
            $f.terminals[0].executable.authenticode_status = 'notsigned'
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'unsigned executable with signer certificate' {
            param($f)
            $f.terminals[0].executable.authenticode_status = 'NotSigned'
            $f.terminals[0].executable.signer_cert_sha256 = ('a' * 64)
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'signed executable without signer certificate' {
            param($f)
            $f.terminals[4].executable.authenticode_status = 'Valid'
            $f.terminals[4].executable.signer_cert_sha256 = $null
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'uppercase signer certificate SHA-256' {
            param($f)
            $f.terminals[4].executable.signer_cert_sha256 = ('A' * 64)
        }
    Invoke-KettlePerfComparatorCampaignObjectFailure `
        'signer certificate on non-string scalar' {
            param($f)
            $f.terminals[4].executable.signer_cert_sha256 = 1
        }

    # A retained manifest file cannot be replaced while Read is inside its
    # snapshot. File and directory reparse points must also fail closed where
    # the current account is permitted to create them.
    $symlinkTarget = Join-Path $scratch 'symlink-target.json'
    Write-KettlePerfComparatorCampaignTestUtf8 $symlinkTarget '{}'
    $script:fixtureSequence++
    $fixture = New-KettlePerfComparatorCampaignFixture `
        $script:fixtureSequence.ToString('x16')
    $directory = Join-Path $script:campaignRoot $fixture.campaign_id
    [void][IO.Directory]::CreateDirectory($directory)
    $symlinkPath = Join-Path $directory 'campaign.json'
    try {
        [void](New-Item -ItemType SymbolicLink -Path $symlinkPath `
            -Target $symlinkTarget -ErrorAction Stop)
        Invoke-KettlePerfComparatorCampaignExpectedFailure `
            -Description 'manifest symbolic link' -Action {
                [void](Read-KettlePerfComparatorCampaign `
                    -Path $symlinkPath `
                    -ExpectedCampaignRoot $script:campaignRoot)
            }
    } catch {
        if (Test-Path -LiteralPath $symlinkPath) {
            throw
        }
        Write-Output (
            'comparator-campaign self-test: SKIP symbolic-link case ' +
            '(account lacks link privilege)'
        )
    }

    # Identity binding rejects every field independently. Its arbitrary parent
    # path is intentionally accepted; only the allowlisted leaf is public and
    # relevant, so no machine-local installation path is trusted or persisted.
    $entry = Get-KettlePerfComparatorCampaignEntry `
        -Campaign $campaign -Name alacritty
    $positiveRecord = New-KettlePerfComparatorTerminalRecord $entry
    Assert-KettlePerfComparatorCampaignTest (
        Test-KettlePerfComparatorCampaignTerminalIdentity `
            $entry $positiveRecord
    ) 'positive arbitrary-parent terminal identity was rejected'
    $identityCases = @(
        @{ name = 'name'; mutate = {
            param($r)
            $r.name = 'rio'
        }},
        @{ name = 'version'; mutate = {
            param($r)
            $r.version = '0.16.1'
        }},
        @{ name = 'executable leaf'; mutate = {
            param($r)
            $r.executable = 'C:\somewhere\cmd.exe'
        }},
        @{ name = 'executable ADS'; mutate = {
            param($r)
            $r.executable = 'C:\somewhere\alacritty.exe:payload'
        }},
        @{ name = 'bytes'; mutate = {
            param($r)
            $r.executable_bytes++
        }},
        @{ name = 'fractional bytes'; mutate = {
            param($r)
            $r.executable_bytes = 11000000.0
        }},
        @{ name = 'SHA'; mutate = {
            param($r)
            $r.executable_sha256 = ('9' * 64)
        }},
        @{ name = 'Authenticode status'; mutate = {
            param($r)
            $r.authenticode_status = if (
                $r.authenticode_status -ceq 'Valid'
            ) {
                'NotSigned'
            } else {
                'Valid'
            }
        }},
        @{ name = 'signer certificate'; mutate = {
            param($r)
            $r.signer_cert_sha256 = ('0' * 64)
        }},
        @{ name = 'role'; mutate = {
            param($r)
            $r.comparator_role = 'advisory'
        }},
        @{ name = 'source kind'; mutate = {
            param($r)
            $r.source.kind = 'unvalidated'
        }},
        @{ name = 'source campaign'; mutate = {
            param($r)
            $r.source.campaign_id = 'different'
        }},
        @{ name = 'source campaign hash'; mutate = {
            param($r)
            $r.source.campaign_sha256 = ('0' * 64)
        }},
        @{ name = 'source origin'; mutate = {
            param($r)
            $r.source.origin = 'https://example.invalid'
        }},
        @{ name = 'source package'; mutate = {
            param($r)
            $r.source.package = 'Untrusted.Package'
        }},
        @{ name = 'source tag'; mutate = {
            param($r)
            $r.source.release_tag = 'latest'
        }},
        @{ name = 'source asset kind'; mutate = {
            param($r)
            $r.source.asset.kind = 'zip'
        }},
        @{ name = 'source asset name'; mutate = {
            param($r)
            $r.source.asset.name = 'other.exe'
        }},
        @{ name = 'source asset URL'; mutate = {
            param($r)
            $r.source.asset.url = 'https://example.invalid'
        }},
        @{ name = 'source asset bytes'; mutate = {
            param($r)
            $r.source.asset.bytes++
        }},
        @{ name = 'source asset SHA'; mutate = {
            param($r)
            $r.source.asset.sha256 = ('0' * 64)
        }},
        @{ name = 'source asset entry'; mutate = {
            param($r)
            $r.source.asset.executable_entry = 'cmd.exe'
        }},
        @{ name = 'source staged file count'; mutate = {
            param($r)
            $r.source.asset.staged_file_count++
        }},
        @{ name = 'source staged total'; mutate = {
            param($r)
            $r.source.asset.staged_total_bytes++
        }},
        @{ name = 'source staged-tree SHA'; mutate = {
            param($r)
            $r.source.asset.staged_tree_sha256 = ('f' * 64)
        }},
        @{ name = 'source staging path'; mutate = {
            param($r)
            $r.source.staging_path = '../alacritty.exe'
        }},
        @{ name = 'missing source'; mutate = {
            param($r)
            $r.PSObject.Properties.Remove('source')
        }}
    )
    foreach ($identityCase in $identityCases) {
        $record = Copy-KettlePerfComparatorTerminalRecord $positiveRecord
        & $identityCase.mutate $record
        Assert-KettlePerfComparatorCampaignTest (
            -not (Test-KettlePerfComparatorCampaignTerminalIdentity `
                -Entry $entry -TerminalRecord $record)
        ) "terminal identity accepted mismatched $($identityCase.name)"
    }
    $uppercaseRecord = Copy-KettlePerfComparatorTerminalRecord $positiveRecord
    $uppercaseRecord.executable_sha256 = (
        $uppercaseRecord.executable_sha256.ToUpperInvariant()
    )
    $uppercaseRecord.source.campaign_sha256 = (
        $uppercaseRecord.source.campaign_sha256.ToUpperInvariant()
    )
    $uppercaseRecord.source.asset.sha256 = (
        $uppercaseRecord.source.asset.sha256.ToUpperInvariant()
    )
    $uppercaseRecord.source.asset.staged_tree_sha256 = (
        $uppercaseRecord.source.asset.staged_tree_sha256.ToUpperInvariant()
    )
    if ($null -ne $uppercaseRecord.signer_cert_sha256) {
        $uppercaseRecord.signer_cert_sha256 = (
            $uppercaseRecord.signer_cert_sha256.ToUpperInvariant()
        )
    }
    Assert-KettlePerfComparatorCampaignTest (
        Test-KettlePerfComparatorCampaignTerminalIdentity `
            -Entry $entry -TerminalRecord $uppercaseRecord
    ) 'terminal identity did not compare canonical SHA-256 case-insensitively'
    Assert-KettlePerfComparatorCampaignTest (
        -not (Test-KettlePerfComparatorCampaignTerminalIdentity `
            -Entry $entry -TerminalRecord $null)
    ) 'terminal identity accepted a null terminal record'
} finally {
    $scratchFull = [IO.Path]::GetFullPath($scratch)
    $tempFull = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    if (-not $scratchFull.StartsWith(
        $tempFull,
        [StringComparison]::OrdinalIgnoreCase
    )) {
        throw 'Refusing unsafe comparator campaign test cleanup'
    }
    if ([IO.Directory]::Exists($scratchFull)) {
        [IO.Directory]::Delete($scratchFull, $true)
    }
}

Write-Output (
    'comparator-campaign self-test: PASS ' +
    "($($PSVersionTable.PSVersion))"
)
