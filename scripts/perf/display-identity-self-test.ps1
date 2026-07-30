# GUI-free tests for fail-closed physical-display identity acquisition.
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. "$PSScriptRoot\lib-win32.ps1"

function Assert-KettlePerfDisplayIdentity {
    param(
        [Parameter(Mandatory = $true)]
        [bool]$Condition,
        [Parameter(Mandatory = $true)]
        [string]$Message
    )

    if (-not $Condition) {
        throw $Message
    }
}

function Get-KettlePerfTestEdid {
    $bytes = New-Object byte[] 128
    $header = [byte[]]@(
        0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00
    )
    [Array]::Copy($header, 0, $bytes, 0, $header.Length)

    # EISA manufacturer AUS, little-endian product 25B2.
    $bytes[8] = 0x06
    $bytes[9] = 0xb3
    $bytes[10] = 0xb2
    $bytes[11] = 0x25
    $bytes[12] = 0x78
    $bytes[13] = 0x56
    $bytes[14] = 0x34
    $bytes[15] = 0x12
    $bytes[16] = 12
    $bytes[17] = 34

    $bytes[57] = 0xfc
    $name = [Text.Encoding]::ASCII.GetBytes('ASUS TEST')
    [Array]::Copy($name, 0, $bytes, 59, $name.Length)
    $bytes[75] = 0xff
    $serial = [Text.Encoding]::ASCII.GetBytes('SERIAL-1')
    [Array]::Copy($serial, 0, $bytes, 77, $serial.Length)

    $bytes[126] = 0
    $sum = 0
    for ($index = 0; $index -lt 127; $index++) {
        $sum = ($sum + [int]$bytes[$index]) -band 0xff
    }
    $bytes[127] = [byte]((256 - $sum) -band 0xff)
    return ,$bytes
}

function Get-KettlePerfTestScreen {
    param(
        [AllowEmptyString()]
        [string]$MonitorDeviceId = ''
    )

    return [pscustomobject][ordered]@{
        device_name = '\\.\DISPLAY1'
        monitor_device_id = $MonitorDeviceId
        primary = $true
        effective_dpi = [pscustomobject][ordered]@{ x = 144; y = 144 }
        scale_factor = 1.5
        refresh_hz = 60
        bounds = [pscustomobject][ordered]@{
            x = 0
            y = 0
            width = 2560
            height = 1440
        }
        working_area = [pscustomobject][ordered]@{
            x = 0
            y = 0
            width = 2560
            height = 1400
        }
        requested_client_fits = $true
    }
}

function Get-KettlePerfTestCcdPath {
    param(
        [Parameter(Mandatory = $true)]
        [string]$MonitorDevicePath,
        [int]$EdidProductCodeId = 0x25b2,
        $OutputTechnology = 10
    )

    return [pscustomobject][ordered]@{
        SourceDeviceName = '\\.\DISPLAY1'
        MonitorDevicePath = $MonitorDevicePath
        FriendlyName = 'ASUS TEST'
        AdapterLuid = '00000000:00000001'
        SourceId = [uint32]0
        TargetId = [uint32]1
        ConnectorInstance = [uint32]0
        OutputTechnology = $OutputTechnology
        EdidManufactureId = 0x06b3
        EdidProductCodeId = $EdidProductCodeId
        SourceInUse = $true
        TargetInUse = $true
        TargetAvailable = $true
        EdidIdsValid = $true
        FriendlyNameFromEdid = $true
        FriendlyNameForced = $false
    }
}

function Get-KettlePerfTestWmiConnection {
    param(
        [string]$InstanceName = 'DISPLAY\AUS25B2\4&abc&0&UID12612',
        [string]$HardwareId = 'AUS25B2',
        $OutputTechnology = 10
    )

    return [pscustomobject][ordered]@{
        instance_name = $InstanceName
        hardware_id = $HardwareId
        video_output_technology = $OutputTechnology
    }
}

