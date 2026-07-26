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
        [int]$EdidProductCodeId = 0x25b2
    )

    return [pscustomobject][ordered]@{
        SourceDeviceName = '\\.\DISPLAY1'
        MonitorDevicePath = $MonitorDevicePath
        FriendlyName = 'ASUS TEST'
        AdapterLuid = '00000000:00000001'
        SourceId = [uint32]0
        TargetId = [uint32]1
        ConnectorInstance = [uint32]0
        OutputTechnology = 10
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
        'kettle-display-identity-acquisition-v1'
    ) -and
    $ccdResult.identity_acquisition.resolver -ceq (
        'wmi-monitor-id-with-ccd-registry-fallback-v1'
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
$wmiConnection = [pscustomobject][ordered]@{
    instance_name = 'DISPLAY\AUS25B2\4&abc&0&UID12612'
    hardware_id = 'AUS25B2'
    video_output_technology = 10
}
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
