# Measure one live Kettle window recovering after moves between two physical
# monitors. This is a recovery-to-capturable-client probe: PrintWindow and the
# control API are software observations, not present-to-photon measurements.
param(
    [string]$ResultsDir = '',
    [string]$KettleExe = '',
    [string]$ConfigPath = '',
    [string]$ExtraConfig = '',
    [ValidatePattern('^[0-9a-fA-F-]{36}$')]
    [string]$RunId = '',
    [ValidateRange(1, 1000)]
    [int]$Samples = 10,
    [ValidateRange(320, 16384)]
    [int]$WindowW = 1280,
    [ValidateRange(240, 16384)]
    [int]$WindowH = 800,
    [ValidateRange(100, 60000)]
    [int]$RecoveryTimeoutMs = 10000,
    [ValidateRange(2, 10)]
    [int]$GeometryStableChecks = 2,
    [ValidateRange(5, 1000)]
    [int]$PollMs = 15
)
$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\lib-win32.ps1"
. "$PSScriptRoot\terminal-specs.ps1"
. "$PSScriptRoot\json-io.ps1"

if (-not $ResultsDir) {
    $ResultsDir = Join-Path $PSScriptRoot '..\..\target\perf-results'
}
if (-not $KettleExe) {
    $KettleExe = Join-Path $PSScriptRoot '..\..\target\release\kettle.exe'
}
if (-not $RunId) {
    $RunId = [Guid]::NewGuid().ToString('D')
}
New-Item -ItemType Directory -Force -Path $ResultsDir | Out-Null
$ResultsDir = (Resolve-Path -LiteralPath $ResultsDir).Path
$resultsRoot = Open-KettlePerfPersistenceRoot -Directory $ResultsDir
$outputPath = Join-Path $ResultsDir 'monitor-transition.json'
$nonClientWidthAllowance = 64
$nonClientHeightAllowance = 96