function Assert-KettlePerfDisplayIdentityRejected {
    param(
        [Parameter(Mandatory = $true)]
        $Result,
        [Parameter(Mandatory = $true)]
        [string]$Description
    )

    Assert-KettlePerfDisplayIdentity (
        $Result.identity_acquisition.method -ceq 'none' -and
        $Result.identity_acquisition.resolved_screen_count -eq 0 -and
        $Result.active_physical_monitors.Count -eq 0 -and
        $Result.active_connections.Count -eq 0 -and
        $Result.desktop_screens.Count -eq 1 -and
        -not [bool]$Result.desktop_screens[0].edid_backed -and
        $Result.desktop_screens[0].edid_match_count -eq 0 -and
        $Result.issues.Count -eq 1
    ) "Display identity resolver accepted $Description"
}

$devicePath = (
    '\\?\DISPLAY#AUS25B2#4&abc&0&UID12612#' +
    '{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}'
)
$secondDevicePath = (
    '\\?\DISPLAY#AUS25B2#5&def&0&UID12613#' +
    '{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}'
)
$wrongGuidDevicePath = (
    '\\?\DISPLAY#AUS25B2#4&abc&0&UID12612#' +
    '{4d36e96e-e325-11ce-bfc1-08002be10318}'
)
$invalidDevicePath = $devicePath + '\Device Parameters'
$validEdid = Get-KettlePerfTestEdid
$screenWithoutWmiId = Get-KettlePerfTestScreen
$ccdPath = Get-KettlePerfTestCcdPath -MonitorDevicePath $devicePath
$registryEdid = @{}
$registryEdid[$devicePath] = [byte[]]$validEdid.Clone()

$signedInternal = [int]::MinValue
$unsignedInternal = [uint32]2147483648
foreach ($physicalTechnology in @(
    0,
    10,
    18,
    [double]18.0,
    $signedInternal,
    $unsignedInternal
)) {
    Assert-KettlePerfDisplayIdentity (
        Test-KettlePerfPhysicalOutputTechnology $physicalTechnology
    ) "Physical output technology was rejected: $physicalTechnology"
}
$signedCanonical = @(
    ConvertTo-KettlePerfCanonicalOutputTechnology $signedInternal
)
$unsignedCanonical = @(
    ConvertTo-KettlePerfCanonicalOutputTechnology $unsignedInternal
)
Assert-KettlePerfDisplayIdentity (
    $signedCanonical.Count -eq 1 -and
    $unsignedCanonical.Count -eq 1 -and
    [uint64]$signedCanonical[0] -eq 2147483648 -and
    [uint64]$unsignedCanonical[0] -eq 2147483648
) 'Signed and unsigned INTERNAL technology did not normalize identically'
foreach ($invalidTechnology in @(
    $null,
    '10',
    $true,
    10.5,
    [double]::NaN,
    [double]::PositiveInfinity,
    -1,
    [uint32]::MaxValue,
    7,
    15,
    16,
    17,
    19,
    [pscustomobject]@{ value = 10 }
)) {
    Assert-KettlePerfDisplayIdentity (
        -not (Test-KettlePerfPhysicalOutputTechnology $invalidTechnology)
    ) "Nonphysical or malformed output technology was accepted: $invalidTechnology"
}

$pathParts = Get-KettlePerfMonitorDevicePathPart $devicePath
Assert-KettlePerfDisplayIdentity (
    $null -ne $pathParts -and
    $pathParts.hardware_id -ceq 'AUS25B2' -and
    $pathParts.instance_id -ceq '4&abc&0&UID12612' -and
    $null -eq (Get-KettlePerfMonitorDevicePathPart $wrongGuidDevicePath) -and
    $null -eq (Get-KettlePerfMonitorDevicePathPart $invalidDevicePath)
) 'Strict monitor device-interface path parsing failed'

$ccdResult = Resolve-KettlePerfDisplayIdentity `
    -DesktopScreens @($screenWithoutWmiId) `
    -WmiMonitors @() `
    -WmiConnections @() `
    -CcdPaths @($ccdPath) `
    -CcdStatus available `
    -RegistryEdidByPath $registryEdid
