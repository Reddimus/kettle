# Full cross-terminal performance suite - the pinned methodology behind
# docs/PERFORMANCE.md's comparative numbers. Runs every probe and drops JSON
# into target/perf-results (label subdirectory per run).
#
# Usage: pwsh -File perf-all.ps1 -Mode smoke -ManifestOnly `
#   -AllowUnidentifiedDisplay -Label topology-smoke
param(
    [ValidatePattern('^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$')]
    [string]$Label = (Get-Date -Format 'yyyyMMdd-HHmmss'),
    [string[]]$Terminals = @('kettle', 'wt', 'alacritty', 'wezterm', 'rio', 'tabby'),
    [ValidateSet('release', 'smoke')]
    [string]$Mode = 'release',
    [ValidatePattern('^[A-Za-z0-9][A-Za-z0-9._:-]{0,255}$')]
    [string]$BenchmarkSeed = 'kettle-windows-release-v1',
    [ValidateSet('current', 'baseline')]
    [string]$KettleCandidate = 'current',
    [string]$KettleExe = '',
    [AllowEmptyString()]
    [ValidatePattern('^$|^[0-9a-fA-F]{40}$')]
    [string]$ExpectedKettleCommit = '',
    [AllowEmptyString()]
    [ValidatePattern('^$|^[0-9a-fA-F]{64}$')]
    [string]$ExpectedKettleSha256 = '',
    [string]$KettleConfig = '',
    [string]$AlacrittyExe = '',
    [string]$WeztermExe = '',
    [string]$RioExe = '',
    [string]$TabbyExe = '',
    [string]$VtebenchRepo = '',
    [string]$WslExe = '',
    [string]$WslDistribution = '',
    [ValidatePattern('^[0-9a-fA-F]{40}$')]
    [string]$VtebenchRevision = 'ead80032e57dee2e75f0b51f2ea67528647d9944',
    [switch]$SkipVtebench,
    [switch]$SkipLatency,
    [switch]$SkipMenuHover,
    [switch]$SkipNativeDisplay,
    [switch]$SkipMonitorTransition,
    [switch]$ManifestOnly,
    [switch]$SkipKettleBuild,
    [switch]$AllowUnidentifiedDisplay,
    [ValidateRange(1, 10000)]
    [int]$HoverSamples = 200,
    [ValidateRange(1, 1000)]
    [int]$MonitorTransitionSamples = 10,
    [ValidateRange(1, 1000)]
    [int]$StartupRuns = 12,
    [ValidateRange(1, 1000)]
    [int]$IdleSamples = 6,
    [ValidateRange(1, 86400)]
    [int]$IdleSeconds = 10,
    [ValidateRange(1, 10000)]
    [int]$LatencySamples = 60,
    [ValidateRange(1, 1000)]
    [int]$LatencyBlockSize = 10,
    [ValidateRange(0, 1000)]
    [int]$MaxLatencyCensored = 3,
    [ValidateRange(100, 10000)]
    [int]$LatencyTimeoutMs = 800,
    [ValidateRange(1, 1000)]
    [int]$ThroughputIterations = 6,
    [ValidateRange(1, 1000)]
    [int]$MinimumThroughputIterations = 6,
    [ValidateRange(0, 10000)]
    [int]$TerminalOrderOffset = 3,
    [ValidateRange(0, 600)]
    [int]$ProbeCooldownSeconds = 15,
    [ValidateRange(320, 16384)]
    [int]$WindowW = 1280,
    [ValidateRange(240, 16384)]
    [int]$WindowH = 800
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\release-contract.ps1"
$releaseContract = Get-KettlePerfReleaseAcquisitionContract
if ($MinimumThroughputIterations -gt $ThroughputIterations) {
    throw 'MinimumThroughputIterations cannot exceed ThroughputIterations'
}
if ($Mode -eq 'release') {
    $methodologyDeviations = [Collections.Generic.List[string]]::new()
    if (
        -not (Test-KettlePerfOrdinalSequenceEqual `
            -Actual $Terminals -Expected $releaseContract.terminals)
    ) {
        $methodologyDeviations.Add('terminals')
    }
    if ($BenchmarkSeed -cne $releaseContract.benchmark_seed) {
        $methodologyDeviations.Add('benchmark_seed')
    }
    if ($VtebenchRevision -cne $releaseContract.vtebench_revision) {
        $methodologyDeviations.Add('vtebench_revision')
    }
    $numericMethodology = [ordered]@{
        startup_runs = @($StartupRuns, $releaseContract.startup_runs)
        idle_samples = @($IdleSamples, $releaseContract.idle_samples)
        idle_seconds = @($IdleSeconds, $releaseContract.idle_seconds)
        latency_samples = @($LatencySamples, $releaseContract.latency_samples)
        latency_block_size = @(
            $LatencyBlockSize,
            $releaseContract.latency_block_size
        )
        max_latency_censored = @(
            $MaxLatencyCensored,
            $releaseContract.max_latency_censored
        )
        latency_timeout_ms = @(
            $LatencyTimeoutMs,
            $releaseContract.latency_timeout_ms
        )
        throughput_iterations = @(
            $ThroughputIterations,
            $releaseContract.throughput_iterations
        )
        minimum_throughput_iterations = @(
            $MinimumThroughputIterations,
            $releaseContract.minimum_throughput_iterations
        )
        menu_hover_samples = @(
            $HoverSamples,
            $releaseContract.menu_hover_samples
        )
        monitor_transition_samples_per_state = @(
            $MonitorTransitionSamples,
            $releaseContract.monitor_transition_samples_per_state
        )
        terminal_order_offset = @(
            $TerminalOrderOffset,
            $releaseContract.terminal_order_offset
        )
        probe_cooldown_seconds = @(
            $ProbeCooldownSeconds,
            $releaseContract.probe_cooldown_seconds
        )
        window_width = @($WindowW, $releaseContract.window_pixels.width)
        window_height = @($WindowH, $releaseContract.window_pixels.height)
    }
    foreach ($methodologyField in $numericMethodology.GetEnumerator()) {
        if ($methodologyField.Value[0] -ne $methodologyField.Value[1]) {
            $methodologyDeviations.Add([string]$methodologyField.Key)
        }
    }
    if ($methodologyDeviations.Count -gt 0) {
        throw (
            'Release mode requires the canonical acquisition methodology; ' +
            'deviations: ' + ($methodologyDeviations -join ', ')
        )
    }
    if (
        $ManifestOnly -or
        $SkipVtebench -or
        $SkipLatency -or
        $SkipMenuHover -or
        $SkipNativeDisplay -or
        $SkipMonitorTransition -or
        $AllowUnidentifiedDisplay
    ) {
        throw (
            'Release mode does not permit manifest-only acquisition, ' +
            'skipped probes, or unidentified displays'
        )
    }
    if ($KettleConfig) {
        throw 'Release mode requires the generated isolated Kettle configuration'
    }
    $comparatorOverrides = [Collections.Generic.List[string]]::new()
    foreach ($override in ([ordered]@{
        AlacrittyExe = $AlacrittyExe
        WeztermExe = $WeztermExe
        RioExe = $RioExe
        TabbyExe = $TabbyExe
    }).GetEnumerator()) {
        if (-not [string]::IsNullOrWhiteSpace([string]$override.Value)) {
            $comparatorOverrides.Add([string]$override.Key)
        }
    }
    foreach ($environmentName in @(
        'KETTLE_PERF_WT_EXE',
        'KETTLE_PERF_ALACRITTY_EXE',
        'KETTLE_PERF_WEZTERM_EXE',
        'KETTLE_PERF_RIO_EXE',
        'KETTLE_PERF_TABBY_EXE'
    )) {
        if (-not [string]::IsNullOrWhiteSpace(
            [Environment]::GetEnvironmentVariable(
                $environmentName,
                [EnvironmentVariableTarget]::Process
            )
        )) {
            $comparatorOverrides.Add($environmentName)
        }
    }
    if ($comparatorOverrides.Count -gt 0) {
        throw (
            'Release mode requires the pinned offline comparator campaign; ' +
            'forbidden overrides: ' +
            ($comparatorOverrides -join ', ')
        )
    }
    if ($KettleCandidate -eq 'current') {
        if (
            $KettleExe -or
            $SkipKettleBuild -or
            $ExpectedKettleCommit -or
            $ExpectedKettleSha256
        ) {
            throw (
                'A current release candidate must be built from the clean ' +
                'checkout and cannot use external Kettle or baseline pins'
            )
        }
    } elseif (
        -not $KettleExe -or
        -not $SkipKettleBuild -or
        -not $ExpectedKettleCommit -or
        -not $ExpectedKettleSha256
    ) {
        throw (
            'A baseline release candidate requires -KettleExe, ' +
            '-SkipKettleBuild, -ExpectedKettleCommit, and ' +
            '-ExpectedKettleSha256'
        )
    }
} elseif (
    $KettleCandidate -ne 'current' -or
    $ExpectedKettleCommit -or
    $ExpectedKettleSha256
) {
    throw 'Baseline candidate acquisition and expected pins are release-only'
}

. "$PSScriptRoot\harness-provenance.ps1"
$harnessLocks = @()
$resultsRoot = $null
$wslLauncherEvidence = $null
$displayStabilityMonitor = $null
$comparatorCampaign = $null
$comparatorCampaignSetup = $null
$comparatorCampaignEvidence = $null
$comparatorCampaignLeases = [Collections.Generic.List[object]]::new()
$comparatorCampaignStreams = [Collections.Generic.List[IO.Stream]]::new()
$windowsTerminalExecutableLease = $null
$releaseWindowsTerminalExe = ''
$releaseWindowsTerminalPackage = $null
$comparatorEntries = @{}
$comparatorLeasesByName = @{}
$terminalVersions = @{}
try {
$harnessLocks = @(
    Open-KettlePerfHarnessLocks -ScriptDirectory $PSScriptRoot
)
$harnessProvenance = Get-KettlePerfHarnessProvenance -Locks $harnessLocks
. "$PSScriptRoot\lib-win32.ps1"
. "$PSScriptRoot\display-stability.ps1"
. "$PSScriptRoot\comparator-campaign.ps1"
. "$PSScriptRoot\terminal-specs.ps1"
. "$PSScriptRoot\json-io.ps1"
. "$PSScriptRoot\isolated-configs.ps1"
. "$PSScriptRoot\schedule.ps1"
. "$PSScriptRoot\wsl-launcher.ps1"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$gitCommitOutput = @(
    & git -C $repoRoot rev-parse 'HEAD^{commit}' 2>$null
)
$gitCommitExitCode = $LASTEXITCODE
$gitCommit = ($gitCommitOutput | Select-Object -First 1) -join ''
if (
    $gitCommitExitCode -ne 0 -or
    $gitCommit -notmatch '^[0-9a-fA-F]{40}$'
) {
    throw 'Could not resolve the repository HEAD commit'
}
$gitCommit = $gitCommit.ToLowerInvariant()
$gitStatus = @(& git -C $repoRoot status --porcelain=v1 2>$null)
if ($LASTEXITCODE -ne 0) {
    throw 'Could not inspect repository cleanliness'
}
$gitDirty = $gitStatus.Count -gt 0
if ($Mode -eq 'release' -and $gitDirty) {
    throw 'Release acquisition requires a clean repository checkout'
}
if ($Mode -eq 'release') {
    $campaignContract = $releaseContract.comparator_campaign
    $trackedCampaignRoot = Join-Path $PSScriptRoot 'campaigns'
    $trackedCampaignPath = Join-Path `
        $trackedCampaignRoot $campaignContract.relative_path
    $trackedCampaign = Read-KettlePerfComparatorCampaign `
        -Path $trackedCampaignPath `
        -ExpectedCampaignRoot $trackedCampaignRoot
    if (
        $trackedCampaign.campaign_id -cne $campaignContract.campaign_id -or
        $trackedCampaign.campaign_file.relative_path -cne
            $campaignContract.relative_path -or
        [long]$trackedCampaign.campaign_file.bytes -ne
            [long]$campaignContract.bytes -or
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            [string]$trackedCampaign.campaign_file.sha256,
            [string]$campaignContract.sha256
        )
    ) {
        throw 'Tracked comparator campaign differs from the release contract'
    }
    $trackedCampaignStream = [IO.File]::Open(
        $trackedCampaign.campaign_file.path,
        [IO.FileMode]::Open,
        [IO.FileAccess]::Read,
        [IO.FileShare]::Read
    )
    $comparatorCampaignStreams.Add($trackedCampaignStream)

    $setupOutput = @(
        & "$PSScriptRoot\setup-comparator-campaign.ps1" `
            -CampaignId $campaignContract.campaign_id -Offline -PassThru
    )
    $setupMatches = @($setupOutput | Where-Object {
        $null -ne $_ -and
        $null -ne $_.PSObject.Properties['schema'] -and
        $_.schema -ceq 'kettle-comparator-campaign-setup-v1'
    })
    if ($setupMatches.Count -ne 1) {
        throw 'Offline comparator setup did not return one verified campaign'
    }
    $comparatorCampaignSetup = $setupMatches[0]
    $comparatorCampaign = $comparatorCampaignSetup.campaign
    if (
        $comparatorCampaign.campaign_id -cne
            $campaignContract.campaign_id -or
        $comparatorCampaign.campaign_file.relative_path -cne
            $campaignContract.relative_path -or
        [long]$comparatorCampaign.campaign_file.bytes -ne
            [long]$campaignContract.bytes -or
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            [string]$comparatorCampaign.campaign_file.sha256,
            [string]$campaignContract.sha256
        )
    ) {
        throw 'Installed comparator campaign differs from the release contract'
    }
    $localCampaignStream = [IO.File]::Open(
        $comparatorCampaign.campaign_file.path,
        [IO.FileMode]::Open,
        [IO.FileAccess]::Read,
        [IO.FileShare]::Read
    )
    $comparatorCampaignStreams.Add($localCampaignStream)
    $comparatorCampaignEvidence = (
        Get-KettlePerfComparatorCampaignEvidence `
            -Campaign $comparatorCampaign
    )

    foreach ($campaignEntry in $comparatorCampaign.terminals) {
        $name = [string]$campaignEntry.name
        $comparatorEntries[$name] = $campaignEntry
        $terminalVersions[$name] = [string]$campaignEntry.version
        if ($campaignEntry.role -cne 'confirmed') {
            continue
        }
        $lease = Open-KettlePerfComparatorCampaignExecutableLease `
            -Campaign $comparatorCampaign -Entry $campaignEntry `
            -CampaignRoot $comparatorCampaignSetup.campaigns_root `
            -StagingRoot $comparatorCampaignSetup.campaigns_root
        $comparatorCampaignLeases.Add($lease)
        $comparatorLeasesByName[$name] = $lease
        switch -CaseSensitive ($name) {
            'alacritty' { $AlacrittyExe = [string]$lease.path }
            'wezterm' { $WeztermExe = [string]$lease.path }
            'rio' { $RioExe = [string]$lease.path }
            'tabby' { $TabbyExe = [string]$lease.path }
            default {
                throw "Unexpected confirmed comparator campaign entry: $name"
            }
        }
    }
    if (
        $comparatorCampaignLeases.Count -ne 4 -or
        $comparatorEntries.Count -ne 5
    ) {
        throw 'Comparator campaign role or terminal coverage is invalid'
    }
}
$explicitKettleExe = [bool]$KettleExe
if (-not $KettleExe) {
    $KettleExe = Join-Path $PSScriptRoot '..\..\target\release\kettle.exe'
}
$kettleBuildPerformed = $false
$buildCurrentRelease = (
    $Mode -eq 'release' -and $KettleCandidate -eq 'current'
)
$buildSmokeCandidate = (
    $Mode -ne 'release' -and
    -not $ManifestOnly -and
    -not $SkipKettleBuild -and
    -not $explicitKettleExe
)
if ($buildCurrentRelease -or $buildSmokeCandidate) {
    Write-Host 'building the Kettle release candidate from this checkout'
    $buildOutput = @(
        & cargo build --locked --release -p kettle --bins `
            --message-format json-render-diagnostics
    )
    if ($LASTEXITCODE -ne 0) {
        throw "Kettle release build failed with exit $LASTEXITCODE"
    }
    $kettleArtifacts = @(
        $buildOutput | ForEach-Object {
            try {
                $message = $_ | ConvertFrom-Json -ErrorAction Stop
                if (
                    $message.reason -eq 'compiler-artifact' -and
                    $message.target.name -eq 'kettle' -and
                    @($message.target.kind) -contains 'bin' -and
                    $message.executable
                ) {
                    [string]$message.executable
                }
            } catch {
                Write-Verbose "non-JSON Cargo build output: $_"
            }
        } | Select-Object -Unique
    )
    if ($kettleArtifacts.Count -ne 1) {
        throw (
            'Cargo did not report exactly one Kettle GUI release artifact: ' +
            ($kettleArtifacts -join ', ')
        )
    }
    $KettleExe = (Resolve-Path -LiteralPath $kettleArtifacts[0]).Path
    $kettleBuildPerformed = $true
}
$expectedKettleCommitLower = if ($ExpectedKettleCommit) {
    $ExpectedKettleCommit.ToLowerInvariant()
} else {
    ''
}
$expectedKettleShaLower = if ($ExpectedKettleSha256) {
    $ExpectedKettleSha256.ToLowerInvariant()
} else {
    ''
}
$expectedCommitObjectVerified = $false
$expectedCommitIsAncestor = $false
if ($Mode -eq 'release' -and $KettleCandidate -eq 'baseline') {
    & git -C $repoRoot cat-file -e `
        "$expectedKettleCommitLower^{commit}" 2>$null
    $expectedCommitObjectVerified = $LASTEXITCODE -eq 0
    if (-not $expectedCommitObjectVerified) {
        throw 'Expected Kettle baseline commit is not a commit object in this repository'
    }
    & git -C $repoRoot merge-base --is-ancestor `
        $expectedKettleCommitLower $gitCommit 2>$null
    $expectedCommitIsAncestor = $LASTEXITCODE -eq 0
    if (-not $expectedCommitIsAncestor) {
        throw 'Expected Kettle baseline commit is not an ancestor of repository HEAD'
    }
    if (-not (Test-Path -LiteralPath $KettleExe -PathType Leaf)) {
        throw "Pinned Kettle baseline executable not found: $KettleExe"
    }
    $KettleExe = (Resolve-Path -LiteralPath $KettleExe).Path
    $actualPinnedHash = (
        Get-FileHash -LiteralPath $KettleExe -Algorithm SHA256
    ).Hash.ToLowerInvariant()
    if (-not [StringComparer]::Ordinal.Equals(
        $actualPinnedHash,
        $expectedKettleShaLower
    )) {
        throw 'Pinned Kettle baseline executable hash differs from -ExpectedKettleSha256'
    }
}
$runId = [Guid]::NewGuid().ToString('D')

function Get-KettlePerfUtf8Sha256 {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Text
    )

    $bytes = [Text.UTF8Encoding]::new($false, $true).GetBytes($Text)
    $algorithm = [Security.Cryptography.SHA256]::Create()
    try {
        return [BitConverter]::ToString(
            $algorithm.ComputeHash($bytes)
        ).Replace('-', '').ToLowerInvariant()
    } finally {
        $algorithm.Dispose()
        [Array]::Clear($bytes, 0, $bytes.Length)
    }
}

function Get-KettlePerfDisplayTopologySnapshot {
    param(
        [AllowEmptyString()]
        [string]$TargetScreenDevice = ''
    )

    $identity = Get-KettlePerfDisplayIdentityTopology `
        -TargetScreenDevice $TargetScreenDevice `
        -ClientWidth $WindowW -ClientHeight $WindowH `
        -NonClientWidthAllowance $nonClientWidthAllowance `
        -NonClientHeightAllowance $nonClientHeightAllowance
    $snapshot = [pscustomobject][ordered]@{
        schema = 'kettle-display-topology-snapshot-v2'
        captured_at = (Get-Date).ToString('o')
        identity_acquisition = $identity.identity_acquisition
        target_screen_device = $identity.target_screen_device
        primary_screen_device = $identity.primary_screen_device
        target_monitor_hardware_id = $identity.target_monitor_hardware_id
        desktop_screens = [object[]]$identity.desktop_screens
        active_physical_monitors = [object[]]$identity.active_physical_monitors
        active_connections = [object[]]$identity.active_connections
        target_edid_monitors = [object[]]$identity.target_edid_monitors
        identity_issues = [object[]]$identity.issues
    }
    $signatureValue = [pscustomobject][ordered]@{
        schema = $snapshot.schema
        identity_acquisition = $snapshot.identity_acquisition
        target_screen_device = $snapshot.target_screen_device
        primary_screen_device = $snapshot.primary_screen_device
        target_monitor_hardware_id = (
            $snapshot.target_monitor_hardware_id
        )
        desktop_screens = $snapshot.desktop_screens
        active_physical_monitors = $snapshot.active_physical_monitors
        active_connections = $snapshot.active_connections
        target_edid_monitors = $snapshot.target_edid_monitors
        identity_issues = $snapshot.identity_issues
    }
    $signatureJson = ConvertTo-Json -InputObject $signatureValue `
        -Compress -Depth 8
    Add-Member -InputObject $snapshot -NotePropertyName signature_sha256 `
        -NotePropertyValue (Get-KettlePerfUtf8Sha256 $signatureJson)
    return $snapshot
}

$nonClientWidthAllowance = 64
$nonClientHeightAllowance = 96
$displayStabilityMonitor = Start-KettlePerfDisplayStabilityMonitor `
    -RunId $runId
$displayCheckpoints = [Collections.Generic.List[object]]::new()
$displayAcquisitionStart = Get-KettlePerfDisplayTopologySnapshot
$displayCheckpoints.Add([pscustomobject][ordered]@{
    phase = 'start'
    snapshot = $displayAcquisitionStart
})
$desktopScreens = @($displayAcquisitionStart.desktop_screens)
$targetDesktopScreen = @($desktopScreens | Where-Object {
    [StringComparer]::OrdinalIgnoreCase.Equals(
        [string]$_.device_name,
        [string]$displayAcquisitionStart.target_screen_device
    )
}) |
    Select-Object -First 1
$physicalMonitors = @($displayAcquisitionStart.active_physical_monitors)
$monitorConnections = @($displayAcquisitionStart.active_connections)
$targetEdidMonitors = @($displayAcquisitionStart.target_edid_monitors)
$requestedClientFits = (
    $null -ne $targetDesktopScreen -and
    $targetDesktopScreen.working_area.width -ge (
        $WindowW + $nonClientWidthAllowance
    ) -and
    $targetDesktopScreen.working_area.height -ge (
        $WindowH + $nonClientHeightAllowance
    )
)
$nativeWindowW = if ($targetDesktopScreen) {
    [int]$targetDesktopScreen.working_area.width - $nonClientWidthAllowance
} else {
    0
}
$nativeWindowH = if ($targetDesktopScreen) {
    [int]$targetDesktopScreen.working_area.height - $nonClientHeightAllowance
} else {
    0
}
$nativeClientFits = $nativeWindowW -ge 320 -and $nativeWindowH -ge 240
$displayIssues = [System.Collections.Generic.List[string]]::new()
if (-not [bool]$displayStabilityMonitor.registration_succeeded) {
    $displayIssues.Add(
        'continuous Windows display-change monitoring was unavailable'
    )
}
foreach ($identityIssue in @($displayAcquisitionStart.identity_issues)) {
    $displayIssues.Add([string]$identityIssue)
}
if ($desktopScreens.Count -eq 0) {
    $displayIssues.Add('Windows reports no active desktop screens')
} elseif (-not $requestedClientFits) {
    $displayIssues.Add(
        "the primary working area cannot contain the requested ${WindowW}x${WindowH} " +
        "physical-pixel client plus a ${nonClientWidthAllowance}x" +
        "${nonClientHeightAllowance} non-client allowance"
    )
}
if ($targetEdidMonitors.Count -ne 1) {
    $displayIssues.Add(
        'the selected benchmark screen is not mapped to exactly one active ' +
        'EDID-backed physical monitor'
    )
}
if (-not $nativeClientFits) {
    $displayIssues.Add(
        'the selected screen cannot contain the native-display Kettle client'
    )
}

$targetDirectory = Join-Path $repoRoot 'target'
$temporaryPersistenceRoot = $null
try {
    $temporaryPersistenceRoot = if (
        Test-Path -LiteralPath $targetDirectory
    ) {
        Open-KettlePerfPersistenceRoot -Directory $targetDirectory
    } else {
        New-KettlePerfPersistenceRoot `
            -ParentDirectory $repoRoot -LeafName 'target'
    }
} finally {
    Close-KettlePerfPersistenceRoot $temporaryPersistenceRoot
}
$resultsParent = Join-Path $targetDirectory 'perf-results'
$temporaryPersistenceRoot = $null
try {
    $temporaryPersistenceRoot = if (
        Test-Path -LiteralPath $resultsParent
    ) {
        Open-KettlePerfPersistenceRoot -Directory $resultsParent
    } else {
        New-KettlePerfPersistenceRoot `
            -ParentDirectory $targetDirectory -LeafName 'perf-results'
    }
} finally {
    Close-KettlePerfPersistenceRoot $temporaryPersistenceRoot
}
$resultsRoot = New-KettlePerfPersistenceRoot `
    -ParentDirectory $resultsParent -LeafName $Label
$resultsDir = $resultsRoot.RootPath
$isolatedRoot = Join-Path $resultsDir 'isolated-configs'
New-Item -ItemType Directory -Path $isolatedRoot | Out-Null
$isolatedProfile = New-KettlePerfIsolatedConfigs -Root $isolatedRoot
if ($KettleConfig -and -not (
    Test-Path -LiteralPath $KettleConfig -PathType Leaf
)) {
    throw "Kettle benchmark config not found: $KettleConfig"
}
$directKettleConfig = if ($KettleConfig) {
    (Resolve-Path -LiteralPath $KettleConfig).Path
} else {
    [string]$isolatedProfile.terminals.kettle.config_file
}
if (
    $Terminals.Count -lt 6 -or
    ($Terminals.Count % 2) -ne 0 -or
    ($StartupRuns % $Terminals.Count) -ne 0 -or
    ($IdleSamples % $Terminals.Count) -ne 0 -or
    ($LatencySamples % $LatencyBlockSize) -ne 0 -or
    (([int]($LatencySamples / $LatencyBlockSize)) % $Terminals.Count) -ne 0 -or
    ($ThroughputIterations % $Terminals.Count) -ne 0
) {
    throw (
        'Balanced probes require at least six even terminals and complete ' +
        'Williams cycles for startup, idle, latency blocks, and throughput'
    )
}
$scheduleSeeds = [ordered]@{
    startup = "${BenchmarkSeed}:startup"
    idle = "${BenchmarkSeed}:idle"
    latency = "${BenchmarkSeed}:latency"
    throughput = "${BenchmarkSeed}:throughput"
}
$schedulePreviews = [ordered]@{
    startup = New-KettlePerfWilliamsSchedule -Terminals $Terminals `
        -Seed $scheduleSeeds.startup `
        -Cycles ([int]($StartupRuns / $Terminals.Count)) `
        -Namespace 'startup'
    idle = New-KettlePerfWilliamsSchedule -Terminals $Terminals `
        -Seed $scheduleSeeds.idle `
        -Cycles ([int]($IdleSamples / $Terminals.Count)) `
        -Namespace 'idle'
    latency = New-KettlePerfWilliamsSchedule -Terminals $Terminals `
        -Seed $scheduleSeeds.latency `
        -Cycles ([int](($LatencySamples / $LatencyBlockSize) / $Terminals.Count)) `
        -Namespace 'latency'
    throughput = New-KettlePerfWilliamsSchedule -Terminals $Terminals `
        -Seed $scheduleSeeds.throughput `
        -Cycles ([int]($ThroughputIterations / $Terminals.Count)) `
        -Namespace 'throughput'
}
$terminalArgs = @{
    RunId = $runId
    TargetScreenDevice = $targetDesktopScreen.device_name
    ResultsDir = $resultsDir
    KettleExe = $KettleExe
    KettleConfig = $KettleConfig
    WindowsTerminalExe = $releaseWindowsTerminalExe
    IsolatedProfile = $isolatedProfile
    AlacrittyExe = $AlacrittyExe
    WeztermExe = $WeztermExe
    RioExe = $RioExe
    TabbyExe = $TabbyExe
    TerminalVersions = $terminalVersions
}

function Get-RotatedTerminalOrder([string[]]$Names, [int]$Offset) {
    if ($Names.Count -eq 0) {
        return @()
    }
    $start = $Offset % $Names.Count
    return @(
        for ($i = 0; $i -lt $Names.Count; $i++) {
            $Names[($start + $i) % $Names.Count]
        }
    )
}

$vtebenchOrder = Get-RotatedTerminalOrder `
    $Terminals ($TerminalOrderOffset + 3)
$pwshCommand = Get-Command pwsh.exe -CommandType Application -ErrorAction Stop |
    Select-Object -First 1
$throughputPowerShell = (Resolve-Path -LiteralPath $pwshCommand.Source).Path
$throughputPowerShellVersion = (
    & $throughputPowerShell -NoLogo -NoProfile -Command `
        '$PSVersionTable.PSVersion.ToString()'
) -join ''
$orchestratorPowerShell = (Get-Process -Id $PID).Path
$latencyWorkloadCommand = Get-Command cmd.exe -CommandType Application `
    -ErrorAction Stop |
    Select-Object -First 1
$latencyWorkloadExecutable = (
    Resolve-Path -LiteralPath $latencyWorkloadCommand.Source -ErrorAction Stop
).Path
$latencyWorkloadItem = Get-Item -LiteralPath $latencyWorkloadExecutable `
    -Force -ErrorAction Stop
if (
    $latencyWorkloadItem.PSIsContainer -or
    ($latencyWorkloadItem.Attributes -band
        [IO.FileAttributes]::ReparsePoint) -ne 0 -or
    [IO.Path]::GetFileName($latencyWorkloadExecutable) -cne 'cmd.exe'
) {
    throw (
        'The latency workload must resolve to an ordinary cmd.exe file: ' +
        $latencyWorkloadExecutable
    )
}
$latencyWorkloadVersionInfo = (
    [Diagnostics.FileVersionInfo]::GetVersionInfo(
        $latencyWorkloadExecutable
    )
)
$latencyWorkloadVersion = if ($latencyWorkloadVersionInfo.ProductVersion) {
    $latencyWorkloadVersionInfo.ProductVersion
} else {
    $latencyWorkloadVersionInfo.FileVersion
}
if (-not $latencyWorkloadVersion) {
    throw 'Could not determine the cmd.exe latency workload version'
}
$wslLauncherEvidence = $null
$wslDistributionEvidence = $null
if (-not $SkipVtebench) {
    $wslLauncherEvidence = Open-KettlePerfWslLauncherEvidence `
        -Path $WslExe
    $WslExe = $wslLauncherEvidence.Path
    $WslDistribution = Resolve-KettlePerfWslDistribution `
        -WslExe $WslExe -Name $WslDistribution
    $wslDistributionEvidence = Get-KettlePerfWslDistributionEvidence `
        -WslExe $WslExe -Distribution $WslDistribution
}

function Get-TerminalConfigProvenance($Name, $Spec) {
    if ($Spec.ConfigurationMode -in @('benchmark-isolated', 'explicit')) {
        return [ordered]@{
            mode = $Spec.ConfigurationMode
            claim_eligible = $Spec.ConfigurationMode -eq 'benchmark-isolated'
            files = [object[]]@($Spec.ConfigurationEvidence)
        }
    }
    $candidates = if ($Name -eq 'wt') {
        @(
            (Join-Path $env:LOCALAPPDATA (
                'Packages\Microsoft.WindowsTerminal_8wekyb3d8bbwe\' +
                'LocalState\settings.json'
            )),
            (Join-Path $env:LOCALAPPDATA (
                'Microsoft\Windows Terminal\settings.json'
            ))
        )
    } else {
        @()
    }
    $files = @(
        foreach ($candidate in $candidates) {
            if (-not $candidate -or -not (
                Test-Path -LiteralPath $candidate -PathType Leaf
            )) {
                continue
            }
            $path = (Resolve-Path -LiteralPath $candidate).Path
            $item = Get-Item -LiteralPath $path
            [ordered]@{
                path = $path
                bytes = $item.Length
                sha256 = (
                    Get-FileHash -LiteralPath $path -Algorithm SHA256
                ).Hash
            }
        }
    )
    $mode = if ($files.Count -gt 0) {
        'advisory-user-config'
    } else {
        'advisory-built-in-default'
    }
    return [ordered]@{
        mode = $mode
        claim_eligible = $false
        files = [object[]]$files
    }
}

function Get-KettlePerfWindowsTerminalPackageEvidence {
    param(
        [Parameter(Mandatory)]
        [string]$ExpectedVersion,
        [Parameter(Mandatory)]
        [string]$Executable
    )

    $packages = @(
        Get-AppxPackage -Name Microsoft.WindowsTerminal -ErrorAction Stop
    )
    if (
        $packages.Count -ne 1 -or
        [string]$packages[0].Version -cne $ExpectedVersion
    ) {
        throw (
            'Windows Terminal campaign requires exactly one installed Appx ' +
            "version $ExpectedVersion"
        )
    }
    $package = $packages[0]
    $expectedExecutable = [IO.Path]::GetFullPath(
        (Join-Path $package.InstallLocation 'WindowsTerminal.exe')
    )
    if (-not [StringComparer]::OrdinalIgnoreCase.Equals(
        $expectedExecutable,
        [IO.Path]::GetFullPath($Executable)
    )) {
        throw 'Windows Terminal executable is outside the pinned Appx package'
    }
    $evidence = [pscustomobject][ordered]@{
        schema = 'kettle-windows-terminal-appx-v1'
        name = [string]$package.Name
        publisher_id = [string]$package.PublisherId
        package_family_name = [string]$package.PackageFamilyName
        package_full_name = [string]$package.PackageFullName
        version = [string]$package.Version
        architecture = [string]$package.Architecture
        status = [string]$package.Status
        signature_kind = [string]$package.SignatureKind
        is_framework = [bool]$package.IsFramework
        non_removable = [bool]$package.NonRemovable
        install_location = [string]$package.InstallLocation
    }
    if (
        $evidence.name -cne 'Microsoft.WindowsTerminal' -or
        $evidence.publisher_id -cne '8wekyb3d8bbwe' -or
        $evidence.package_family_name -cne
            'Microsoft.WindowsTerminal_8wekyb3d8bbwe' -or
        $evidence.version -cne $ExpectedVersion -or
        $evidence.architecture -cne 'X64' -or
        $evidence.status -cne 'Ok' -or
        $evidence.signature_kind -cne 'Store' -or
        $evidence.is_framework -ne $false
    ) {
        throw 'Windows Terminal Appx package identity is not release-eligible'
    }
    return $evidence
}

if ($Mode -eq 'release') {
    $windowsTerminalEntry = $comparatorEntries['wt']
    $installedWindowsTerminalPackages = @(
        Get-AppxPackage -Name Microsoft.WindowsTerminal -ErrorAction Stop
    )
    if (
        $installedWindowsTerminalPackages.Count -ne 1 -or
        [string]$installedWindowsTerminalPackages[0].Version -cne
            [string]$windowsTerminalEntry.version
    ) {
        throw (
            'Windows Terminal campaign requires exactly one installed Appx ' +
            "version $($windowsTerminalEntry.version)"
        )
    }
    $windowsTerminalHost = Join-Path `
        $installedWindowsTerminalPackages[0].InstallLocation `
        'WindowsTerminal.exe'
    if (-not (Test-Path -LiteralPath $windowsTerminalHost -PathType Leaf)) {
        throw "Windows Terminal hosted executable not found: $windowsTerminalHost"
    }
    $releaseWindowsTerminalExe = (
        Resolve-Path -LiteralPath $windowsTerminalHost -ErrorAction Stop
    ).Path
    $releaseWindowsTerminalPackage = (
        Get-KettlePerfWindowsTerminalPackageEvidence `
            -ExpectedVersion $windowsTerminalEntry.version `
            -Executable $releaseWindowsTerminalExe
    )
    $windowsTerminalExecutableLease = Open-KettlePerfExecutableLease `
        -Executable $releaseWindowsTerminalExe `
        -ExpectedSha256 $windowsTerminalEntry.executable.sha256
    if (
        [long]$windowsTerminalExecutableLease.Length -ne
            [long]$windowsTerminalEntry.executable.bytes
    ) {
        Close-KettlePerfExecutableLease $windowsTerminalExecutableLease
        $windowsTerminalExecutableLease = $null
        throw 'Windows Terminal Appx host length differs from comparator campaign'
    }
    $terminalArgs.WindowsTerminalExe = $releaseWindowsTerminalExe
}

$terminalManifest = @()
foreach ($terminal in $Terminals) {
    $isolatedConfig = Get-KettlePerfIsolatedConfigEntry `
        -ConfigProfile $isolatedProfile -Name $terminal
    if ($terminal -eq 'kettle' -and $KettleConfig) {
        $isolatedConfig = $null
    }
    $versionOverride = if ($terminalVersions.ContainsKey($terminal)) {
        [string]$terminalVersions[$terminal]
    } else {
        ''
    }
    $spec = Resolve-KettlePerfTerminal -Name $terminal -KettleExe $KettleExe `
        -KettleConfig $KettleConfig `
        -WindowsTerminalExe $releaseWindowsTerminalExe `
        -AlacrittyExe $AlacrittyExe `
        -WeztermExe $WeztermExe -RioExe $RioExe -TabbyExe $TabbyExe `
        -VersionOverride $versionOverride -IsolatedConfig $isolatedConfig
    $terminalRecord = [ordered]@{
        name = $terminal
        available = $spec.Available
        launcher = $spec.Exe
        executable = $spec.BenchmarkExe
        executable_sha256 = if ($spec.Available) {
            Get-KettlePerfExecutableSha256 $spec.BenchmarkExe
        } else {
            $null
        }
        version = Get-KettlePerfVersion $spec
        command_workloads = $spec.SupportsCommand
        command_confirmation = $spec.CommandConfirmation
        helper_binaries = [object[]]@($spec.HelperBinaries)
        configuration = Get-TerminalConfigProvenance $terminal $spec
    }
    if ($terminal -eq 'wt') {
        $terminalRecord['launch_mode'] = $spec.WindowsTerminalLaunchMode
    }
    if ($Mode -eq 'release' -and $terminal -ne 'kettle') {
        if (-not $comparatorEntries.ContainsKey($terminal)) {
            throw "Comparator campaign has no entry for $terminal"
        }
        $campaignEntry = $comparatorEntries[$terminal]
        $authenticodeStatus = $null
        $signerCertSha256 = $null
        if ($campaignEntry.role -ceq 'confirmed') {
            if (-not $comparatorLeasesByName.ContainsKey($terminal)) {
                throw "Comparator campaign has no retained lease for $terminal"
            }
            $lease = $comparatorLeasesByName[$terminal]
            $authenticodeStatus = [string]$lease.authenticode_status
            $signerCertSha256 = $lease.signer_cert_sha256
        } elseif ($terminal -ceq 'wt') {
            if (
                $null -eq $windowsTerminalExecutableLease -or
                $null -eq $windowsTerminalExecutableLease.Stream -or
                -not $windowsTerminalExecutableLease.Stream.CanRead -or
                -not [StringComparer]::OrdinalIgnoreCase.Equals(
                    [string]$windowsTerminalExecutableLease.Path,
                    [string]$spec.Exe
                ) -or
                -not [StringComparer]::OrdinalIgnoreCase.Equals(
                    [string]$spec.Exe,
                    [string]$spec.BenchmarkExe
                )
            ) {
                throw (
                    'Windows Terminal release launcher is not the retained ' +
                    'installed Appx host'
                )
            }
            $signature = Get-AuthenticodeSignature `
                -FilePath $spec.BenchmarkExe -ErrorAction Stop
            $authenticodeStatus = [string]$signature.Status
            $signerCertSha256 = if ($null -ne $signature.SignerCertificate) {
                Get-KettlePerfComparatorCertificateSha256 `
                    -Certificate $signature.SignerCertificate
            } else {
                $null
            }
            $terminalRecord['installed_package'] = $releaseWindowsTerminalPackage
        } else {
            throw "Comparator campaign has an unsupported advisory peer: $terminal"
        }
        $terminalRecord['executable_bytes'] = [long](
            Get-Item -LiteralPath $spec.BenchmarkExe -Force
        ).Length
        $terminalRecord['authenticode_status'] = $authenticodeStatus
        $terminalRecord['signer_cert_sha256'] = $signerCertSha256
        $terminalRecord['comparator_role'] = [string]$campaignEntry.role
        $terminalRecord['source'] = (
            New-KettlePerfComparatorTerminalSource -Entry $campaignEntry
        )
        if (-not (Test-KettlePerfComparatorCampaignTerminalIdentity `
            -Entry $campaignEntry -TerminalRecord $terminalRecord)) {
            throw "Terminal identity differs from comparator campaign: $terminal"
        }
    }
    $terminalManifest += $terminalRecord
}
$computer = Get-CimInstance Win32_ComputerSystem -ErrorAction SilentlyContinue
$manufacturer = $null
$model = $null
$totalMemoryBytes = $null
if ($null -ne $computer) {
    $manufacturer = $computer.Manufacturer
    $model = $computer.Model
    if ($null -ne $computer.TotalPhysicalMemory) {
        $totalMemoryBytes = [long]$computer.TotalPhysicalMemory
    }
}
$processors = @(Get-CimInstance Win32_Processor -ErrorAction SilentlyContinue |
    ForEach-Object {
        [ordered]@{
            name = $_.Name
            cores = $_.NumberOfCores
            logical_processors = $_.NumberOfLogicalProcessors
            max_clock_mhz = $_.MaxClockSpeed
        }
    })