function Get-MonitorTransitionTopology {
    param(
        [int]$ClientWidth,
        [int]$ClientHeight
    )

    $identity = Get-KettlePerfDisplayIdentityTopology `
        -ClientWidth $ClientWidth -ClientHeight $ClientHeight `
        -NonClientWidthAllowance $nonClientWidthAllowance `
        -NonClientHeightAllowance $nonClientHeightAllowance
    return [pscustomobject][ordered]@{
        identity_acquisition = $identity.identity_acquisition
        identity_issues = [object[]]$identity.issues
        timestamp = (Get-Date).ToString('o')
        requested_client = [pscustomobject][ordered]@{
            width = $ClientWidth
            height = $ClientHeight
            non_client_allowance = [pscustomobject][ordered]@{
                width = $nonClientWidthAllowance
                height = $nonClientHeightAllowance
            }
        }
        desktop_screens = [object[]]$identity.desktop_screens
        active_physical_monitors = (
            [object[]]$identity.active_physical_monitors
        )
        active_connections = [object[]]$identity.active_connections
    }
}

function Get-MonitorTransitionTopologySignature {
    param($Topology)

    return (
        [ordered]@{
            identity_acquisition = $Topology.identity_acquisition
            identity_issues = $Topology.identity_issues
            requested_client = $Topology.requested_client
            desktop_screens = $Topology.desktop_screens
            active_physical_monitors = $Topology.active_physical_monitors
            active_connections = $Topology.active_connections
        } | ConvertTo-Json -Compress -Depth 12
    )
}

function Get-MonitorTransitionEndpoint {
    param($Screen)

    return [pscustomobject][ordered]@{
        device_name = [string]$Screen.device_name
        monitor_device_id = [string]$Screen.monitor_device_id
        monitor_hardware_id = [string]$Screen.monitor_hardware_id
        edid_instance_name = if ($null -ne $Screen.edid_monitor) {
            [string]$Screen.edid_monitor.instance_name
        } else {
            $null
        }
        friendly_name = if ($null -ne $Screen.edid_monitor) {
            [string]$Screen.edid_monitor.friendly_name
        } else {
            $null
        }
        serial_number = if ($null -ne $Screen.edid_monitor) {
            [string]$Screen.edid_monitor.serial_number
        } else {
            $null
        }
        effective_dpi = $Screen.effective_dpi
        scale_factor = $Screen.scale_factor
        refresh_hz = $Screen.refresh_hz
        bounds = $Screen.bounds
        working_area = $Screen.working_area
        requested_client_fits = [bool]$Screen.requested_client_fits
    }
}

function Get-MonitorTransitionOrderedScreens {
    param([object[]]$Screens)

    $ordered = [Collections.Generic.List[object]]::new()
    foreach ($screen in $Screens) {
        $insertAt = $ordered.Count
        for ($index = 0; $index -lt $ordered.Count; $index++) {
            if (
                [StringComparer]::OrdinalIgnoreCase.Compare(
                    [string]$screen.device_name,
                    [string]$ordered[$index].device_name
                ) -lt 0
            ) {
                $insertAt = $index
                break
            }
        }
        $ordered.Insert($insertAt, $screen)
    }
    return [object[]]$ordered.ToArray()
}

function Get-MonitorTransitionPairContrast {
    param(
        $First,
        $Second
    )

    $dpiDelta = [Math]::Max(
        [Math]::Abs(
            [int]$First.effective_dpi.x -
            [int]$Second.effective_dpi.x
        ),
        [Math]::Abs(
            [int]$First.effective_dpi.y -
            [int]$Second.effective_dpi.y
        )
    )
    $refreshDelta = [Math]::Abs(
        [int]$First.refresh_hz - [int]$Second.refresh_hz
    )
    $geometryDelta = 0
    foreach ($field in @('width', 'height')) {
        $geometryDelta = [Math]::Max(
            $geometryDelta,
            [Math]::Abs(
                [int]$First.bounds.$field -
                [int]$Second.bounds.$field
            )
        )
        $geometryDelta = [Math]::Max(
            $geometryDelta,
            [Math]::Abs(
                [int]$First.working_area.$field -
                [int]$Second.working_area.$field
            )
        )
    }
    $meaningfulDimensions = 0
    if ($dpiDelta -gt 0) {
        $meaningfulDimensions++
    }
    if ($refreshDelta -gt 0) {
        $meaningfulDimensions++
    }
    if ($geometryDelta -gt 0) {
        $meaningfulDimensions++
    }
    $deviceNames = [string[]]@(
        [string]$First.device_name,
        [string]$Second.device_name
    )
    return [pscustomobject][ordered]@{
        pair_key = $deviceNames -join '|'
        device_names = $deviceNames
        meaningful_dimension_count = $meaningfulDimensions
        dpi_delta = $dpiDelta
        refresh_hz_delta = $refreshDelta
        geometry_delta_pixels = $geometryDelta
    }
}

function Test-MonitorTransitionContrastBetter {
    param(
        $Candidate,
        $Current
    )

    if ($null -eq $Current) {
        return $true
    }
    foreach ($field in @(
        'meaningful_dimension_count',
        'dpi_delta',
        'refresh_hz_delta',
        'geometry_delta_pixels'
    )) {
        $candidateValue = [int]$Candidate.$field
        $currentValue = [int]$Current.$field
        if ($candidateValue -ne $currentValue) {
            return $candidateValue -gt $currentValue
        }
    }
    return (
        [StringComparer]::OrdinalIgnoreCase.Compare(
            [string]$Candidate.pair_key,
            [string]$Current.pair_key
        ) -lt 0
    )
}

function Get-MonitorTransitionSelectionPolicy {
    param([object[]]$EligibleScreens)

    $candidates = [Collections.Generic.List[object]]::new()
    $selected = $null
    for ($firstIndex = 0; $firstIndex -lt $EligibleScreens.Count; $firstIndex++) {
        for (
            $secondIndex = $firstIndex + 1;
            $secondIndex -lt $EligibleScreens.Count;
            $secondIndex++
        ) {
            $candidate = Get-MonitorTransitionPairContrast `
                $EligibleScreens[$firstIndex] $EligibleScreens[$secondIndex]
            $candidates.Add($candidate)
            if (Test-MonitorTransitionContrastBetter $candidate $selected) {
                $selected = $candidate
            }
        }
    }
    return [pscustomobject][ordered]@{
        algorithm = 'maximum-meaningful-contrast-v1'
        eligible_screen_order = 'device-name-ordinal-ignore-case'
        ranking = [string[]]@(
            'meaningful_dimension_count:descending',
            'dpi_delta:descending',
            'refresh_hz_delta:descending',
            'geometry_delta_pixels:descending',
            'pair_key:ordinal-ignore-case-ascending'
        )
        geometry_definition = (
            'maximum absolute width/height delta across bounds and ' +
            'working-area sizes; desktop coordinates are excluded'
        )
        eligible_screen_device_names = [string[]]@(
            $EligibleScreens |
                ForEach-Object { [string]$_.device_name }
        )
        candidate_pair_count = $candidates.Count
        candidate_pairs = [object[]]$candidates.ToArray()
        selected_pair_key = if ($null -ne $selected) {
            [string]$selected.pair_key
        } else {
            $null
        }
        selected_device_names = if ($null -ne $selected) {
            [string[]]$selected.device_names
        } else {
            [string[]]@()
        }
        selected_contrast = $selected
    }
}

function Get-MonitorTransitionPercentile {
    param(
        [double[]]$Sorted,
        [double]$Percentile
    )

    if ($Sorted.Count -eq 0) {
        return $null
    }
    if ([Math]::Abs($Percentile - 0.5) -le [double]::Epsilon) {
        return Get-KettlePerfMedian $Sorted
    }
    $index = [Math]::Min(
        $Sorted.Count - 1,
        [Math]::Max(
            0,
            [int][Math]::Ceiling($Sorted.Count * $Percentile) - 1
        )
    )
    return [double]$Sorted[$index]
}