Assert-KettlePerfDisplayIdentity (
    $ccdResult.issues.Count -eq 0 -and
    $ccdResult.identity_acquisition.schema -ceq (
        'kettle-display-identity-acquisition-v2'
    ) -and
    $ccdResult.identity_acquisition.resolver -ceq (
        'wmi-monitor-id-with-ccd-registry-fallback-v2'
    ) -and
    $ccdResult.identity_acquisition.method -ceq (
        'display-config-ccd-registry-edid-v1'
    ) -and
    $ccdResult.identity_acquisition.ccd_status -ceq 'available' -and
    $ccdResult.active_physical_monitors.Count -eq 1 -and
    $ccdResult.desktop_screens[0].edid_backed -and
    $ccdResult.desktop_screens[0].edid_match_count -eq 1 -and
    $ccdResult.desktop_screens[0].monitor_hardware_id -ceq 'AUS25B2' -and
    $ccdResult.desktop_screens[0].edid_monitor.identity_source -ceq (
        'display-config-ccd-registry-edid-v1'
    ) -and
    $ccdResult.desktop_screens[0].edid_monitor.monitor_device_path -ceq (
        $devicePath
    ) -and
    $ccdResult.desktop_screens[0].edid_monitor.registry_edid_block_count -eq 1 -and
    $ccdResult.desktop_screens[0].edid_monitor.registry_edid_sha256 -match (
        '^[0-9a-f]{64}$'
    ) -and
    $ccdResult.desktop_screens[0].connection.identity_source -ceq (
        'display-config-ccd-registry-edid-v1'
    )
) 'Valid exact-path CCD/registry EDID fallback did not resolve'

foreach ($physicalCcdTechnology in @(18, $signedInternal)) {
    $physicalCcdResult = Resolve-KettlePerfDisplayIdentity `
        -DesktopScreens @($screenWithoutWmiId) `
        -WmiMonitors @() `
        -WmiConnections @() `
        -CcdPaths @(
            Get-KettlePerfTestCcdPath `
                -MonitorDevicePath $devicePath `
                -OutputTechnology $physicalCcdTechnology
        ) `
        -CcdStatus available `
        -RegistryEdidByPath $registryEdid
    Assert-KettlePerfDisplayIdentity (
        $physicalCcdResult.issues.Count -eq 0 -and
        $physicalCcdResult.identity_acquisition.method -ceq (
            'display-config-ccd-registry-edid-v1'
        ) -and
        $physicalCcdResult.active_connections.Count -eq 1
    ) "Physical CCD output technology did not resolve: $physicalCcdTechnology"
}

$wmiMonitor = [pscustomobject][ordered]@{
    instance_name = 'DISPLAY\AUS25B2\4&abc&0&UID12612'
    hardware_id = 'AUS25B2'
    manufacturer_code = 'AUS'
    product_code = '25B2'
    friendly_name = 'ASUS TEST'
    serial_number = 'SERIAL-1'
    manufacture_week = 12
    manufacture_year = 2024
}
$wmiConnection = Get-KettlePerfTestWmiConnection
$screenWithWmiId = Get-KettlePerfTestScreen `
    -MonitorDeviceId 'MONITOR\AUS25B2\4&abc&0&UID12612'
$wmiResult = Resolve-KettlePerfDisplayIdentity `
    -DesktopScreens @($screenWithWmiId) `
    -WmiMonitors @($wmiMonitor) `
    -WmiConnections @($wmiConnection) `
    -CcdPaths @($ccdPath) `
    -CcdStatus available `
    -RegistryEdidByPath $registryEdid
Assert-KettlePerfDisplayIdentity (
    $wmiResult.issues.Count -eq 0 -and
    $wmiResult.identity_acquisition.method -ceq 'wmi-monitor-id-v1' -and
    $wmiResult.desktop_screens[0].edid_monitor.identity_source -ceq (
        'wmi-monitor-id-v1'
    ) -and
    $wmiResult.desktop_screens[0].edid_monitor.monitor_device_path -ceq (
        $devicePath
    ) -and
    $wmiResult.desktop_screens[0].connection.identity_source -ceq (
        'wmi-monitor-connection-v1'
    )
) 'Unique WMI identity did not take precedence over corroborating CCD evidence'

$wmiOnlyResult = Resolve-KettlePerfDisplayIdentity `
    -DesktopScreens @($screenWithWmiId) `
    -WmiMonitors @($wmiMonitor) `
    -WmiConnections @($wmiConnection) `
    -CcdPaths @() `
    -CcdStatus unavailable