$video = @(Get-CimInstance Win32_VideoController -ErrorAction SilentlyContinue |
    ForEach-Object {
        [ordered]@{
            name = $_.Name
            driver_version = $_.DriverVersion
            driver_date = if ($_.DriverDate) { ([datetime]$_.DriverDate).ToString('o') } else { $null }
            current_width = $_.CurrentHorizontalResolution
            current_height = $_.CurrentVerticalResolution
            current_refresh_hz = $_.CurrentRefreshRate
        }
    })
$powerScheme = try {
    (& powercfg.exe /getactivescheme 2>$null) -join "`n"
} catch {
    $null
}
$kettleRecord = @(
    $terminalManifest | Where-Object { $_.name -eq 'kettle' }
)
if ($kettleRecord.Count -eq 1) {
    $embeddedCommit = $null
    $embeddedAbbreviation = $null
    $embeddedDirty = $null
    if (
        [string]$kettleRecord[0].version -match
            '\(([0-9a-fA-F]{7,40})(\+dirty)?\)'
    ) {
        $embeddedAbbreviation = $Matches[1]
        $embeddedDirty = [bool]$Matches[2]
        $embeddedCommit = try {
            (
                & git -C $repoRoot rev-parse `
                    "$embeddedAbbreviation^{commit}" 2>$null |
                    Select-Object -First 1
            ) -join ''
        } catch {
            $null
        }
    }
    $actualKettleHash = [string]$kettleRecord[0].executable_sha256
    if ($Mode -eq 'release') {
        if (
            -not $embeddedAbbreviation -or
            $embeddedCommit -notmatch '^[0-9a-fA-F]{40}$' -or
            $embeddedDirty -ne $false
        ) {
            throw 'Release Kettle version lacks a clean embedded commit identity'
        }
        $embeddedCommit = $embeddedCommit.ToLowerInvariant()
        if ($KettleCandidate -eq 'current') {
            if (
                -not $kettleBuildPerformed -or
                $embeddedCommit -cne $gitCommit
            ) {
                throw (
                    'Current Kettle release candidate was not built from ' +
                    'the clean repository HEAD'
                )
            }
        } elseif (
            $kettleBuildPerformed -or
            $embeddedCommit -cne $expectedKettleCommitLower -or
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                $actualKettleHash,
                $expectedKettleShaLower
            )
        ) {
            throw 'Pinned Kettle baseline identity differs from its expected commit/hash'
        }
    }
    $kettleRecord[0].source = [ordered]@{
        candidate = $KettleCandidate
        acquisition = if (
            $Mode -eq 'release' -and $KettleCandidate -eq 'baseline'
        ) {
            'pinned-external'
        } else {
            'repository'
        }
        embedded_commit = $embeddedCommit
        embedded_commit_abbreviation = $embeddedAbbreviation
        embedded_dirty = $embeddedDirty
        expected_commit = if ($ExpectedKettleCommit) {
            $expectedKettleCommitLower
        } else {
            $null
        }
        expected_sha256 = if ($ExpectedKettleSha256) {
            $expectedKettleShaLower
        } else {
            $null
        }
        actual_sha256 = if ($actualKettleHash) {
            $actualKettleHash.ToLowerInvariant()
        } else {
            $null
        }
        commit_object_verified = if ($KettleCandidate -eq 'baseline') {
            $expectedCommitObjectVerified
        } else {
            $true
        }
        commit_is_ancestor = if ($KettleCandidate -eq 'baseline') {
            $expectedCommitIsAncestor
        } else {
            $true
        }
        external_executable = (
            $Mode -eq 'release' -and $KettleCandidate -eq 'baseline'
        )
        skip_build = [bool]$SkipKettleBuild
        build_performed = $kettleBuildPerformed
        release_build_performed = $kettleBuildPerformed
    }
}
$manifest = [ordered]@{
    schema_version = if ($Mode -eq 'release') { 4 } else { 2 }
    run_id = $runId
    timestamp = (Get-Date).ToString('o')
    label = $Label
    repository_commit = $gitCommit
    repository_dirty = $gitDirty
    kettle_config = $directKettleConfig
    kettle_config_sha256 = (
        Get-FileHash -LiteralPath $directKettleConfig -Algorithm SHA256
    ).Hash
    harness_provenance = $harnessProvenance
    comparator_campaign = if ($Mode -eq 'release') {
        $comparatorCampaignEvidence
    } else {
        $null
    }
    isolated_configuration = [ordered]@{
        schema_version = $isolatedProfile.schema_version
        root = $isolatedProfile.root
        benchmark_profile = $isolatedProfile.benchmark_profile
        files = [object[]]@($isolatedProfile.files)
        windows_terminal_note = (
            'installed Windows Terminal has no per-launch settings-file flag; ' +
            'its measurements are advisory and excluded from confirmed wins'
        )
    }
    os = [ordered]@{
        description = [System.Runtime.InteropServices.RuntimeInformation]::OSDescription
        version = [Environment]::OSVersion.VersionString
        architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    }
    toolchain = [ordered]@{
        orchestrator_powershell = [ordered]@{
            path = $orchestratorPowerShell
            sha256 = (
                Get-FileHash -LiteralPath $orchestratorPowerShell `
                    -Algorithm SHA256
            ).Hash
            edition = $PSVersionTable.PSEdition
            version = $PSVersionTable.PSVersion.ToString()
        }
        throughput_powershell = [ordered]@{
            path = $throughputPowerShell
            sha256 = (
                Get-FileHash -LiteralPath $throughputPowerShell `
                    -Algorithm SHA256
            ).Hash
            edition = 'Core'
            version = $throughputPowerShellVersion
        }
        latency_workload = [ordered]@{
            path = $latencyWorkloadExecutable
            sha256 = Get-KettlePerfExecutableSha256 `
                $latencyWorkloadExecutable
            version = $latencyWorkloadVersion
        }
        vtebench_wsl = if ($null -ne $wslLauncherEvidence) {
            [ordered]@{
                path = $wslLauncherEvidence.Path
                sha256 = $wslLauncherEvidence.Sha256
                version = $wslLauncherEvidence.Version
                file_version = $wslLauncherEvidence.FileVersion
                runtime_version = $wslLauncherEvidence.RuntimeVersion
                version_output = $wslLauncherEvidence.VersionOutput
                version_output_sha256 = (
                    $wslLauncherEvidence.VersionOutputSha256
                )
                resolution_policy = (
                    $wslLauncherEvidence.ResolutionPolicy
                )
                distribution = [ordered]@{
                    schema = $wslDistributionEvidence.Schema
                    name = $wslDistributionEvidence.Name
                    os_release_path = (
                        $wslDistributionEvidence.OsReleasePath
                    )
                    os_release_sha256 = (
                        $wslDistributionEvidence.OsReleaseSha256
                    )
                    os_pretty_line = (
                        $wslDistributionEvidence.OsPrettyLine
                    )
                    os_version_line = (
                        $wslDistributionEvidence.OsVersionLine
                    )
                    kernel_release = (
                        $wslDistributionEvidence.KernelRelease
                    )
                    kernel_version = (
                        $wslDistributionEvidence.KernelVersion
                    )
                    architecture = (
                        $wslDistributionEvidence.Architecture
                    )
                    user_name = $wslDistributionEvidence.UserName
                    user_id = $wslDistributionEvidence.UserId
                }
            }
        } else {
            $null
        }
    }
    machine = [ordered]@{
        manufacturer = $manufacturer
        model = $model
        total_memory_bytes = $totalMemoryBytes
        processors = $processors
        video_controllers = $video
        display_topology = [ordered]@{
            acquisition_schema = 'kettle-display-topology-acquisition-v2'
            acquisition_start = $displayAcquisitionStart
            acquisition_end = $null
            stability_monitoring = $null
            start_signature_sha256 = (
                $displayAcquisitionStart.signature_sha256
            )
            end_signature_sha256 = $null
            topology_stable = $false
            desktop_screens = $desktopScreens
            target_screen_device = if ($targetDesktopScreen) {
                $targetDesktopScreen.device_name
            } else {
                $null
            }
            non_client_allowance = [ordered]@{
                width = $nonClientWidthAllowance
                height = $nonClientHeightAllowance
            }
            active_physical_monitors = $physicalMonitors
            active_connections = $monitorConnections
            target_edid_monitors = $targetEdidMonitors
            requested_client_fits = $requestedClientFits
            native_client = [ordered]@{
                width = $nativeWindowW
                height = $nativeWindowH
                fits = $nativeClientFits
            }
            start_evidence_valid = (
                $displayIssues.Count -eq 0 -and
                $requestedClientFits -and
                $nativeClientFits -and
                $targetEdidMonitors.Count -eq 1
            )
            release_evidence_valid = $false
            issues = [object[]]$displayIssues
        }
        active_power_scheme = $powerScheme
    }
    settings = [ordered]@{
        mode = $Mode
        benchmark_seed = $BenchmarkSeed
        kettle_candidate = $KettleCandidate
        expected_kettle_commit = if ($ExpectedKettleCommit) {
            $expectedKettleCommitLower
        } else {
            $null
        }
        expected_kettle_sha256 = if ($ExpectedKettleSha256) {
            $expectedKettleShaLower
        } else {
            $null
        }
        terminals = $Terminals
        comparator_campaign_id = if ($Mode -eq 'release') {
            [string]$comparatorCampaign.campaign_id
        } else {
            $null
        }
        window_pixels = @{ width = $WindowW; height = $WindowH }
        native_window_pixels = @{
            width = $nativeWindowW
            height = $nativeWindowH
        }
        startup_runs = $StartupRuns
        idle_samples = $IdleSamples
        idle_seconds = $IdleSeconds
        latency_samples = $LatencySamples
        latency_block_size = $LatencyBlockSize
        max_latency_censored = $MaxLatencyCensored
        latency_timeout_ms = $LatencyTimeoutMs
        menu_hover_samples = $HoverSamples
        menu_hover_block_size = $releaseContract.menu_hover_block_size
        native_display_enabled = -not [bool]$SkipNativeDisplay
        monitor_transition_samples_per_state = $MonitorTransitionSamples
        throughput_iterations = $ThroughputIterations
        minimum_throughput_iterations = $MinimumThroughputIterations
        terminal_order_offset = $TerminalOrderOffset
        vtebench_terminal_order = $vtebenchOrder
        schedules = [ordered]@{
            startup = $schedulePreviews.startup
            idle = $schedulePreviews.idle
            latency = $schedulePreviews.latency
            throughput = $schedulePreviews.throughput
        }
        vtebench_enabled = -not [bool]$SkipVtebench
        monitor_transition_enabled = -not [bool]$SkipMonitorTransition
        probe_cooldown_seconds = $ProbeCooldownSeconds
        vtebench_revision = $VtebenchRevision.ToLowerInvariant()
        unidentified_display_allowed = [bool]$AllowUnidentifiedDisplay
        kettle_build_skipped = [bool]$SkipKettleBuild
    }
    terminals = $terminalManifest
}
Write-KettlePerfJsonFile `
    -Path (Join-Path $resultsDir 'benchmark-manifest.json') `
    -InputObject $manifest -Depth 8 -Root $resultsRoot
if ($ManifestOnly) {
    Write-Host "benchmark manifest written to $resultsDir"
    foreach ($issue in $displayIssues) {
        Write-Warning $issue
    }
    return
}

if (
    $PSVersionTable.PSEdition -ne 'Core' -or
    $PSVersionTable.PSVersion.Major -lt 7
) {
    throw (
        'Release benchmarks require PowerShell 7 so capture/orchestration ' +
        'overhead is pinned. Re-run with pwsh; Windows PowerShell 5.1 remains ' +
        'supported for schema/self-test smoke checks.'
    )
}
if (-not $requestedClientFits) {
    throw ($displayIssues -join '; ')
}
if (-not $nativeClientFits -and -not $SkipNativeDisplay) {
    throw ($displayIssues -join '; ')
}
if ($targetEdidMonitors.Count -ne 1 -and -not $AllowUnidentifiedDisplay) {
    throw (
        ($displayIssues -join '; ') +
        '. Reconnect an EDID-backed physical display, or use ' +
        '-AllowUnidentifiedDisplay only for non-release smoke testing.'
    )
}

$unavailableTerminals = @(
    $terminalManifest | Where-Object { -not $_.available }
)
if ($unavailableTerminals.Count -gt 0) {
    throw (
        'Benchmark terminals are unavailable: ' +
        (($unavailableTerminals | ForEach-Object { $_.name }) -join ', ')
    )
}
$configLocks = [Collections.Generic.List[IO.FileStream]]::new()
try {
foreach ($configFile in $isolatedProfile.files) {
    $lock = [IO.FileStream]::new(
        [string]$configFile.path,
        [IO.FileMode]::Open,
        [IO.FileAccess]::Read,
        [IO.FileShare]::Read
    )
    try {
        $sha = [Security.Cryptography.SHA256]::Create()
        try {
            $actualHash = [Convert]::ToHexString(
                $sha.ComputeHash($lock)
            )
        } finally {
            $sha.Dispose()
        }
        if (
            $lock.Length -ne [long]$configFile.bytes -or
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                $actualHash,
                [string]$configFile.sha256
            )
        ) {
            throw "Isolated config changed before lock: $($configFile.path)"
        }
        $lock.Position = 0
        $configLocks.Add($lock)
        $lock = $null
    } finally {
        if ($null -ne $lock) {
            $lock.Dispose()
        }
    }
}

Write-Host "=== kettle perf suite - label: $Label ==="
function Start-KettlePerfProbeCooldown {
    param([string]$After)

    if ($ProbeCooldownSeconds -gt 0) {
        Write-Host (
            "--- cooldown after $After ($ProbeCooldownSeconds seconds) ---"
        )
        Start-Sleep -Seconds $ProbeCooldownSeconds
    }
}

function Add-KettlePerfDisplayCheckpoint {
    param(
        [Parameter(Mandatory)]
        [ValidatePattern('^[a-z0-9][a-z0-9-]{0,63}$')]
        [string]$Phase
    )

    $snapshot = Get-KettlePerfDisplayTopologySnapshot `
        -TargetScreenDevice $displayAcquisitionStart.target_screen_device
    $displayCheckpoints.Add([pscustomobject][ordered]@{
        phase = $Phase
        snapshot = $snapshot
    })
    return $snapshot
}

Write-Host "--- startup / fresh memory / idle CPU ---"
& "$PSScriptRoot\startup-idle.ps1" @terminalArgs `
    -Terminals $Terminals `
    -StartupRuns $StartupRuns -IdleSamples $IdleSamples `
    -IdleSeconds $IdleSeconds `
    -StartupScheduleSeed $scheduleSeeds.startup `
    -IdleScheduleSeed $scheduleSeeds.idle `
    -WindowW $WindowW -WindowH $WindowH
Start-KettlePerfProbeCooldown 'startup/idle'
[void](Add-KettlePerfDisplayCheckpoint -Phase 'after-startup-idle')

if (-not $SkipLatency) {
    Write-Host "--- input latency probe ---"
    & "$PSScriptRoot\latency.ps1" @terminalArgs `
        -Terminals $Terminals `
        -Samples $LatencySamples -BlockSize $LatencyBlockSize `
        -MaxCensored $MaxLatencyCensored `
        -SampleTimeoutMs $LatencyTimeoutMs `
        -ScheduleSeed $scheduleSeeds.latency `
        -WindowW $WindowW -WindowH $WindowH
    Start-KettlePerfProbeCooldown 'input latency'
    [void](Add-KettlePerfDisplayCheckpoint -Phase 'after-input-latency')
}

if (-not $SkipMenuHover) {
    Write-Host "--- Kettle context-menu hover latency / pacing ---"
    & "$PSScriptRoot\menu-hover.ps1" -ResultsDir $resultsDir `
        -KettleExe $KettleExe -ConfigPath $directKettleConfig `
        -RunId $runId -Samples $HoverSamples `
        -BlockSize $releaseContract.menu_hover_block_size `
        -TargetScreenDevice $targetDesktopScreen.device_name `
        -WindowW $WindowW -WindowH $WindowH -NoFail
    Start-KettlePerfProbeCooldown 'menu hover'
    [void](Add-KettlePerfDisplayCheckpoint -Phase 'after-menu-hover')
}

if (-not $SkipNativeDisplay) {
    Write-Host "--- Kettle native-display menu-hover ROI latency ---"
    & "$PSScriptRoot\menu-hover.ps1" -ResultsDir $resultsDir `
        -KettleExe $KettleExe -ConfigPath $directKettleConfig `
        -RunId $runId -Samples $HoverSamples `
        -BlockSize $releaseContract.menu_hover_block_size `
        -TargetScreenDevice $targetDesktopScreen.device_name `
        -WindowW $nativeWindowW -WindowH $nativeWindowH `
        -ResultFileName 'native-display-menu-hover.json' `
        -Variant native-display -NoFail
    Start-KettlePerfProbeCooldown 'native-display menu hover'
    [void](Add-KettlePerfDisplayCheckpoint `
        -Phase 'after-native-display-menu-hover')
}