function Invoke-MonitorTransitionCtlJson {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Exe,
        [Parameter(Mandatory = $true)]
        [int]$TargetProcessId,
        [Parameter(Mandatory = $true)]
        [string]$Method,
        [string]$Text = ''
    )

    $arguments = @(
        'ctl', '--pid', [string]$TargetProcessId, $Method
    )
    if ($Text) {
        $arguments += @('--text', $Text)
    }
    $arguments += '--raw'
    try {
        $capture = Invoke-KettlePerfBoundedProcess -FilePath $Exe `
            -ArgumentList $arguments -TimeoutMs 2000 `
            -MaxStdoutBytes 1048576 -MaxStderrBytes 65536
    } catch {
        return $null
    }
    if ($capture.ExitCode -ne 0) {
        return $null
    }
    try {
        return ConvertFrom-KettlePerfBoundedJson `
            -Json $capture.StandardOutput -MaximumDepth 32 -MaximumTokens 10000
    } catch {
        return $null
    }
}

function Invoke-MonitorTransitionRecovery {
    param(
        [Parameter(Mandatory = $true)]
        [IntPtr]$Hwnd,
        [Parameter(Mandatory = $true)]
        [int]$ExpectedPid,
        [Parameter(Mandatory = $true)]
        [string]$CliExe,
        [Parameter(Mandatory = $true)]
        $TargetScreen,
        [Parameter(Mandatory = $true)]
        [int]$ClientWidth,
        [Parameter(Mandatory = $true)]
        [int]$ClientHeight,
        [Parameter(Mandatory = $true)]
        [bool]$ExpectedMenuOpen,
        [Parameter(Mandatory = $true)]
        [int]$TimeoutMs,
        [Parameter(Mandatory = $true)]
        [int]$RequiredStableChecks,
        [Parameter(Mandatory = $true)]
        [int]$PollingMs
    )

    $timer = [Diagnostics.Stopwatch]::StartNew()
    $setWindowError = $null
    try {
        Set-WindowSize `
            $Hwnd $ClientWidth $ClientHeight $TargetScreen.device_name
    } catch {
        $setWindowError = $_.Exception.Message
    }
    if ($setWindowError) {
        $timer.Stop()
        return [pscustomobject][ordered]@{
            status = 'miss'
            reason = "Set-WindowSize failed: $setWindowError"
            recovery_to_capturable_client_ms = $null
            actual_device_name = [KettlePerf.Native]::MonitorDeviceForWindow(
                $Hwnd
            )
            target_effective_dpi = $null
            target_refresh_hz = $null
            capture = $null
            surface = $null
            context_menu = $null
            geometry_checks = 0
        }
    }

    $stableChecks = 0
    $geometryChecks = 0
    $lastReason = 'the target state was not observed'
    $lastActualDevice = $null
    $lastDpi = $null
    $lastRefresh = $null
    $lastCapture = $null
    $lastSurface = $null
    $lastContextMenu = $null
    while ($timer.ElapsedMilliseconds -lt $TimeoutMs) {
        $ownerPid = Get-WindowPid $Hwnd
        if (-not $ownerPid -or $ownerPid -ne $ExpectedPid) {
            $lastReason = (
                "window owner changed from expected pid $ExpectedPid to " +
                "$ownerPid"
            )
            break
        }

        $lastActualDevice = [KettlePerf.Native]::MonitorDeviceForWindow($Hwnd)
        if (-not [StringComparer]::OrdinalIgnoreCase.Equals(
            $lastActualDevice,
            [string]$TargetScreen.device_name
        )) {
            $stableChecks = 0
            $lastReason = (
                "window is on $lastActualDevice instead of " +
                [string]$TargetScreen.device_name
            )
            Start-Sleep -Milliseconds $PollingMs
            continue
        }

        $centerX = (
            [int]$TargetScreen.bounds.x +
            [int]([double]$TargetScreen.bounds.width / 2.0)
        )
        $centerY = (
            [int]$TargetScreen.bounds.y +
            [int]([double]$TargetScreen.bounds.height / 2.0)
        )
        $lastDpi = [KettlePerf.Native]::EffectiveDpiAt($centerX, $centerY)
        $lastRefresh = [KettlePerf.Native]::CurrentRefreshRate(
            [string]$TargetScreen.device_name
        )
        $dpiMatches = (
            $null -ne $lastDpi -and
            [int]$lastDpi[0] -eq [int]$TargetScreen.effective_dpi.x -and
            [int]$lastDpi[1] -eq [int]$TargetScreen.effective_dpi.y
        )
        $refreshMatches = (
            $lastRefresh -eq [int]$TargetScreen.refresh_hz -and
            $lastRefresh -gt 0
        )
        if (-not $dpiMatches -or -not $refreshMatches) {
            $stableChecks = 0
            $lastReason = 'target DPI or refresh changed during recovery'
            Start-Sleep -Milliseconds $PollingMs
            continue
        }

        $geometry = Invoke-MonitorTransitionCtlJson `
            -Exe $CliExe -TargetProcessId $ExpectedPid -Method ui_geometry
        $geometryChecks++
        if ($null -eq $geometry) {
            $stableChecks = 0
            $lastReason = 'ui_geometry did not return a valid response'
            Start-Sleep -Milliseconds $PollingMs
            continue
        }
        $surfaceMatches = (
            [int]$geometry.surface.width -eq $ClientWidth -and
            [int]$geometry.surface.height -eq $ClientHeight
        )
        $menuOpen = [bool]$geometry.modals.context_menu
        $menuObjectPresent = $null -ne $geometry.context_menu
        $menuMatches = if ($ExpectedMenuOpen) {
            $menuOpen -and $menuObjectPresent
        } else {
            -not $menuOpen -and -not $menuObjectPresent
        }
        $captureWidth = 0
        $captureHeight = 0
        $capture = [KettlePerf.Native]::CaptureWindow(
            $Hwnd,
            [ref]$captureWidth,
            [ref]$captureHeight
        )
        $captureMatches = (
            $null -ne $capture -and
            $captureWidth -eq $ClientWidth -and
            $captureHeight -eq $ClientHeight
        )
        $lastCapture = [pscustomobject][ordered]@{
            width = $captureWidth
            height = $captureHeight
            bytes = if ($null -ne $capture) { $capture.Length } else { 0 }
        }
        $lastSurface = [pscustomobject][ordered]@{
            width = [int]$geometry.surface.width
            height = [int]$geometry.surface.height
        }
        $lastContextMenu = if ($menuObjectPresent) {
            [pscustomobject][ordered]@{
                open = $menuOpen
                rect = $geometry.context_menu.rect
                rows = @($geometry.context_menu.rows).Count
            }
        } else {
            [pscustomobject][ordered]@{
                open = $false
                rect = $null
                rows = 0
            }
        }

        if ($surfaceMatches -and $menuMatches -and $captureMatches) {
            $stableChecks++
            if ($stableChecks -ge $RequiredStableChecks) {
                $timer.Stop()
                return [pscustomobject][ordered]@{
                    status = 'ok'
                    reason = $null
                    recovery_to_capturable_client_ms = [Math]::Round(
                        $timer.Elapsed.TotalMilliseconds,
                        3
                    )
                    actual_device_name = $lastActualDevice
                    target_effective_dpi = [pscustomobject][ordered]@{
                        x = [int]$lastDpi[0]
                        y = [int]$lastDpi[1]
                    }
                    target_refresh_hz = $lastRefresh
                    capture = $lastCapture
                    surface = $lastSurface
                    context_menu = $lastContextMenu
                    geometry_checks = $geometryChecks
                }
            }
        } else {
            $stableChecks = 0
            $lastReason = (
                "surface=$surfaceMatches menu=$menuMatches " +
                "capture=$captureMatches"
            )
        }
        Start-Sleep -Milliseconds $PollingMs
    }
    $timer.Stop()
    return [pscustomobject][ordered]@{
        status = 'miss'
        reason = "recovery timed out or aborted: $lastReason"
        recovery_to_capturable_client_ms = $null
        actual_device_name = $lastActualDevice
        target_effective_dpi = if ($null -ne $lastDpi) {
            [pscustomobject][ordered]@{
                x = [int]$lastDpi[0]
                y = [int]$lastDpi[1]
            }
        } else {
            $null
        }
        target_refresh_hz = $lastRefresh
        capture = $lastCapture
        surface = $lastSurface
        context_menu = $lastContextMenu
        geometry_checks = $geometryChecks
    }
}

$topologyStart = Get-MonitorTransitionTopology $WindowW $WindowH
$eligibleScreens = @(
    Get-MonitorTransitionOrderedScreens @(
        $topologyStart.desktop_screens |
            Where-Object {
                $_.edid_backed -and
                $_.requested_client_fits -and
                $null -ne $_.effective_dpi -and
                [int]$_.refresh_hz -gt 0
            }
    )
)
$selectionPolicy = Get-MonitorTransitionSelectionPolicy $eligibleScreens

if ($eligibleScreens.Count -lt 2) {
    $reasons = [Collections.Generic.List[string]]::new()
    foreach ($identityIssue in @($topologyStart.identity_issues)) {
        [void]$reasons.Add([string]$identityIssue)
    }
    $desktopCount = @($topologyStart.desktop_screens).Count
    $edidCount = @(
        $topologyStart.desktop_screens | Where-Object { $_.edid_backed }
    ).Count
    $fitCount = @(
        $topologyStart.desktop_screens | Where-Object {
            $_.edid_backed -and $_.requested_client_fits
        }
    ).Count
    if ($desktopCount -lt 2) {
        [void]$reasons.Add(
            "requires at least two active Windows.Forms screens; found $desktopCount"
        )
    }
    if ($edidCount -lt 2) {
        [void]$reasons.Add(
            "requires two screens mapped one-to-one to active EDID monitors; found $edidCount"
        )
    }
    if ($fitCount -lt 2) {
        [void]$reasons.Add(
            "requires two EDID-backed working areas that fit a ${WindowW}x" +
            "${WindowH} client plus the non-client allowance; found $fitCount"
        )
    }
    if ($eligibleScreens.Count -lt $fitCount) {
        [void]$reasons.Add(
            'one or more otherwise eligible screens has no stable effective ' +
            'DPI or refresh-rate mapping'
        )
    }
    if ($reasons.Count -eq 0) {
        [void]$reasons.Add(
            "requires two eligible physical screens; found $($eligibleScreens.Count)"
        )
    }
    $topologyEnd = Get-MonitorTransitionTopology $WindowW $WindowH
    $skippedTopologyStable = [StringComparer]::Ordinal.Equals(
        (Get-MonitorTransitionTopologySignature $topologyStart),
        (Get-MonitorTransitionTopologySignature $topologyEnd)
    )
    $skipped = [ordered]@{
        schema_version = 2
        run_id = $RunId
        timestamp = (Get-Date).ToString('o')
        status = 'skipped'
        release_evidence_valid = $false
        reason = $reasons -join '; '
        metric_name = 'recovery_to_capturable_client_ms'
        requested = [ordered]@{
            samples_per_state = $Samples
            states = @('menu_closed', 'context_menu_open')
            window_pixels = [ordered]@{
                width = $WindowW
                height = $WindowH
            }
            recovery_timeout_ms = $RecoveryTimeoutMs
            geometry_stable_checks = $GeometryStableChecks
            poll_ms = $PollMs
        }
        topology_start = $topologyStart
        topology_end = $topologyEnd
        topology_stable = $skippedTopologyStable
        selection_policy = $selectionPolicy
        observations = @()
        misses = 0
    }
    Write-KettlePerfJsonFile -Path $outputPath -InputObject $skipped `
        -Depth 16 -Root $resultsRoot
    Close-KettlePerfPersistenceRoot $resultsRoot
    Write-Warning (
        "monitor-transition skipped: $($skipped.reason); evidence in $outputPath"
    )
    return
}