Assert-KettlePerfDisplayIdentity (
    $wmiOnlyResult.issues.Count -eq 0 -and
    $wmiOnlyResult.identity_acquisition.method -ceq 'wmi-monitor-id-v1' -and
    $wmiOnlyResult.desktop_screens[0].edid_monitor.identity_source -ceq (
        'wmi-monitor-id-v1'
    ) -and
    $wmiOnlyResult.desktop_screens[0].connection.identity_source -ceq (
        'wmi-monitor-connection-v1'
    ) -and
    $null -eq (
        $wmiOnlyResult.desktop_screens[0].edid_monitor.monitor_device_path
    )
) 'Exact physical WMI monitor/connection pair did not resolve independently'

foreach ($physicalWmiTechnology in @(18, $unsignedInternal)) {
    $physicalWmiResult = Resolve-KettlePerfDisplayIdentity `
        -DesktopScreens @($screenWithWmiId) `
        -WmiMonitors @($wmiMonitor) `
        -WmiConnections @(
            Get-KettlePerfTestWmiConnection `
                -OutputTechnology $physicalWmiTechnology
        ) `
        -CcdPaths @() `
        -CcdStatus unavailable
    Assert-KettlePerfDisplayIdentity (
        $physicalWmiResult.issues.Count -eq 0 -and
        $physicalWmiResult.identity_acquisition.method -ceq (
            'wmi-monitor-id-v1'
        ) -and
        $physicalWmiResult.active_connections.Count -eq 1
    ) "Physical WMI output technology did not resolve: $physicalWmiTechnology"
}

$ccdPairFallback = Resolve-KettlePerfDisplayIdentity `
    -DesktopScreens @($screenWithWmiId) `
    -WmiMonitors @($wmiMonitor) `
    -WmiConnections @() `
    -CcdPaths @($ccdPath) `
    -CcdStatus available `
    -RegistryEdidByPath $registryEdid
Assert-KettlePerfDisplayIdentity (
    $ccdPairFallback.issues.Count -eq 0 -and
    $ccdPairFallback.identity_acquisition.method -ceq (
        'display-config-ccd-registry-edid-v1'
    ) -and
    $ccdPairFallback.desktop_screens[0].edid_monitor.identity_source -ceq (
        'display-config-ccd-registry-edid-v1'
    ) -and
    $ccdPairFallback.desktop_screens[0].connection.identity_source -ceq (
        'display-config-ccd-registry-edid-v1'
    )
) 'Missing WMI connection did not fall back to one strict CCD/CCD pair'