if (-not $SkipMonitorTransition) {
    Write-Host "--- Kettle cross-monitor DPI/swapchain recovery ---"
    & "$PSScriptRoot\monitor-transition.ps1" `
        -ResultsDir $resultsDir -KettleExe $KettleExe `
        -ConfigPath $directKettleConfig -RunId $runId `
        -Samples $MonitorTransitionSamples `
        -WindowW $WindowW -WindowH $WindowH
    Start-KettlePerfProbeCooldown 'monitor transition'
    [void](Add-KettlePerfDisplayCheckpoint -Phase 'after-monitor-transition')
}

Write-Host "--- throughput (console write through parser-drain response) ---"
& "$PSScriptRoot\throughput.ps1" @terminalArgs `
    -Terminals $Terminals `
    -PowerShellExe $throughputPowerShell `
    -WindowW $WindowW -WindowH $WindowH `
    -Iterations $ThroughputIterations `
    -ScheduleSeed $scheduleSeeds.throughput
Start-KettlePerfProbeCooldown 'throughput'
[void](Add-KettlePerfDisplayCheckpoint -Phase 'after-throughput')

if (-not $SkipVtebench) {
    Write-Host "--- vtebench (WSL PTY read) ---"
    & "$PSScriptRoot\vtebench-wsl.ps1" @terminalArgs `
        -Terminals $vtebenchOrder `
        -PowerShellExe $throughputPowerShell `
        -WslExe $WslExe `
        -WslDistribution $WslDistribution `
        -WslResolutionPolicy $wslLauncherEvidence.ResolutionPolicy `
        -VtebenchRepo $VtebenchRepo `
        -VtebenchRevision $VtebenchRevision `
        -WindowW $WindowW -WindowH $WindowH
    [void](Add-KettlePerfDisplayCheckpoint -Phase 'after-vtebench')
}

$displayAcquisitionEnd = Add-KettlePerfDisplayCheckpoint -Phase 'end'
[void](Stop-KettlePerfDisplayStabilityMonitor `
    -Monitor $displayStabilityMonitor)