$selectedScreens = @(
    foreach ($deviceName in $selectionPolicy.selected_device_names) {
        @(
            $eligibleScreens |
                Where-Object {
                    [StringComparer]::OrdinalIgnoreCase.Equals(
                        [string]$_.device_name,
                        [string]$deviceName
                    )
                }
        )[0]
    }
)
$artifactDir = Join-Path $ResultsDir ("monitor-transition-$RunId")
New-Item -ItemType Directory -Force -Path $artifactDir | Out-Null
$effectiveConfig = $null
$configMode = $null
$spec = $null
$version = $null
$gpuInfo = $null
$binaryEvidence = $null
$fatalError = $null
$observations = [Collections.Generic.List[object]]::new()
$beforeWindows = $null
$preexistingPids = $null
$proc = $null
$hwnd = [IntPtr]::Zero
$winPid = 0
$guiLease = $null
$cliLease = $null
$oldCursor = [KettlePerf.Native+POINT]::new()
$oldCursorCaptured = [KettlePerf.Native]::GetCursorPos([ref]$oldCursor)
$stdoutLog = Join-Path $artifactDir 'kettle.stdout.log'
$stderrLog = Join-Path $artifactDir 'kettle.stderr.log'

try {
    $spec = Resolve-KettlePerfTerminal -Name kettle -KettleExe $KettleExe
    if (-not $spec.Available) {
        throw "Kettle executable not found: $KettleExe"
    }
    if (-not $spec.HasReliableCli) {
        throw (
            'monitor-transition requires kettle-console.exe or kettle.com ' +
            'beside kettle.exe for reliable control responses'
        )
    }
    $guiLease = Open-KettlePerfExecutableLease `
        -Executable $spec.BenchmarkExe `
        -ExpectedSha256 $spec.BenchmarkExeSha256
    $cliLease = Open-KettlePerfExecutableLease `
        -Executable $spec.CliExe -ExpectedSha256 $spec.CliExeSha256
    if ($ConfigPath) {
        if (-not (Test-Path -LiteralPath $ConfigPath -PathType Leaf)) {
            throw "Config file not found: $ConfigPath"
        }
        $effectiveConfig = (Resolve-Path -LiteralPath $ConfigPath).Path
        $configMode = 'provided'
    } else {
        $effectiveConfig = Join-Path $artifactDir 'config'
        $configLines = @(
            'agent-server = full'
            'tab-bar = always'
            'status-bar = off'
            'restore-session = false'
            'update-check = false'
            'record = off'
            'font-size = 13'
            'background = #101010'
            'foreground = #f4f4f4'
            'window-width = 100'
            'window-height = 28'
        )
        if ($ExtraConfig) {
            $configLines += $ExtraConfig
        }
        [IO.File]::WriteAllLines(
            $effectiveConfig,
            $configLines,
            [Text.UTF8Encoding]::new($false)
        )
        $configMode = 'generated-isolated'
    }

    $version = Get-KettlePerfVersion $spec
    $binaryEvidence = [ordered]@{
        executable = $spec.BenchmarkExe
        executable_sha256 = $spec.BenchmarkExeSha256
        cli_executable = $spec.CliExe
        cli_executable_sha256 = $spec.CliExeSha256
        product_version = $version
        config = $effectiveConfig
        config_mode = $configMode
        config_sha256 = (
            Get-FileHash -LiteralPath $effectiveConfig -Algorithm SHA256
        ).Hash
    }
    $gpuCapture = Invoke-KettlePerfBoundedProcess -FilePath $spec.CliExe `
        -ArgumentList @('--config', $effectiveConfig, '--gpu-info') `
        -TimeoutMs 10000 -MaxStdoutBytes 1048576 -MaxStderrBytes 1048576
    if ($gpuCapture.ExitCode -ne 0) {
        throw "Kettle --gpu-info failed with exit code $($gpuCapture.ExitCode)"
    }
    $gpuInfo = @(
        $gpuCapture.StandardOutput,
        $gpuCapture.StandardError
    ) -join "`n"

    $beforeWindows = Get-VisibleWindowSet
    $preexistingPids = Get-PidSet
    $preexistingKettle = @(
        Get-Process -Name $spec.WindowProcessNames -ErrorAction SilentlyContinue |
            Where-Object { $preexistingPids.Contains($_.Id) }
    )
    if ($preexistingKettle.Count -gt 0) {
        throw (
            'Kettle is already running; close it before the isolated ' +
            'monitor-transition benchmark'
        )
    }

    $launchArguments = @(
        '--new-process',
        '--config',
        $effectiveConfig,
        '--agent-server',
        'full'
    )
    $proc = Start-Process -FilePath $spec.Exe `
        -ArgumentList (Join-KettlePerfArguments $launchArguments) `
        -RedirectStandardOutput $stdoutLog `
        -RedirectStandardError $stderrLog `
        -PassThru
    $hwnd = Wait-NewWindow -Before $beforeWindows `
        -PreexistingPids $preexistingPids -RootPid $proc.Id `
        -ProcessNames $spec.WindowProcessNames `
        -ExpectedExecutable $spec.BenchmarkExe -TimeoutMs 30000
    if ($hwnd -eq [IntPtr]::Zero) {
        throw 'Kettle window never appeared'
    }
    $winPid = Get-WindowPid $hwnd
    if (-not $winPid -or $preexistingPids.Contains($winPid)) {
        throw 'Kettle window is not attributable to the isolated launch'
    }

    $setup = Invoke-MonitorTransitionRecovery `
        -Hwnd $hwnd -ExpectedPid $winPid -CliExe $spec.CliExe `
        -TargetScreen $selectedScreens[0] `
        -ClientWidth $WindowW -ClientHeight $WindowH `
        -ExpectedMenuOpen $false -TimeoutMs $RecoveryTimeoutMs `
        -RequiredStableChecks $GeometryStableChecks -PollingMs $PollMs
    if ($setup.status -ne 'ok') {
        throw "initial Kettle recovery failed: $($setup.reason)"
    }

    $abortMeasurements = $false
    foreach ($state in @('menu_closed', 'context_menu_open')) {
        if ($state -eq 'context_menu_open') {
            $geometry = Invoke-MonitorTransitionCtlJson `
                -Exe $spec.CliExe -TargetProcessId $winPid -Method ui_geometry
            if ($null -eq $geometry) {
                throw 'ui_geometry was unavailable before opening the menu'
            }
            $cursorX = [int](
                [double]$geometry.content.x +
                [Math]::Min(100.0, [double]$geometry.content.width / 2.0)
            )
            $cursorY = [int](
                [double]$geometry.content.y +
                [Math]::Min(100.0, [double]$geometry.content.height / 2.0)
            )
            if (-not [KettlePerf.Native]::SetClientCursorPos(
                $hwnd,
                $cursorX,
                $cursorY
            )) {
                throw 'could not position the pointer inside Kettle'
            }
            [void][KettlePerf.Native]::SetForegroundWindow($hwnd)
            Start-Sleep -Milliseconds 100
            $opened = Invoke-MonitorTransitionCtlJson `
                -Exe $spec.CliExe -TargetProcessId $winPid `
                -Method perform_action -Text open_context_menu
            if ($null -eq $opened) {
                throw 'the Kettle control server could not open the context menu'
            }
            $currentDevice = [KettlePerf.Native]::MonitorDeviceForWindow($hwnd)
            $currentScreen = @(
                $selectedScreens | Where-Object {
                    [StringComparer]::OrdinalIgnoreCase.Equals(
                        [string]$_.device_name,
                        $currentDevice
                    )
                }
            ) | Select-Object -First 1
            if ($null -eq $currentScreen) {
                throw "Kettle is on unexpected screen $currentDevice"
            }
            $menuSetup = Invoke-MonitorTransitionRecovery `
                -Hwnd $hwnd -ExpectedPid $winPid -CliExe $spec.CliExe `
                -TargetScreen $currentScreen `
                -ClientWidth $WindowW -ClientHeight $WindowH `
                -ExpectedMenuOpen $true -TimeoutMs $RecoveryTimeoutMs `
                -RequiredStableChecks $GeometryStableChecks -PollingMs $PollMs
            if ($menuSetup.status -ne 'ok') {
                throw "context menu did not stabilize: $($menuSetup.reason)"
            }
        }

        for ($sample = 0; $sample -lt $Samples; $sample++) {
            $sourceDevice = [KettlePerf.Native]::MonitorDeviceForWindow($hwnd)
            $sourceIndex = -1
            for ($index = 0; $index -lt $selectedScreens.Count; $index++) {
                if ([StringComparer]::OrdinalIgnoreCase.Equals(
                    [string]$selectedScreens[$index].device_name,
                    $sourceDevice
                )) {
                    $sourceIndex = $index
                    break
                }
            }
            if ($sourceIndex -lt 0) {
                $observations.Add([pscustomobject][ordered]@{
                    started_utc = (Get-Date).ToString('o')
                    state = $state
                    sample = $sample
                    direction = "$sourceDevice->unknown"
                    source = $null
                    target = $null
                    status = 'miss'
                    miss_reason = (
                        "window source $sourceDevice is outside the selected screens"
                    )
                    recovery_to_capturable_client_ms = $null
                })
                $abortMeasurements = $true
                break
            }
            $targetIndex = if ($sourceIndex -eq 0) { 1 } else { 0 }
            $sourceScreen = $selectedScreens[$sourceIndex]
            $targetScreen = $selectedScreens[$targetIndex]
            $observationStartedUtc = (Get-Date).ToString('o')
            $recovery = Invoke-MonitorTransitionRecovery `
                -Hwnd $hwnd -ExpectedPid $winPid -CliExe $spec.CliExe `
                -TargetScreen $targetScreen `
                -ClientWidth $WindowW -ClientHeight $WindowH `
                -ExpectedMenuOpen ($state -eq 'context_menu_open') `
                -TimeoutMs $RecoveryTimeoutMs `
                -RequiredStableChecks $GeometryStableChecks -PollingMs $PollMs
            $observations.Add([pscustomobject][ordered]@{
                started_utc = $observationStartedUtc
                state = $state
                sample = $sample
                direction = (
                    [string]$sourceScreen.device_name + '->' +
                    [string]$targetScreen.device_name
                )
                source = Get-MonitorTransitionEndpoint $sourceScreen
                target = Get-MonitorTransitionEndpoint $targetScreen
                status = $recovery.status
                miss_reason = $recovery.reason
                recovery_to_capturable_client_ms = (
                    $recovery.recovery_to_capturable_client_ms
                )
                actual_target_device_name = $recovery.actual_device_name
                target_effective_dpi_observed = (
                    $recovery.target_effective_dpi
                )
                target_refresh_hz_observed = $recovery.target_refresh_hz
                capture = $recovery.capture
                ui_geometry_surface = $recovery.surface
                context_menu = $recovery.context_menu
                ui_geometry_checks = $recovery.geometry_checks
            })
            if ($recovery.status -ne 'ok') {
                $abortMeasurements = $true
                break
            }
            Start-Sleep -Milliseconds 100
        }
        if ($abortMeasurements) {
            break
        }
    }
} catch {
    $fatalError = $_.Exception.Message
} finally {
    if ($oldCursorCaptured) {
        [void][KettlePerf.Native]::SetCursorPos($oldCursor.X, $oldCursor.Y)
    }
    if (
        $hwnd -ne [IntPtr]::Zero -and
        $winPid -and
        $null -ne $preexistingPids
    ) {
        [void](Close-SpawnedTerminal -Hwnd $hwnd `
            -ExpectedPid $winPid -PreexistingPids $preexistingPids)
    }
    try {
        if ($null -ne $proc -and -not $proc.HasExited) {
            Stop-Process -Id $proc.Id -Force
        }
    } catch {
        Write-Verbose (
            "monitor-transition launcher cleanup raced process exit: " +
            $_.Exception.Message
        )
    }
    Close-KettlePerfExecutableLease $cliLease
    Close-KettlePerfExecutableLease $guiLease
}

$topologyEnd = Get-MonitorTransitionTopology $WindowW $WindowH
$topologyStable = [StringComparer]::Ordinal.Equals(
    (Get-MonitorTransitionTopologySignature $topologyStart),
    (Get-MonitorTransitionTopologySignature $topologyEnd)
)
$misses = @($observations | Where-Object { $_.status -ne 'ok' }).Count
$successful = @($observations | Where-Object { $_.status -eq 'ok' })
$allValues = @(
    $successful |
        ForEach-Object {
            [double]$_.recovery_to_capturable_client_ms
        } |
        Sort-Object
)
$stateSummaries = [ordered]@{}
foreach ($state in @('menu_closed', 'context_menu_open')) {
    $stateValues = @(
        $successful |
            Where-Object { $_.state -eq $state } |
            ForEach-Object {
                [double]$_.recovery_to_capturable_client_ms
            } |
            Sort-Object
    )
    $stateSummaries[$state] = [ordered]@{
        requested_samples = $Samples
        samples = $stateValues.Count
        misses = @(
            $observations | Where-Object {
                $_.state -eq $state -and $_.status -ne 'ok'
            }
        ).Count
        recovery_to_capturable_client_ms_all = $stateValues
        recovery_to_capturable_client_ms_median = (
            Get-MonitorTransitionPercentile $stateValues 0.50
        )
        recovery_to_capturable_client_ms_p95 = (
            Get-MonitorTransitionPercentile $stateValues 0.95
        )
        recovery_to_capturable_client_ms_max = if ($stateValues.Count) {
            ($stateValues | Measure-Object -Maximum).Maximum
        } else {
            $null
        }
    }
}
$releaseValid = (
    -not $fatalError -and
    $topologyStable -and
    $misses -eq 0 -and
    $successful.Count -eq ($Samples * 2)
)
$result = [ordered]@{
    schema_version = 2
    run_id = $RunId
    timestamp = (Get-Date).ToString('o')
    status = if ($releaseValid) { 'passed' } else { 'failed' }
    release_evidence_valid = $releaseValid
    reason = if ($fatalError) {
        $fatalError
    } elseif (-not $topologyStable) {
        'display topology, DPI, refresh, EDID, or primary mapping changed during the probe'
    } elseif ($misses -gt 0) {
        "$misses monitor transition(s) missed the recovery contract"
    } elseif ($successful.Count -ne ($Samples * 2)) {
        "collected $($successful.Count) of $($Samples * 2) requested transitions"
    } else {
        $null
    }
    metric_name = 'recovery_to_capturable_client_ms'
    metric_definition = (
        'Set-WindowSize start through correct-monitor mapping, exact-size ' +
        'PrintWindow client capture, target DPI/refresh verification, and ' +
        'stable exact ui_geometry surface; not present-to-photon'
    )
    requested = [ordered]@{
        samples_per_state = $Samples
        states = @('menu_closed', 'context_menu_open')
        window_pixels = [ordered]@{
            width = $WindowW
            height = $WindowH
        }
        recovery_timeout_ms = $RecoveryTimeoutMs
        geometry_stable_checks = $GeometryStableChecks
        poll_ms = $PollMs
    }
    selected_screens = @(
        $selectedScreens |
            ForEach-Object { Get-MonitorTransitionEndpoint $_ }
    )
    selection_policy = $selectionPolicy
    binary = $binaryEvidence
    gpu_info = $gpuInfo
    artifacts = [ordered]@{
        directory = $artifactDir
        stdout = $stdoutLog
        stderr = $stderrLog
    }
    topology_start = $topologyStart
    topology_end = $topologyEnd
    topology_stable = $topologyStable
    observations = $observations
    requested_samples = $Samples * 2
    samples = $successful.Count
    misses = $misses
    recovery_to_capturable_client_ms_all = $allValues
    recovery_to_capturable_client_ms_median = (
        Get-MonitorTransitionPercentile $allValues 0.50
    )
    recovery_to_capturable_client_ms_p95 = (
        Get-MonitorTransitionPercentile $allValues 0.95
    )
    recovery_to_capturable_client_ms_max = if ($allValues.Count) {
        ($allValues | Measure-Object -Maximum).Maximum
    } else {
        $null
    }
    states = $stateSummaries
}
Write-KettlePerfJsonFile -Path $outputPath -InputObject $result `
    -Depth 20 -Root $resultsRoot
Close-KettlePerfPersistenceRoot $resultsRoot
if (-not $releaseValid) {
    throw "monitor-transition evidence is invalid: $($result.reason)"
}
Write-Host (
    "monitor transition: median={0:N2} ms p95={1:N2} ms samples={2}; evidence in {3}" -f
    $result.recovery_to_capturable_client_ms_median,
    $result.recovery_to_capturable_client_ms_p95,
    $result.samples,
    $outputPath
)