$missingConnectionResult = Resolve-KettlePerfDisplayIdentity `
    -DesktopScreens @($screenWithWmiId) `
    -WmiMonitors @($wmiMonitor) `
    -WmiConnections @() `
    -CcdPaths @() `
    -CcdStatus unavailable
Assert-KettlePerfDisplayIdentityRejected `
    -Result $missingConnectionResult `
    -Description 'a WMI monitor without physical connection evidence'

$mismatchedInstanceResult = Resolve-KettlePerfDisplayIdentity `
    -DesktopScreens @($screenWithWmiId) `
    -WmiMonitors @($wmiMonitor) `
    -WmiConnections @(
        Get-KettlePerfTestWmiConnection `
            -InstanceName 'DISPLAY\AUS25B2\different-instance'
    ) `
    -CcdPaths @($ccdPath) `
    -CcdStatus available `
    -RegistryEdidByPath $registryEdid
Assert-KettlePerfDisplayIdentityRejected `
    -Result $mismatchedInstanceResult `
    -Description 'a hardware-only WMI connection match masked by CCD evidence'

$unrelatedWmiCcdFallback = Resolve-KettlePerfDisplayIdentity `
    -DesktopScreens @($screenWithWmiId) `
    -WmiMonitors @($wmiMonitor) `
    -WmiConnections @(
        Get-KettlePerfTestWmiConnection `
            -InstanceName 'DISPLAY\OTHER99\unrelated-instance' `
            -HardwareId 'OTHER99'
    ) `
    -CcdPaths @($ccdPath) `
    -CcdStatus available `
    -RegistryEdidByPath $registryEdid
Assert-KettlePerfDisplayIdentity (
    $unrelatedWmiCcdFallback.issues.Count -eq 0 -and
    $unrelatedWmiCcdFallback.identity_acquisition.method -ceq (
        'display-config-ccd-registry-edid-v1'
    ) -and
    $unrelatedWmiCcdFallback.desktop_screens[0].connection.
        identity_source -ceq 'display-config-ccd-registry-edid-v1'
) 'Unrelated WMI connection evidence incorrectly blocked strict CCD fallback'

$mismatchedHardwareResult = Resolve-KettlePerfDisplayIdentity `
    -DesktopScreens @($screenWithWmiId) `
    -WmiMonitors @($wmiMonitor) `
    -WmiConnections @(
        Get-KettlePerfTestWmiConnection -HardwareId 'OTHER99'
    ) `
    -CcdPaths @($ccdPath) `
    -CcdStatus available `
    -RegistryEdidByPath $registryEdid
Assert-KettlePerfDisplayIdentityRejected `
    -Result $mismatchedHardwareResult `
    -Description 'same-instance WMI connection with mismatched hardware'

$duplicateConnectionResult = Resolve-KettlePerfDisplayIdentity `
    -DesktopScreens @($screenWithWmiId) `
    -WmiMonitors @($wmiMonitor) `
    -WmiConnections @(
        (Get-KettlePerfTestWmiConnection),
        (Get-KettlePerfTestWmiConnection)
    ) `
    -CcdPaths @($ccdPath) `
    -CcdStatus available `
    -RegistryEdidByPath $registryEdid
Assert-KettlePerfDisplayIdentityRejected `
    -Result $duplicateConnectionResult `
    -Description 'duplicate same-instance WMI connection evidence'

foreach ($syntheticTechnology in @(15, 16, 17)) {
    $syntheticWmiResult = Resolve-KettlePerfDisplayIdentity `
        -DesktopScreens @($screenWithWmiId) `
        -WmiMonitors @($wmiMonitor) `
        -WmiConnections @(
            Get-KettlePerfTestWmiConnection `
                -OutputTechnology $syntheticTechnology
        ) `
        -CcdPaths @($ccdPath) `
        -CcdStatus available `
        -RegistryEdidByPath $registryEdid
    Assert-KettlePerfDisplayIdentityRejected `
        -Result $syntheticWmiResult `
        -Description (
            "WMI synthetic output technology $syntheticTechnology " +
            'masked by physical CCD evidence'
        )

    $syntheticCcdResult = Resolve-KettlePerfDisplayIdentity `
        -DesktopScreens @($screenWithoutWmiId) `
        -WmiMonitors @() `
        -WmiConnections @() `
        -CcdPaths @(
            Get-KettlePerfTestCcdPath `
                -MonitorDevicePath $devicePath `
                -OutputTechnology $syntheticTechnology
        ) `
        -CcdStatus available `
        -RegistryEdidByPath $registryEdid
    Assert-KettlePerfDisplayIdentityRejected `
        -Result $syntheticCcdResult `
        -Description "CCD synthetic output technology $syntheticTechnology"
}

foreach ($malformedTechnology in @('10', $true, 10.5)) {
    $malformedWmiResult = Resolve-KettlePerfDisplayIdentity `
        -DesktopScreens @($screenWithWmiId) `
        -WmiMonitors @($wmiMonitor) `
        -WmiConnections @(
            Get-KettlePerfTestWmiConnection `
                -OutputTechnology $malformedTechnology
        ) `
        -CcdPaths @($ccdPath) `
        -CcdStatus available `
        -RegistryEdidByPath $registryEdid
    Assert-KettlePerfDisplayIdentityRejected `
        -Result $malformedWmiResult `
        -Description (
            "malformed WMI technology $malformedTechnology " +
            'masked by physical CCD evidence'
        )

    $malformedCcdResult = Resolve-KettlePerfDisplayIdentity `
        -DesktopScreens @($screenWithoutWmiId) `
        -WmiMonitors @() `
        -WmiConnections @() `
        -CcdPaths @(
            Get-KettlePerfTestCcdPath `
                -MonitorDevicePath $devicePath `
                -OutputTechnology $malformedTechnology
        ) `
        -CcdStatus available `
        -RegistryEdidByPath $registryEdid
    Assert-KettlePerfDisplayIdentityRejected `
        -Result $malformedCcdResult `
        -Description "malformed CCD technology $malformedTechnology"
}

$ambiguousRegistryEdid = @{}
$ambiguousRegistryEdid[$devicePath] = [byte[]]$validEdid.Clone()
$ambiguousRegistryEdid[$secondDevicePath] = [byte[]]$validEdid.Clone()
$ambiguousResult = Resolve-KettlePerfDisplayIdentity `
    -DesktopScreens @($screenWithoutWmiId) `
    -WmiMonitors @() `
    -WmiConnections @() `
    -CcdPaths @(
        $ccdPath,
        (Get-KettlePerfTestCcdPath -MonitorDevicePath $secondDevicePath)
    ) `
    -CcdStatus available `
    -RegistryEdidByPath $ambiguousRegistryEdid
Assert-KettlePerfDisplayIdentityRejected `
    -Result $ambiguousResult `
    -Description 'multiple active CCD paths for one desktop source'

$checksumTamperedEdid = [byte[]]$validEdid.Clone()
$checksumTamperedEdid[20] = [byte](
    [int]$checksumTamperedEdid[20] -bxor 1
)
$checksumRegistryEdid = @{}
$checksumRegistryEdid[$devicePath] = $checksumTamperedEdid
$checksumResult = Resolve-KettlePerfDisplayIdentity `
    -DesktopScreens @($screenWithoutWmiId) `
    -WmiMonitors @() `
    -WmiConnections @() `
    -CcdPaths @($ccdPath) `
    -CcdStatus available `
    -RegistryEdidByPath $checksumRegistryEdid
Assert-KettlePerfDisplayIdentityRejected `
    -Result $checksumResult `
    -Description 'a checksum-tampered exact-path EDID'

$productResult = Resolve-KettlePerfDisplayIdentity `
    -DesktopScreens @($screenWithoutWmiId) `
    -WmiMonitors @() `
    -WmiConnections @() `
    -CcdPaths @(
        Get-KettlePerfTestCcdPath `
            -MonitorDevicePath $devicePath `
            -EdidProductCodeId 0x1234
    ) `
    -CcdStatus available `
    -RegistryEdidByPath $registryEdid
Assert-KettlePerfDisplayIdentityRejected `
    -Result $productResult `
    -Description 'CCD product identifiers that disagree with exact-path EDID'

$invalidPathRegistryEdid = @{}
$invalidPathRegistryEdid[$invalidDevicePath] = [byte[]]$validEdid.Clone()
$pathResult = Resolve-KettlePerfDisplayIdentity `
    -DesktopScreens @($screenWithoutWmiId) `
    -WmiMonitors @() `
    -WmiConnections @() `
    -CcdPaths @(
        Get-KettlePerfTestCcdPath -MonitorDevicePath $invalidDevicePath
    ) `
    -CcdStatus available `
    -RegistryEdidByPath $invalidPathRegistryEdid
Assert-KettlePerfDisplayIdentityRejected `
    -Result $pathResult `
    -Description 'a suffix-injected monitor device-interface path'

$wrongGuidRegistryEdid = @{}
$wrongGuidRegistryEdid[$wrongGuidDevicePath] = [byte[]]$validEdid.Clone()
$wrongGuidResult = Resolve-KettlePerfDisplayIdentity `
    -DesktopScreens @($screenWithoutWmiId) `
    -WmiMonitors @() `
    -WmiConnections @() `
    -CcdPaths @(
        Get-KettlePerfTestCcdPath -MonitorDevicePath $wrongGuidDevicePath
    ) `
    -CcdStatus available `
    -RegistryEdidByPath $wrongGuidRegistryEdid
Assert-KettlePerfDisplayIdentityRejected `
    -Result $wrongGuidResult `
    -Description 'a non-monitor device-interface class GUID'

Write-Output 'display-identity self-test: PASS'