$displayStabilityEvidence = Get-KettlePerfDisplayStabilityEvidence `
    -Monitor $displayStabilityMonitor `
    -InitialSignature $displayAcquisitionStart.signature_sha256 `
    -Checkpoints $displayCheckpoints.ToArray()
$displayStable = [bool]$displayStabilityEvidence.stable
$manifest.machine.display_topology.acquisition_end = $displayAcquisitionEnd
$manifest.machine.display_topology.stability_monitoring = (
    $displayStabilityEvidence
)
$manifest.machine.display_topology.end_signature_sha256 = (
    $displayAcquisitionEnd.signature_sha256
)
$manifest.machine.display_topology['topology_stable'] = $displayStable
$endTargetScreens = @(
    $displayAcquisitionEnd.desktop_screens |
        Where-Object {
            [StringComparer]::OrdinalIgnoreCase.Equals(
                [string]$_.device_name,
                [string]$displayAcquisitionStart.target_screen_device
            )
        }
)
$endEvidenceValid = (
    @($displayAcquisitionEnd.identity_issues).Count -eq 0 -and
    $endTargetScreens.Count -eq 1 -and
    @($displayAcquisitionEnd.target_edid_monitors).Count -eq 1
)
$manifest.machine.display_topology.release_evidence_valid = (
    $manifest.machine.display_topology.start_evidence_valid -eq $true -and
    $endEvidenceValid -and
    $displayStable
)
if (-not $displayStable -or -not $endEvidenceValid) {
    $manifest.machine.display_topology.issues = [object[]]@(
        @($manifest.machine.display_topology.issues) +
        @($displayAcquisitionEnd.identity_issues) +
        @(
            'display topology, DPI, refresh, EDID, or primary mapping changed ' +
            'during the benchmark suite'
        )
    )
}
if ($Mode -eq 'release') {
    $freshTrackedCampaign = Read-KettlePerfComparatorCampaign `
        -Path $trackedCampaignPath `
        -ExpectedCampaignRoot $trackedCampaignRoot
    $freshInstalledCampaign = Read-KettlePerfComparatorCampaign `
        -Path $comparatorCampaign.campaign_file.path `
        -ExpectedCampaignRoot $comparatorCampaignSetup.campaigns_root
    if (
        -not (Test-KettlePerfComparatorCampaignEvidence `
            -Campaign $freshTrackedCampaign `
            -Evidence $manifest.comparator_campaign) -or
        -not (Test-KettlePerfComparatorCampaignEvidence `
            -Campaign $freshInstalledCampaign `
            -Evidence $manifest.comparator_campaign)
    ) {
        throw 'Comparator campaign identity changed during benchmarking'
    }
    foreach ($lease in $comparatorCampaignLeases) {
        if (
            $lease.closed -eq $true -or
            $lease.tree_lease.closed -eq $true -or
            @($lease.files).Count -ne [int]$lease.staged_file_count -or
            @($lease.files | Where-Object {
                $null -eq $_.stream -or -not $_.stream.CanRead
            }).Count -ne 0
        ) {
            throw 'A retained comparator staged-tree lease became invalid'
        }
    }
    foreach ($terminal in @('wt', 'alacritty', 'wezterm', 'rio', 'tabby')) {
        $records = @($terminalManifest | Where-Object {
            $_.name -ceq $terminal
        })
        if (
            $records.Count -ne 1 -or
            -not (Test-KettlePerfComparatorCampaignTerminalIdentity `
                -Entry $comparatorEntries[$terminal] `
                -TerminalRecord $records[0])
        ) {
            throw "Comparator terminal identity changed during run: $terminal"
        }
        $endHash = (
            Get-FileHash -LiteralPath $records[0].executable `
                -Algorithm SHA256
        ).Hash
        if (-not [StringComparer]::OrdinalIgnoreCase.Equals(
            $endHash,
            [string]$records[0].executable_sha256
        )) {
            throw "Comparator executable changed during run: $terminal"
        }
    }
    $windowsTerminalRecord = @($terminalManifest | Where-Object {
        $_.name -ceq 'wt'
    })[0]
    if (
        $null -eq $windowsTerminalExecutableLease -or
        $null -eq $windowsTerminalExecutableLease.Stream -or
        -not $windowsTerminalExecutableLease.Stream.CanRead -or
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            [string]$windowsTerminalExecutableLease.Path,
            [string]$windowsTerminalRecord.launcher
        )
    ) {
        throw 'Windows Terminal Appx host lease became invalid during benchmarking'
    }
    $endWindowsTerminalPackage = (
        Get-KettlePerfWindowsTerminalPackageEvidence `
            -ExpectedVersion $comparatorEntries['wt'].version `
            -Executable $windowsTerminalRecord.executable
    )
    if (
        (ConvertTo-Json -InputObject $endWindowsTerminalPackage `
            -Depth 5 -Compress) -cne
        (ConvertTo-Json -InputObject $windowsTerminalRecord.installed_package `
            -Depth 5 -Compress)
    ) {
        throw 'Windows Terminal Appx identity changed during benchmarking'
    }
}
$endHarnessProvenance = Get-KettlePerfHarnessProvenance -Locks $harnessLocks
if (-not [StringComparer]::Ordinal.Equals(
    [string]$endHarnessProvenance.aggregate_sha256,
    [string]$manifest.harness_provenance.aggregate_sha256
)) {
    throw 'Performance harness provenance changed during benchmarking'
}
$manifest.harness_provenance = $endHarnessProvenance
Write-KettlePerfJsonFile `
    -Path (Join-Path $resultsDir 'benchmark-manifest.json') `
    -InputObject $manifest -Depth 10 -Root $resultsRoot
if (-not $manifest.machine.display_topology.release_evidence_valid) {
    throw 'display topology changed during benchmarking; evidence was invalidated'
}

Write-Host "=== complete - results in $resultsDir ==="
} finally {
    foreach ($configLock in $configLocks) {
        $configLock.Dispose()
    }
}
} finally {
    Close-KettlePerfExecutableLease $windowsTerminalExecutableLease
    foreach ($lease in $comparatorCampaignLeases) {
        Close-KettlePerfComparatorCampaignExecutableLease -Lease $lease
    }
    foreach ($stream in $comparatorCampaignStreams) {
        if ($null -ne $stream) {
            $stream.Dispose()
        }
    }
    if ($null -ne $wslLauncherEvidence) {
        $wslLauncherEvidence.Stream.Dispose()
    }
    if (
        $null -ne $displayStabilityMonitor -and
        -not [bool]$displayStabilityMonitor.stopped
    ) {
        [void](Stop-KettlePerfDisplayStabilityMonitor `
            -Monitor $displayStabilityMonitor)
    }
    Close-KettlePerfPersistenceRoot $resultsRoot
    Close-KettlePerfHarnessLocks -Locks $harnessLocks
}
