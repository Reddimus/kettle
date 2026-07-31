#requires -Version 5.1
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

# Windows PowerShell cannot load PowerShell 7's .NET-Core build of the modules
# whose names it shares. Launching this script from a pwsh 7 session is the
# normal case -- `just` shells out to powershell.exe, and pwsh is the default
# shell on developer machines -- and that inherits a PSModulePath with
# PowerShell 7's module roots ahead of the system one. Autoloading
# Microsoft.PowerShell.Security for Get-Acl then resolves to the Core build and
# fails with CouldNotAutoloadMatchingModule, before any product logic runs.
#
# Keep only roots this edition can actually load, and guarantee the system root
# is present. Assigning $env:PSModulePath also fixes it for child processes.
if ($PSVersionTable.PSEdition -ne 'Core') {
    $systemModules = Join-Path $env:SystemRoot 'system32\WindowsPowerShell\v1.0\Modules'
    # `*\PowerShell\*` excludes PowerShell 7's user and shared roots without
    # excluding `...\WindowsPowerShell\...`, where no separator precedes
    # "PowerShell"; `*microsoft.powershell_*` excludes its MSIX package root.
    $loadable = @(
        $env:PSModulePath -split ';' |
            Where-Object { $_ } |
            Where-Object { $_ -notlike '*\PowerShell\*' -and $_ -notlike '*microsoft.powershell_*' }
    )
    if ($loadable -notcontains $systemModules) {
        $loadable = @($systemModules) + $loadable
    }
    $env:PSModulePath = ($loadable -join ';')
}

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Set-Location $repo

# The hardened installer intentionally rejects ancestors writable by another
# local principal. RUNNER_TEMP and %TEMP% can carry runner/sandbox group Modify
# ACEs, so put the security smoke below the current user's normal protected
# LocalAppData chain instead of weakening the production check for test hosts.
$tempRoot = Join-Path $env:LOCALAPPDATA 'kettle-windows-installer-smoke-root'
[void][System.IO.Directory]::CreateDirectory($tempRoot)
$portableRoot = Join-Path $tempRoot "kettle-windows-install-smoke"
$prefix = Join-Path $portableRoot 'kettle'
$integrationRoot = Join-Path $tempRoot "kettle-windows-default-install-smoke"
$startMenuDir = Join-Path $integrationRoot 'Start Menu\Programs'
$shortcutPath = Join-Path $startMenuDir 'kettle.lnk'
$profilePath = Join-Path $integrationRoot 'WindowsPowerShell\profile.ps1'
$testUninstallKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\kettle-installer-smoke-$PID"
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $portableRoot
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $integrationRoot
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $testUninstallKey

function Assert-PathPresent {
    param([string] $Path, [string] $Label)
    if (-not (Test-Path -LiteralPath $Path)) {
        throw "missing ${Label}: $Path"
    }
}

function Assert-PathAbsent {
    param([string] $Path, [string] $Label)
    if (Test-Path -LiteralPath $Path) {
        throw "${Label} survived uninstall: $Path"
    }
}

function Assert-Equal {
    param([string] $Actual, [string] $Expected, [string] $Label)
    if ($Actual -ne $Expected) {
        throw "${Label} mismatch: expected '$Expected', got '$Actual'"
    }
}

function Assert-KettleProtectedAcl {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Path,
        [Parameter(Mandatory = $true)]
        [bool] $Directory
    )

    $acl = Get-Acl -LiteralPath $Path
    if (-not $acl.AreAccessRulesProtected) {
        throw "Kettle ACL inherits from its parent: $Path"
    }
    $currentSid = [Security.Principal.WindowsIdentity]::GetCurrent().User
    $ownerSid = $acl.GetOwner(
        [Security.Principal.SecurityIdentifier]
    ).Value
    if ($ownerSid -ne $currentSid.Value) {
        throw "Kettle ACL has the wrong owner '$ownerSid': $Path"
    }

    $expected = @(
        $currentSid.Value,
        'S-1-5-18',
        'S-1-5-32-544'
    )
    $rules = @(
        $acl.GetAccessRules(
            $true,
            $true,
            [Security.Principal.SecurityIdentifier]
        )
    )
    if ($rules.Count -ne $expected.Count) {
        throw "Kettle ACL has $($rules.Count) rules instead of three: $Path"
    }
    foreach ($rule in $rules) {
        if (
            $rule.AccessControlType -ne
                [Security.AccessControl.AccessControlType]::Allow -or
            $rule.FileSystemRights -ne
                [Security.AccessControl.FileSystemRights]::FullControl -or
            $expected -notcontains $rule.IdentityReference.Value
        ) {
            throw "Kettle ACL has an unexpected rule '$rule': $Path"
        }
        $expected = @(
            $expected | Where-Object {
                $_ -ne $rule.IdentityReference.Value
            }
        )
        if ($Directory) {
            $requiredInheritance = (
                [Security.AccessControl.InheritanceFlags]::ContainerInherit -bor
                [Security.AccessControl.InheritanceFlags]::ObjectInherit
            )
            if (
                $rule.InheritanceFlags -ne $requiredInheritance -or
                $rule.PropagationFlags -ne
                    [Security.AccessControl.PropagationFlags]::None
            ) {
                throw "Kettle directory ACL has unexpected inheritance: $Path"
            }
        } elseif (
            $rule.InheritanceFlags -ne
                [Security.AccessControl.InheritanceFlags]::None -or
            $rule.PropagationFlags -ne
                [Security.AccessControl.PropagationFlags]::None
        ) {
            throw "Kettle file ACL unexpectedly propagates: $Path"
        }
    }
    if ($expected.Count -ne 0) {
        throw "Kettle ACL is missing a required trustee: $Path"
    }
}

function Assert-KettleProtectedTree {
    param([Parameter(Mandatory = $true)][string] $Path)

    Assert-KettleProtectedAcl -Path $Path -Directory $true
    foreach ($item in Get-ChildItem -LiteralPath $Path -Force -Recurse) {
        Assert-KettleProtectedAcl -Path $item.FullName `
            -Directory ([bool]$item.PSIsContainer)
    }
}

function ConvertTo-KettleComparableDaclSddl {
    param([string] $Sddl)
    # Windows clears the informational SE_DACL_AUTO_INHERITED bit when the
    # same inherited ACE set is assigned explicitly to a replacement file.
    # Compare owner/group, protection, and every ACE while ignoring only that
    # non-enforcement control bit.
    return ($Sddl -replace 'D:PAI', 'D:P' -replace 'D:AI', 'D:')
}

function Assert-InstallerRejected {
    param(
        [Parameter(Mandatory = $true)]
        [object[]] $Arguments,
        [Parameter(Mandatory = $true)]
        [string] $MessagePattern,
        [Parameter(Mandatory = $true)]
        [string] $Label
    )

    $rejected = $false
    $invokeParameters = @{}
    for ($index = 0; $index -lt $Arguments.Count; $index++) {
        switch ([string]$Arguments[$index]) {
            '-Uninstall' {
                $invokeParameters.Uninstall = $true
            }
            '-Prefix' {
                $index++
                $invokeParameters.Prefix = [string]$Arguments[$index]
            }
            '-IntegrationTestRoot' {
                $index++
                $invokeParameters.IntegrationTestRoot =
                    [string]$Arguments[$index]
            }
            '-MigrateLegacyPermissions' {
                $invokeParameters.MigrateLegacyPermissions = $true
            }
            default {
                throw "unsupported installer-test argument: $($Arguments[$index])"
            }
        }
    }
    try {
        & (Join-Path $repo 'scripts\install.ps1') @invokeParameters |
            Out-Null
    } catch {
        if ($_.Exception.Message -notlike $MessagePattern) {
            throw "${Label} failed for the wrong reason: $($_.Exception.Message)"
        }
        $rejected = $true
    }
    if (-not $rejected) {
        throw "${Label} was not rejected"
    }
}

$profileMatrixRoot = Join-Path $tempRoot (
    'kettle-installer-profile-matrix-' + [guid]::NewGuid().ToString('N')
)
try {
    $atomicProfilePath = Join-Path $profileMatrixRoot `
        'atomic-replacement\profile.ps1'
    [void][System.IO.Directory]::CreateDirectory(
        (Split-Path $atomicProfilePath -Parent)
    )
    $atomicProfileBytes = (
        New-Object System.Text.UTF8Encoding($false, $true)
    ).GetBytes("# atomic profile sentinel`r`n")
    [System.IO.File]::WriteAllBytes(
        $atomicProfilePath,
        $atomicProfileBytes
    )
    $env:KETTLE_INSTALLER_TEST_PROFILE_ONLY = $atomicProfilePath
    $env:KETTLE_INSTALLER_TEST_PROFILE_FAIL_BEFORE_REPLACE = '1'
    $profileFaultObserved = $false
    try {
        & (Join-Path $repo 'scripts\install.ps1') `
            -IntegrationTestRoot $profileMatrixRoot | Out-Null
    } catch {
        $profileFaultObserved = (
            $_.Exception.Message -like
                '*Injected profile publication failure before atomic replacement*'
        )
    } finally {
        Remove-Item Env:\KETTLE_INSTALLER_TEST_PROFILE_FAIL_BEFORE_REPLACE `
            -ErrorAction SilentlyContinue
        Remove-Item Env:\KETTLE_INSTALLER_TEST_PROFILE_ONLY `
            -ErrorAction SilentlyContinue
    }
    if (-not $profileFaultObserved) {
        throw 'the profile pre-replacement fault checkpoint was not observed'
    }
    Assert-Equal (
        [Convert]::ToBase64String(
            [System.IO.File]::ReadAllBytes($atomicProfilePath)
        )
    ) (
        [Convert]::ToBase64String($atomicProfileBytes)
    ) 'PowerShell profile pre-replacement preservation'
    $profilePublicationArtifacts = @(
        Get-ChildItem -LiteralPath (
            Split-Path $atomicProfilePath -Parent
        ) -Force |
            Where-Object {
                $_.Name -like '.kettle-install-retired-*' -or
                $_.Name -like '.kettle-install-tmp-*'
            }
    )
    if ($profilePublicationArtifacts.Count -ne 0) {
        throw 'profile failure left a retired or temporary publication artifact'
    }
    Write-Output 'windows-installer check: profile atomic fault boundary OK'

    $encodingCases = @(
        [pscustomobject]@{
            Name = 'utf8'
            Encoding = (New-Object System.Text.UTF8Encoding($false, $true))
        },
        [pscustomobject]@{
            Name = 'utf8-bom'
            Encoding = (New-Object System.Text.UTF8Encoding($true, $true))
        },
        [pscustomobject]@{
            Name = 'utf16-le'
            Encoding = (New-Object System.Text.UnicodeEncoding($false, $true, $true))
        },
        [pscustomobject]@{
            Name = 'utf16-be'
            Encoding = (New-Object System.Text.UnicodeEncoding($true, $true, $true))
        }
    )
    $newlineCases = @(
        [pscustomobject]@{ Name = 'crlf'; Value = "`r`n" },
        [pscustomobject]@{ Name = 'lf'; Value = "`n" },
        [pscustomobject]@{ Name = 'cr'; Value = "`r" }
    )
    foreach ($encodingCase in $encodingCases) {
        foreach ($newlineCase in $newlineCases) {
            foreach ($trailing in @($false, $true)) {
                $caseName = (
                    $encodingCase.Name + '-' +
                    $newlineCase.Name + '-' +
                    $(if ($trailing) { 'trailing' } else { 'unterminated' })
                )
                $casePath = Join-Path $profileMatrixRoot (
                    $caseName + '\profile.ps1'
                )
                [void][System.IO.Directory]::CreateDirectory(
                    (Split-Path $casePath -Parent)
                )
                $originalText = (
                    '# first user line' +
                    $newlineCase.Value +
                    '# final user line' +
                    $(if ($trailing) { $newlineCase.Value } else { '' })
                )
                [System.IO.File]::WriteAllText(
                    $casePath,
                    $originalText,
                    $encodingCase.Encoding
                )
                $before = [System.IO.File]::ReadAllBytes($casePath)
                $env:KETTLE_INSTALLER_TEST_PROFILE_ONLY = $casePath
                Remove-Item Env:\KETTLE_INSTALLER_TEST_PROFILE_REMOVE `
                    -ErrorAction SilentlyContinue
                & (Join-Path $repo 'scripts\install.ps1') `
                    -IntegrationTestRoot $profileMatrixRoot | Out-Null
                $env:KETTLE_INSTALLER_TEST_PROFILE_REMOVE = '1'
                & (Join-Path $repo 'scripts\install.ps1') `
                    -IntegrationTestRoot $profileMatrixRoot | Out-Null
                Assert-Equal (
                    [Convert]::ToBase64String(
                        [System.IO.File]::ReadAllBytes($casePath)
                    )
                ) (
                    [Convert]::ToBase64String($before)
                ) "PowerShell profile round trip $caseName"
            }
        }
    }
    Write-Output 'windows-installer check: profile encoding/newline matrix OK'
} finally {
    Remove-Item Env:\KETTLE_INSTALLER_TEST_PROFILE_ONLY `
        -ErrorAction SilentlyContinue
    Remove-Item Env:\KETTLE_INSTALLER_TEST_PROFILE_REMOVE `
        -ErrorAction SilentlyContinue
    Remove-Item Env:\KETTLE_INSTALLER_TEST_PROFILE_FAIL_BEFORE_REPLACE `
        -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $profileMatrixRoot) {
        [System.IO.Directory]::Delete($profileMatrixRoot, $true)
    }
}

# Windows PowerShell's CodeDOM compiler can emit a tiny console fixture without
# adding a repository artifact. Exercise both denial paths of the fixed-buffer
# version probe there; PowerShell 7 still covers its normal-output path below.
if ($PSVersionTable.PSEdition -eq 'Desktop') {
    $versionProbeRoot = Join-Path $tempRoot (
        'kettle-installer-version-probe-' +
        [guid]::NewGuid().ToString('N')
    )
    [void][System.IO.Directory]::CreateDirectory($versionProbeRoot)
    $versionProbeExe = Join-Path $versionProbeRoot 'probe-fixture.exe'
    $versionProbeSource = @'
using System;
using System.Threading;
public static class KettleInstallerProbeFixture
{
    public static int Main(string[] args)
    {
        if (Environment.GetEnvironmentVariable(
            "KETTLE_INSTALLER_PROBE_FIXTURE") == "overflow")
        {
            Console.Out.Write(new string('x', 100000));
        }
        else
        {
            Thread.Sleep(5000);
        }
        return 0;
    }
}
'@
    try {
        Add-Type -TypeDefinition $versionProbeSource `
            -OutputAssembly $versionProbeExe -OutputType ConsoleApplication
        $env:KETTLE_INSTALLER_PROBE_FIXTURE = 'overflow'
        $probeTimer = [System.Diagnostics.Stopwatch]::StartNew()
        $overflowRejected = $false
        try {
            [void][KettleInstaller.NativeFileSystemV1]::ProbeExecutableVersion(
                $versionProbeExe,
                4096,
                15000
            )
        } catch {
            $overflowRejected = (
                $_.Exception.Message -like '*output limit*'
            )
        }
        if (
            -not $overflowRejected -or
            $probeTimer.ElapsedMilliseconds -ge 5000
        ) {
            throw 'The version probe did not reject bounded output promptly.'
        }

        $env:KETTLE_INSTALLER_PROBE_FIXTURE = 'timeout'
        $timeoutRejected = $false
        try {
            [void][KettleInstaller.NativeFileSystemV1]::ProbeExecutableVersion(
                $versionProbeExe,
                4096,
                100
            )
        } catch {
            $timeoutRejected = (
                $_.Exception.Message -like '*time limit*'
            )
        }
        if (-not $timeoutRejected) {
            throw 'The version probe did not enforce its time limit.'
        }
        Write-Output 'windows-installer check: bounded version probe OK'
    } finally {
        Remove-Item Env:\KETTLE_INSTALLER_PROBE_FIXTURE `
            -ErrorAction SilentlyContinue
        if (Test-Path -LiteralPath $versionProbeRoot) {
            [System.IO.Directory]::Delete($versionProbeRoot, $true)
        }
    }
}

# Every supplied component is validated before normalization. Win32 device
# aliases, ADS syntax, wildcard/invalid characters, control characters,
# trailing aliases, and traversal spellings must all fail before touching disk.
$unsafePrefixes = @(
    (Join-Path $tempRoot 'installer-path-CON\CON\kettle'),
    (Join-Path $tempRoot 'installer-path-CON-space\CON .txt\kettle'),
    (Join-Path $tempRoot 'installer-path-COM1\COM1.txt\kettle'),
    (Join-Path $tempRoot (
        'installer-path-COM-superscript\COM' + [char]0x00B9 + '\kettle'
    )),
    (Join-Path $tempRoot 'installer-path-ads\parent:stream\kettle'),
    (Join-Path $tempRoot 'installer-path-wild\bad*\kettle'),
    (Join-Path $tempRoot 'installer-path-invalid\bad<name\kettle'),
    (Join-Path $tempRoot ('installer-path-control\bad' + [char]1 + '\kettle')),
    (Join-Path $tempRoot ('installer-path-c1\bad' + [char]0x0085 + '\kettle')),
    (Join-Path $tempRoot 'installer-path-trailing\outer.\kettle'),
    ((Join-Path $tempRoot 'installer-path-dot\outer') + '\..\kettle')
)
foreach ($unsafePrefix in $unsafePrefixes) {
    Assert-InstallerRejected `
        -Arguments @(
            '-Uninstall',
            '-Prefix',
            $unsafePrefix,
            '-IntegrationTestRoot',
            $integrationRoot
        ) `
        -MessagePattern '*unsafe Win32 path component*' `
        -Label "unsafe prefix $unsafePrefix"
}

# A junction in the prefix chain must be rejected even when its textual path is
# otherwise valid and ends in the required dedicated `kettle` leaf.
$chainTarget = Join-Path $tempRoot 'kettle-installer-chain-target'
$chainLink = Join-Path $tempRoot 'kettle-installer-chain-link'
Remove-Item -LiteralPath $chainTarget -Recurse -Force `
    -ErrorAction SilentlyContinue
if (Test-Path -LiteralPath $chainLink) {
    [KettleInstaller.NativeFileSystemV1]::RemoveEmptyDirectory($chainLink)
}
try {
    New-Item -ItemType Directory -Force -Path $chainTarget | Out-Null
    New-Item -ItemType Junction -Path $chainLink -Target $chainTarget |
        Out-Null
    Assert-InstallerRejected `
        -Arguments @(
            '-Uninstall',
            '-Prefix',
            (Join-Path $chainLink 'kettle'),
            '-IntegrationTestRoot',
            $integrationRoot
        ) `
        -MessagePattern '*reparse point*' `
        -Label 'junction-bearing prefix chain'
} finally {
    if (Test-Path -LiteralPath $chainLink) {
        [KettleInstaller.NativeFileSystemV1]::RemoveEmptyDirectory($chainLink)
    }
    Remove-Item -LiteralPath $chainTarget -Recurse -Force `
        -ErrorAction SilentlyContinue
}

# An otherwise valid fixed-volume prefix must not be created below an ancestor
# that another local user can modify. Such a user could replace the permanent
# root after installation even if the root itself received a private DACL.
$broadAclRoot = Join-Path $tempRoot 'kettle-installer-broad-acl-parent'
$broadAclPrefix = Join-Path $broadAclRoot 'kettle'
Remove-Item -LiteralPath $broadAclRoot -Recurse -Force `
    -ErrorAction SilentlyContinue
[void][System.IO.Directory]::CreateDirectory($broadAclRoot)
$broadAcl = Get-Acl -LiteralPath $broadAclRoot
$everyoneSid = New-Object Security.Principal.SecurityIdentifier `
    -ArgumentList 'S-1-1-0'
$broadAclRule = New-Object Security.AccessControl.FileSystemAccessRule `
    -ArgumentList @(
        $everyoneSid,
        [Security.AccessControl.FileSystemRights]::Modify,
        (
            [Security.AccessControl.InheritanceFlags]::ContainerInherit -bor
            [Security.AccessControl.InheritanceFlags]::ObjectInherit
        ),
        [Security.AccessControl.PropagationFlags]::None,
        [Security.AccessControl.AccessControlType]::Allow
    )
$broadAcl.AddAccessRule($broadAclRule)
Set-Acl -LiteralPath $broadAclRoot -AclObject $broadAcl
Assert-InstallerRejected `
    -Arguments @(
        '-Prefix',
        $broadAclPrefix,
        '-IntegrationTestRoot',
        $integrationRoot
    ) `
    -MessagePattern '*grants untrusted replacement access*' `
    -Label 'broad-ACL install ancestor'
Assert-PathAbsent $broadAclPrefix 'broad-ACL install prefix'
Remove-Item -LiteralPath $broadAclRoot -Recurse -Force
Write-Output 'windows-installer check: broad-ACL ancestor rejection OK'

# A portable install owns only its prefix. Seed isolated default-install state
# and verify the portable uninstaller cannot remove it.
New-Item -ItemType Directory -Force -Path $startMenuDir | Out-Null
$ws = New-Object -ComObject WScript.Shell
$sentinel = $ws.CreateShortcut($shortcutPath)
$sentinel.TargetPath = (Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe')
$sentinel.Arguments = '-NoProfile -File "default-install-sentinel.ps1"'
$sentinel.WorkingDirectory = $tempRoot
$sentinel.Save()
New-Item -Path $testUninstallKey -Force | Out-Null
Set-ItemProperty -Path $testUninstallKey -Name 'Sentinel' -Value 'default-install'
$profileSentinel = @'
# >>> kettle shell-integration (managed by install.ps1)
# default-install sentinel
# <<< kettle shell-integration (managed by install.ps1)
'@
New-Item -ItemType Directory -Force -Path (Split-Path $profilePath -Parent) | Out-Null
Set-Content -Path $profilePath -Value $profileSentinel -NoNewline
$userPathBefore = [Environment]::GetEnvironmentVariable('Path', 'User')

# Installing into a nonempty but unowned directory named `kettle` must fail
# before overwriting or adopting unrelated content.
$unownedRoot = Join-Path $tempRoot 'kettle-windows-unowned-prefix'
$unownedPrefix = Join-Path $unownedRoot 'kettle'
$unownedSentinel = Join-Path $unownedPrefix 'must-survive.txt'
Remove-Item -LiteralPath $unownedRoot -Recurse -Force `
    -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $unownedPrefix | Out-Null
Set-Content -LiteralPath $unownedSentinel -Value 'sentinel' -NoNewline
Assert-InstallerRejected `
    -Arguments @(
        '-Prefix',
        $unownedPrefix,
        '-IntegrationTestRoot',
        $integrationRoot
    ) `
    -MessagePattern '*does not have Kettle''s protected owner/DACL*' `
    -Label 'unowned nonempty install prefix'
Assert-PathPresent $unownedSentinel 'unowned-prefix sentinel'
Remove-Item -LiteralPath $unownedRoot -Recurse -Force

# A predictable sibling transaction name is recovery evidence only when the
# initiating Windows identity owns a protected, exact trustee ACL. Reject a
# preseeded broad directory before reading its attacker-controlled journal.
$preseededTransaction = $prefix + '.install-transaction'
[void][System.IO.Directory]::CreateDirectory($preseededTransaction)
$preseededSentinel = Join-Path $preseededTransaction 'must-survive.txt'
Set-Content -LiteralPath $preseededSentinel -Value 'sentinel' -NoNewline
$preseededAcl = Get-Acl -LiteralPath $preseededTransaction
$preseededAcl.SetAccessRuleProtection($true, $true)
Set-Acl -LiteralPath $preseededTransaction -AclObject $preseededAcl
Assert-InstallerRejected `
    -Arguments @(
        '-Prefix',
        $prefix,
        '-IntegrationTestRoot',
        $integrationRoot
    ) `
    -MessagePattern '*private installer transaction directory*' `
    -Label 'preseeded broad-ACL package transaction'
Assert-PathPresent $preseededSentinel 'preseeded transaction sentinel'
Remove-Item -LiteralPath $preseededTransaction -Recurse -Force
if (Test-Path -LiteralPath $prefix) {
    Remove-Item -LiteralPath $prefix -Recurse -Force
}

# A reparse point at the same sibling name must likewise fail closed without
# traversing or modifying its target.
$preseededTarget = Join-Path $tempRoot `
    'kettle-installer-transaction-junction-target'
[void][System.IO.Directory]::CreateDirectory($preseededTarget)
$preseededTargetSentinel = Join-Path $preseededTarget 'must-survive.txt'
Set-Content -LiteralPath $preseededTargetSentinel -Value 'sentinel' -NoNewline
New-Item -ItemType Junction -Path $preseededTransaction `
    -Target $preseededTarget | Out-Null
Assert-InstallerRejected `
    -Arguments @(
        '-Prefix',
        $prefix,
        '-IntegrationTestRoot',
        $integrationRoot
    ) `
    -MessagePattern '*not a real directory*' `
    -Label 'preseeded transaction junction'
Assert-PathPresent $preseededTargetSentinel `
    'preseeded transaction junction target sentinel'
[KettleInstaller.NativeFileSystemV1]::RemoveEmptyDirectory(
    $preseededTransaction
)
Remove-Item -LiteralPath $preseededTarget -Recurse -Force
if (Test-Path -LiteralPath $prefix) {
    Remove-Item -LiteralPath $prefix -Recurse -Force
}
Write-Output 'windows-installer check: private transaction root preseed rejection OK'

& (Join-Path $repo 'scripts\install.ps1') -Prefix $prefix -IntegrationTestRoot $integrationRoot |
    Tee-Object -FilePath (Join-Path $tempRoot 'kettle-windows-install.out')

Assert-PathPresent (Join-Path $prefix 'kettle.exe') 'kettle.exe'
Assert-PathPresent (Join-Path $prefix 'kettle.com') 'kettle.com console launcher'
Assert-PathPresent (Join-Path $prefix 'install.ps1') 'saved install.ps1'
Assert-PathPresent (Join-Path $prefix '.kettle-install-prefix') 'prefix marker'
Assert-PathPresent (Join-Path $prefix '.kettle-install.json') 'self-update ownership marker'
$marker = Get-Content -LiteralPath (Join-Path $prefix '.kettle-install.json') -Raw | ConvertFrom-Json
Assert-Equal $marker.channel 'local-dev' 'repo install marker channel'
& (Join-Path $prefix 'install.ps1') -IntegrationTestRoot $integrationRoot |
    Out-Null
$savedHelperMarker = Get-Content -LiteralPath (
    Join-Path $prefix '.kettle-install.json'
) -Raw | ConvertFrom-Json
Assert-Equal $savedHelperMarker.channel 'local-dev' `
    'saved local-development helper marker channel'
Assert-PathPresent (Join-Path $prefix 'kettle.ico') 'icon'
Assert-PathPresent (Join-Path $prefix 'shell-integration\kettle.ps1') 'PowerShell shell integration'
Assert-KettleProtectedTree -Path $prefix

# Legacy installers inherited their root and file ACLs. A normal rerun must
# fail closed, while an explicit migration from this trusted source checkout
# must repair both the root and every bounded managed leaf before publication.
$legacyInstallerPath = Join-Path $prefix 'install.ps1'
& icacls.exe $prefix /grant '*S-1-1-0:(OI)(CI)M' | Out-Null
if ($LASTEXITCODE -ne 0) {
    throw "could not prepare legacy writable root ACL (icacls $LASTEXITCODE)"
}
& icacls.exe $legacyInstallerPath /grant '*S-1-1-0:M' | Out-Null
if ($LASTEXITCODE -ne 0) {
    throw "could not prepare legacy writable file ACL (icacls $LASTEXITCODE)"
}
Assert-InstallerRejected `
    -Arguments @(
        '-Prefix',
        $prefix,
        '-IntegrationTestRoot',
        $integrationRoot
    ) `
    -MessagePattern '*does not have Kettle''s protected owner/DACL*' `
    -Label 'legacy writable install without migration opt-in'
& (Join-Path $repo 'scripts\install.ps1') -Prefix $prefix `
    -IntegrationTestRoot $integrationRoot -MigrateLegacyPermissions |
    Out-Null
Assert-KettleProtectedTree -Path $prefix
Write-Output 'windows-installer check: permanent ACL migration OK'

# Rust atomic writes use a distinct PID-bearing staging grammar. Remove only
# exact leaves whose owner process is provably dead; retain live or linked
# lookalikes and fail before managed payload mutation.
$deadProcess = Start-Process -FilePath $env:ComSpec `
    -ArgumentList @('/c', 'exit', '0') -PassThru -Wait
$deadProcessId = $deadProcess.Id
$deadProcess.Dispose()
$deadRustTemp = Join-Path $prefix (
    ".README.md.tmp.$deadProcessId.1.0"
)
Set-Content -LiteralPath $deadRustTemp -Value 'dead process temp' -NoNewline
& (Join-Path $repo 'scripts\install.ps1') -Prefix $prefix `
    -IntegrationTestRoot $integrationRoot | Out-Null
Assert-PathAbsent $deadRustTemp 'dead-process Rust atomic temporary leaf'

$maximumRustTemp = Join-Path $prefix (
    ".README.md.tmp.$deadProcessId." +
    '340282366920938463463374607431768211455.' +
    '18446744073709551615'
)
Set-Content -LiteralPath $maximumRustTemp `
    -Value 'maximum canonical Rust temp' -NoNewline
& (Join-Path $repo 'scripts\install.ps1') -Prefix $prefix `
    -IntegrationTestRoot $integrationRoot | Out-Null
Assert-PathAbsent $maximumRustTemp `
    'maximum canonical Rust atomic temporary leaf'

$overflowNanosecondsTemp = Join-Path $prefix (
    ".README.md.tmp.$deadProcessId." +
    '340282366920938463463374607431768211456.0'
)
Set-Content -LiteralPath $overflowNanosecondsTemp `
    -Value 'overflow nanoseconds sentinel' -NoNewline
Assert-InstallerRejected `
    -Arguments @(
        '-Prefix',
        $prefix,
        '-IntegrationTestRoot',
        $integrationRoot
    ) `
    -MessagePattern '*noncanonical Rust atomic temporary file*' `
    -Label 'overflowing Rust atomic nanoseconds'
Assert-PathPresent $overflowNanosecondsTemp `
    'overflowing Rust atomic nanoseconds sentinel'
Remove-Item -LiteralPath $overflowNanosecondsTemp -Force

$overflowSequenceTemp = Join-Path $prefix (
    ".README.md.tmp.$deadProcessId.1.18446744073709551616"
)
Set-Content -LiteralPath $overflowSequenceTemp `
    -Value 'overflow sequence sentinel' -NoNewline
Assert-InstallerRejected `
    -Arguments @(
        '-Prefix',
        $prefix,
        '-IntegrationTestRoot',
        $integrationRoot
    ) `
    -MessagePattern '*noncanonical Rust atomic temporary file*' `
    -Label 'overflowing Rust atomic sequence'
Assert-PathPresent $overflowSequenceTemp `
    'overflowing Rust atomic sequence sentinel'
Remove-Item -LiteralPath $overflowSequenceTemp -Force

$liveRustTemp = Join-Path $prefix ".README.md.tmp.$PID.1.0"
Set-Content -LiteralPath $liveRustTemp -Value 'live process temp' -NoNewline
Assert-InstallerRejected `
    -Arguments @(
        '-Prefix',
        $prefix,
        '-IntegrationTestRoot',
        $integrationRoot
    ) `
    -MessagePattern '*still belongs to a live process*' `
    -Label 'live-process Rust atomic temporary leaf'
Assert-PathPresent $liveRustTemp 'live-process Rust temporary sentinel'
Remove-Item -LiteralPath $liveRustTemp -Force

$linkedRustTarget = Join-Path $tempRoot `
    'kettle-installer-rust-temp-hardlink-target'
Set-Content -LiteralPath $linkedRustTarget -Value 'linked sentinel' -NoNewline
$linkedRustTemp = Join-Path $prefix (
    ".README.md.tmp.$deadProcessId.2.0"
)
New-Item -ItemType HardLink -Path $linkedRustTemp `
    -Target $linkedRustTarget | Out-Null
Assert-InstallerRejected `
    -Arguments @(
        '-Prefix',
        $prefix,
        '-IntegrationTestRoot',
        $integrationRoot
    ) `
    -MessagePattern '*single-link ordinary file*' `
    -Label 'hard-linked Rust atomic temporary leaf'
Assert-Equal (Get-Content -LiteralPath $linkedRustTarget -Raw) `
    'linked sentinel' 'hard-linked Rust temporary target'
Remove-Item -LiteralPath $linkedRustTemp -Force
Remove-Item -LiteralPath $linkedRustTarget -Force

$rustJunctionTarget = Join-Path $tempRoot `
    'kettle-installer-rust-temp-junction-target'
[void][System.IO.Directory]::CreateDirectory($rustJunctionTarget)
$rustJunctionSentinel = Join-Path $rustJunctionTarget 'must-survive.txt'
Set-Content -LiteralPath $rustJunctionSentinel -Value 'sentinel' -NoNewline
$rustJunctionTemp = Join-Path $prefix (
    ".README.md.tmp.$deadProcessId.3.0"
)
New-Item -ItemType Junction -Path $rustJunctionTemp `
    -Target $rustJunctionTarget | Out-Null
Assert-InstallerRejected `
    -Arguments @(
        '-Prefix',
        $prefix,
        '-IntegrationTestRoot',
        $integrationRoot
    ) `
    -MessagePattern '*reparse point*' `
    -Label 'junction Rust atomic temporary path'
Assert-PathPresent $rustJunctionSentinel `
    'Rust atomic temporary junction target sentinel'
[KettleInstaller.NativeFileSystemV1]::RemoveEmptyDirectory(
    $rustJunctionTemp
)
Remove-Item -LiteralPath $rustJunctionTarget -Recurse -Force
Write-Output 'windows-installer check: Rust atomic temporary recovery OK'

$substDrive = $null
foreach ($candidate in @('R:', 'S:', 'T:', 'U:', 'V:')) {
    if (-not (Test-Path ($candidate + '\'))) {
        $substDrive = $candidate
        break
    }
}
if ($null -ne $substDrive) {
    $substRoot = Join-Path $tempRoot (
        'kettle-installer-subst-' + [guid]::NewGuid().ToString('N')
    )
    New-Item -ItemType Directory -Force -Path $substRoot | Out-Null
    & subst.exe $substDrive $substRoot
    if ($LASTEXITCODE -ne 0) {
        throw 'Could not create the SUBST installer-path fixture.'
    }
    try {
        $substRejected = $false
        try {
            & (Join-Path $repo 'scripts\install.ps1') `
                -Prefix ($substDrive + '\mapped\kettle') `
                -IntegrationTestRoot $integrationRoot | Out-Null
        } catch {
            $substRejected = (
                $_.Exception.Message -like '*SUBST*' -or
                $_.Exception.Message -like '*local fixed drive*'
            )
        }
        if (-not $substRejected) {
            throw 'a SUBST-backed install prefix was not rejected'
        }
        Assert-PathAbsent (Join-Path $substRoot 'mapped\kettle') `
            'SUBST-backed install prefix'
    } finally {
        & subst.exe $substDrive /D
        if (Test-Path -LiteralPath $substRoot) {
            [System.IO.Directory]::Delete($substRoot, $true)
        }
    }
}

$savedPrefix = Get-Content (Join-Path $prefix '.kettle-install-prefix') -Raw
if ($savedPrefix.Trim() -ne $prefix) {
    throw "prefix marker mismatch: expected $prefix, got $savedPrefix"
}

# Upgrade publication must replace a destination directory entry atomically,
# not open an existing hard link and overwrite its unrelated backing file.
$hardLinkTarget = Join-Path $tempRoot 'kettle-installer-hardlink-target.txt'
$installedReadme = Join-Path $prefix 'README.md'
Set-Content -LiteralPath $hardLinkTarget -Value 'must-survive' -NoNewline
Remove-Item -LiteralPath $installedReadme -Force
New-Item -ItemType HardLink -Path $installedReadme -Target $hardLinkTarget |
    Out-Null
& (Join-Path $repo 'scripts\install.ps1') -Prefix $prefix `
    -IntegrationTestRoot $integrationRoot | Out-Null
Assert-Equal (Get-Content -LiteralPath $hardLinkTarget -Raw) `
    'must-survive' 'hard-link target after atomic upgrade'
if ((Get-Content -LiteralPath $installedReadme -Raw) -eq 'must-survive') {
    throw 'atomic upgrade did not replace the managed README directory entry'
}
Remove-Item -LiteralPath $hardLinkTarget -Force
Write-Output 'windows-installer check: atomic managed-file upgrade OK'

# Preserve compatibility with the exact bounded backup names left by older
# Windows installers. Near-miss `.bak-*` files remain unmanaged.
foreach ($legacyBackup in @(
    'kettle.com.bak-2.36.6-20260722',
    'kettle.exe.bak-2.36.6-20260722',
    'kettle.exe.bak-2026-07-21',
    'kettle.exe.bak-2036-6'
)) {
    Copy-Item -LiteralPath (Join-Path $prefix 'kettle.exe') `
        -Destination (Join-Path $prefix $legacyBackup)
}
$legacyNearMiss = Join-Path $prefix 'kettle.exe.bak-user-copy'
Set-Content -LiteralPath $legacyNearMiss -Value 'must-survive' -NoNewline
Assert-InstallerRejected `
    -Arguments @(
        '-Uninstall',
        '-Prefix',
        $prefix,
        '-IntegrationTestRoot',
        $integrationRoot
    ) `
    -MessagePattern '*unmanaged file*' `
    -Label 'noncanonical legacy binary backup'
Assert-PathPresent $legacyNearMiss 'legacy-backup near-miss sentinel'
Remove-Item -LiteralPath $legacyNearMiss -Force
Write-Output 'windows-installer check: exact legacy binary backups accepted'

# Ownership JSON is a security decision, not a loose configuration file.
# Duplicate/extra keys and scalar type changes must all fail closed, with the
# installed binary preserved.
$ownershipPath = Join-Path $prefix '.kettle-install.json'
$ownershipBytes = [System.IO.File]::ReadAllBytes($ownershipPath)
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)
$badOwnershipMarkers = @(
    '{"schema":1,"schema":1,"product":"kettle","managed_by":"kettle-installer","channel":"local-dev","target":"x86_64-pc-windows-msvc","version":"1.2.3"}',
    '{"schema":"1","product":"kettle","managed_by":"kettle-installer","channel":"local-dev","target":"x86_64-pc-windows-msvc","version":"1.2.3"}',
    '{"schema":1,"product":"kettle","managed_by":"kettle-installer","channel":"local-dev","target":"x86_64-pc-windows-msvc","version":"1.2.3","extra":true}',
    '{"schema":1,"product":1,"managed_by":"kettle-installer","channel":"local-dev","target":"x86_64-pc-windows-msvc","version":"1.2.3"}'
)
try {
    foreach ($badOwnershipMarker in $badOwnershipMarkers) {
        [System.IO.File]::WriteAllText(
            $ownershipPath,
            $badOwnershipMarker,
            $utf8NoBom
        )
        Assert-InstallerRejected `
            -Arguments @(
                '-Uninstall',
                '-Prefix',
                $prefix,
                '-IntegrationTestRoot',
                $integrationRoot
            ) `
            -MessagePattern '*install ownership marker*' `
            -Label 'malformed ownership marker'
        Assert-PathPresent (Join-Path $prefix 'kettle.exe') `
            'kettle.exe after rejected ownership marker'
    }
} finally {
    [System.IO.File]::WriteAllBytes($ownershipPath, $ownershipBytes)
}

# The uninstaller accepts one exact, bounded product tree. It must preserve an
# unexpected leaf, directory, or nested shell file rather than recursively
# sweeping it.
$unmanagedFile = Join-Path $prefix 'user-data.txt'
Set-Content -LiteralPath $unmanagedFile -Value 'must-survive' -NoNewline
Assert-InstallerRejected `
    -Arguments @(
        '-Uninstall',
        '-Prefix',
        $prefix,
        '-IntegrationTestRoot',
        $integrationRoot
    ) `
    -MessagePattern '*unmanaged file*' `
    -Label 'unmanaged root file'
Assert-PathPresent $unmanagedFile 'unmanaged root file'
Remove-Item -LiteralPath $unmanagedFile -Force

$unmanagedDirectory = Join-Path $prefix 'user-data'
$unmanagedDirectorySentinel = Join-Path $unmanagedDirectory 'must-survive.txt'
New-Item -ItemType Directory -Path $unmanagedDirectory | Out-Null
Set-Content -LiteralPath $unmanagedDirectorySentinel `
    -Value 'must-survive' -NoNewline
Assert-InstallerRejected `
    -Arguments @(
        '-Uninstall',
        '-Prefix',
        $prefix,
        '-IntegrationTestRoot',
        $integrationRoot
    ) `
    -MessagePattern '*unmanaged directory*' `
    -Label 'unmanaged root directory'
Assert-PathPresent $unmanagedDirectorySentinel 'unmanaged-directory sentinel'
Remove-Item -LiteralPath $unmanagedDirectory -Recurse -Force

$unmanagedShellFile = Join-Path $prefix 'shell-integration\user-data'
Set-Content -LiteralPath $unmanagedShellFile -Value 'must-survive' -NoNewline
Assert-InstallerRejected `
    -Arguments @(
        '-Uninstall',
        '-Prefix',
        $prefix,
        '-IntegrationTestRoot',
        $integrationRoot
    ) `
    -MessagePattern '*shell-integration*' `
    -Label 'unmanaged shell-integration file'
Assert-PathPresent $unmanagedShellFile 'unmanaged shell-integration file'
Remove-Item -LiteralPath $unmanagedShellFile -Force

# Replacing the only managed subdirectory with a junction must not make either
# upgrade or uninstall traverse into the junction target.
$shellPath = Join-Path $prefix 'shell-integration'
$shellBackup = Join-Path $tempRoot 'kettle-installer-shell-backup'
$shellTarget = Join-Path $tempRoot 'kettle-installer-shell-target'
$shellTargetSentinel = Join-Path $shellTarget 'must-survive.txt'
Remove-Item -LiteralPath $shellBackup -Recurse -Force `
    -ErrorAction SilentlyContinue
Remove-Item -LiteralPath $shellTarget -Recurse -Force `
    -ErrorAction SilentlyContinue
try {
    Move-Item -LiteralPath $shellPath -Destination $shellBackup
    New-Item -ItemType Directory -Path $shellTarget | Out-Null
    Set-Content -LiteralPath $shellTargetSentinel `
        -Value 'must-survive' -NoNewline
    New-Item -ItemType Junction -Path $shellPath -Target $shellTarget |
        Out-Null
    Assert-InstallerRejected `
        -Arguments @(
            '-Uninstall',
            '-Prefix',
            $prefix,
            '-IntegrationTestRoot',
            $integrationRoot
        ) `
        -MessagePattern '*reparse point*' `
        -Label 'shell-integration junction'
    Assert-PathPresent $shellTargetSentinel 'junction-target sentinel'
} finally {
    if (Test-Path -LiteralPath $shellPath) {
        [KettleInstaller.NativeFileSystemV1]::RemoveEmptyDirectory($shellPath)
    }
    if (Test-Path -LiteralPath $shellBackup) {
        Move-Item -LiteralPath $shellBackup -Destination $shellPath
    }
    Remove-Item -LiteralPath $shellTarget -Recurse -Force `
        -ErrorAction SilentlyContinue
}

# A saved helper must never let a mutable marker redirect recursive deletion.
# Corrupt the marker toward an unrelated sentinel tree and prove that both the
# install and unrelated target survive the rejected uninstall.
$redirectRoot = Join-Path $integrationRoot 'redirect-target'
$redirectSentinel = Join-Path $redirectRoot 'must-survive.txt'
New-Item -ItemType Directory -Force -Path $redirectRoot | Out-Null
Set-Content -LiteralPath $redirectSentinel -Value 'sentinel' -NoNewline
Set-Content -LiteralPath (Join-Path $prefix '.kettle-install-prefix') `
    -Value $redirectRoot -NoNewline
$redirectRejected = $false
try {
    & (Join-Path $prefix 'install.ps1') -Uninstall `
        -IntegrationTestRoot $integrationRoot | Out-Null
} catch {
    $redirectRejected = (
        $_.Exception.Message -like
            '*installed prefix marker does not name its helper directory*' -or
        $_.Exception.Message -like
            '*Install prefix must be a dedicated directory named kettle*'
    )
}
if (-not $redirectRejected) {
    throw 'a redirected uninstall prefix marker was not rejected'
}
Assert-PathPresent $redirectSentinel 'redirect-target sentinel'
Assert-PathPresent (Join-Path $prefix 'kettle.exe') `
    'installed kettle.exe after rejected redirected uninstall'
Set-Content -LiteralPath (Join-Path $prefix '.kettle-install-prefix') `
    -Value $prefix -NoNewline

# Explicit broad/shared targets are also fail-closed before ownership or
# deletion. This guard protects invocations made from the source helper.
$broadRejected = $false
try {
    & (Join-Path $repo 'scripts\install.ps1') -Uninstall `
        -Prefix $integrationRoot -IntegrationTestRoot $integrationRoot |
        Out-Null
} catch {
    $broadRejected = (
        $_.Exception.Message -like
            '*Install prefix must be a dedicated directory named kettle*' -or
        $_.Exception.Message -like
            '*protected broad directory*'
    )
}
if (-not $broadRejected) {
    throw 'a broad explicit uninstall prefix was not rejected'
}
Assert-PathPresent $redirectSentinel 'broad-target sentinel'

$versionFile = Join-Path $tempRoot 'kettle-windows-install-version.txt'
Remove-Item -Path $versionFile -ErrorAction SilentlyContinue
Start-Process -FilePath (Join-Path $prefix 'kettle.exe') `
    -ArgumentList '--version' `
    -NoNewWindow -Wait -RedirectStandardOutput $versionFile
$version = (Get-Content $versionFile -Raw).Trim()
if ($version -notmatch '^kettle \d+\.\d+\.\d+') {
    throw "unexpected installed kettle version output: $version"
}
Write-Output "windows-installer check: installed $version"

# A bare command must resolve to the console shim before kettle.exe so
# PowerShell waits and propagates CLI exit codes.
$processPathBefore = $env:Path
try {
    $env:Path = "$prefix;$processPathBefore"
    $resolvedKettle = (Get-Command kettle -CommandType Application | Select-Object -First 1).Source
    Assert-Equal $resolvedKettle (Join-Path $prefix 'kettle.com') 'bare kettle command resolution'
    $shimVersion = (& kettle --version | Out-String).Trim()
    Assert-Equal $LASTEXITCODE 0 'kettle.com version exit code'
    if ($shimVersion -notmatch '^kettle [0-9]+\.[0-9]+\.[0-9]+') {
        throw "unexpected kettle.com --version output: $shimVersion"
    }
} finally {
    $env:Path = $processPathBefore
}

# Interrupted authenticated-update state remains a managed, bounded tree. The
# saved installer must accept exact transaction artifacts, reject a near-miss,
# and be able to uninstall without following recursive aliases.
foreach ($badStageName in @(
    '.kettle-update-stage-not-exact',
    '.kettle-update-stage-4294967296-1',
    '.kettle-update-stage-01-2'
)) {
    $badStage = Join-Path $prefix $badStageName
    New-Item -ItemType Directory -Path $badStage | Out-Null
    Set-Content -LiteralPath (Join-Path $badStage 'README.md') `
        -Value 'must-survive' -NoNewline
    Assert-InstallerRejected `
        -Arguments @(
            '-Uninstall',
            '-Prefix',
            $prefix,
            '-IntegrationTestRoot',
            $integrationRoot
        ) `
        -MessagePattern '*unmanaged directory*' `
        -Label 'noncanonical updater stage'
    Assert-PathPresent (Join-Path $badStage 'README.md') `
        'noncanonical-stage sentinel'
    Remove-Item -LiteralPath $badStage -Recurse -Force
}

$transactionId = '123-456'
$stagePath = Join-Path $prefix ".kettle-update-stage-$transactionId"
$stageShell = Join-Path $stagePath 'shell-integration'
New-Item -ItemType Directory -Path $stageShell | Out-Null
Copy-Item -LiteralPath (Join-Path $prefix 'README.md') `
    -Destination (Join-Path $stagePath 'README.md')
Copy-Item -LiteralPath (Join-Path $prefix 'shell-integration\kettle.ps1') `
    -Destination (Join-Path $stageShell 'kettle.ps1')
$helperName = ".kettle-update-helper-$transactionId.exe"
$helperPath = Join-Path $prefix $helperName
Copy-Item -LiteralPath (Join-Path $prefix 'kettle.exe') `
    -Destination $helperPath
$pendingFiles = @(
    [ordered]@{
        path = 'README.md'
        size = (Get-Item -LiteralPath (
            Join-Path $stagePath 'README.md'
        )).Length
        sha256 = (Get-FileHash -LiteralPath (
            Join-Path $stagePath 'README.md'
        ) -Algorithm SHA256).Hash.ToLowerInvariant()
    },
    [ordered]@{
        path = 'shell-integration/kettle.ps1'
        size = (Get-Item -LiteralPath (
            Join-Path $stageShell 'kettle.ps1'
        )).Length
        sha256 = (Get-FileHash -LiteralPath (
            Join-Path $stageShell 'kettle.ps1'
        ) -Algorithm SHA256).Hash.ToLowerInvariant()
    }
)
$pending = [ordered]@{
    schema = 2
    product = 'kettle'
    target = 'x86_64-pc-windows-msvc'
    transaction_id = $transactionId
    target_version = '99.0.0'
    staging_dir = ".kettle-update-stage-$transactionId"
    helper = $helperName
    helper_size = (Get-Item -LiteralPath $helperPath).Length
    helper_sha256 = (Get-FileHash -LiteralPath $helperPath `
        -Algorithm SHA256).Hash.ToLowerInvariant()
    files = $pendingFiles
    attempts = 0
    handoff_timeouts = 0
    last_error = $null
} | ConvertTo-Json -Depth 5
[System.IO.File]::WriteAllText(
    (Join-Path $prefix '.kettle-update-pending.json'),
    $pending,
    $utf8NoBom
)
$pendingPath = Join-Path $prefix '.kettle-update-pending.json'
$legacyPending = $pending | ConvertFrom-Json
$legacyPending.schema = 1
[System.IO.File]::WriteAllText(
    $pendingPath,
    ($legacyPending | ConvertTo-Json -Depth 5),
    $utf8NoBom
)
Assert-InstallerRejected `
    -Arguments @(
        '-Uninstall',
        '-Prefix',
        $prefix,
        '-IntegrationTestRoot',
        $integrationRoot
    ) `
    -MessagePattern '*pending update record has an invalid artifact identity*' `
    -Label 'legacy pending-update schema'
$unknownPending = $pending | ConvertFrom-Json
$unknownPending | Add-Member -NotePropertyName unexpected `
    -NotePropertyValue 'must-fail-closed'
[System.IO.File]::WriteAllText(
    $pendingPath,
    ($unknownPending | ConvertTo-Json -Depth 5),
    $utf8NoBom
)
Assert-InstallerRejected `
    -Arguments @(
        '-Uninstall',
        '-Prefix',
        $prefix,
        '-IntegrationTestRoot',
        $integrationRoot
    ) `
    -MessagePattern '*pending update record has an invalid artifact identity*' `
    -Label 'pending-update unknown field'
[System.IO.File]::WriteAllText($pendingPath, $pending, $utf8NoBom)

$backupPath = Join-Path $prefix ".kettle-update-backup-$transactionId"
New-Item -ItemType Directory -Path $backupPath | Out-Null
$backupReadme = Join-Path $backupPath 'README.md'
Copy-Item -LiteralPath (Join-Path $prefix 'README.md') `
    -Destination $backupReadme
$backupSize = (Get-Item -LiteralPath $backupReadme).Length
$backupHash = (Get-FileHash -LiteralPath $backupReadme `
    -Algorithm SHA256).Hash.ToLowerInvariant()
$backupMarker = [ordered]@{
    schema = 2
    product = 'kettle'
    transaction_id = $transactionId
} | ConvertTo-Json
$journal = [ordered]@{
    schema = 2
    transaction_id = $transactionId
    target_version = '99.0.0'
    phase = 'applying'
    backup_dir = ".kettle-update-backup-$transactionId"
    entries = @(
        [ordered]@{
            relative = 'README.md'
            existed = $true
            previous_unix_mode = $null
            previous_size = $backupSize
            previous_sha256 = $backupHash
            replacement_size = $backupSize
            replacement_sha256 = $backupHash
            state = 'installed'
        }
    )
} | ConvertTo-Json -Depth 5
[System.IO.File]::WriteAllText(
    (Join-Path $backupPath '.kettle-update-backup.json'),
    $backupMarker,
    $utf8NoBom
)
[System.IO.File]::WriteAllText(
    (Join-Path $prefix '.kettle-update-journal.json'),
    $journal,
    $utf8NoBom
)
Set-Content -LiteralPath (
    Join-Path $prefix ".kettle-update-failed-$transactionId.txt"
) -Value 'bounded diagnostic' -NoNewline

& (Join-Path $prefix 'install.ps1') -RefreshIntegration `
    -IntegrationTestRoot $integrationRoot | Out-Null
Write-Output 'windows-installer check: schema-2 pending updater tree accepted'
Remove-Item -LiteralPath (
    Join-Path $prefix '.kettle-update-journal.json'
) -Force
& (Join-Path $prefix 'install.ps1') -RefreshIntegration `
    -IntegrationTestRoot $integrationRoot | Out-Null
Write-Output 'windows-installer check: post-commit orphan backup accepted'

# Run the saved helper without -Prefix; it should infer $prefix from the marker.
& (Join-Path $prefix 'install.ps1') -Uninstall -IntegrationTestRoot $integrationRoot |
    Tee-Object -FilePath (Join-Path $tempRoot 'kettle-windows-uninstall.out')

Assert-PathAbsent (Join-Path $prefix 'kettle.exe') 'kettle.exe'
Assert-PathAbsent (Join-Path $prefix 'install.ps1') 'install helper'
Assert-PathAbsent (Join-Path $prefix '.kettle-install-prefix') 'prefix marker'
Assert-PathPresent $shortcutPath 'default-install shortcut sentinel'
Assert-PathPresent $testUninstallKey 'default-install registry sentinel'
$sentinelAfter = $ws.CreateShortcut($shortcutPath)
Assert-Equal $sentinelAfter.Arguments '-NoProfile -File "default-install-sentinel.ps1"' 'shortcut sentinel arguments'
$registrySentinel = (Get-ItemProperty -Path $testUninstallKey -Name 'Sentinel').Sentinel
Assert-Equal $registrySentinel 'default-install' 'registry sentinel'
Assert-Equal (Get-Content $profilePath -Raw) $profileSentinel 'PowerShell profile sentinel'
Assert-Equal ([Environment]::GetEnvironmentVariable('Path', 'User')) $userPathBefore 'user PATH'

Write-Output 'windows-installer check: custom-prefix install/uninstall OK'

# Exercise the real default-install integration path under isolated filesystem
# and registry roots. Seed the exact upgrade hazard seen in production: WScript
# opens an existing shortcut and retains stale PowerShell recorder arguments
# unless the installer replaces or explicitly clears it.
$integrationPrefix = Join-Path $integrationRoot 'Programs\kettle'
Remove-Item -Force -ErrorAction SilentlyContinue $shortcutPath
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $testUninstallKey
$profileAdsName = 'kettle-installer-audit'
Set-Content -LiteralPath $profilePath -Stream $profileAdsName `
    -Value 'unsupported stream' -NoNewline
$adsRejected = $false
try {
    & (Join-Path $repo 'scripts\install.ps1') `
        -IntegrationTestRoot $integrationRoot -WithShellIntegration |
        Out-Null
} catch {
    $adsRejected = (
        $_.Exception.Message -like '*unsupported alternate stream*'
    )
} finally {
    Remove-Item -LiteralPath $profilePath -Stream $profileAdsName `
        -ErrorAction SilentlyContinue
}
if (-not $adsRejected) {
    throw 'a PowerShell profile alternate data stream was not rejected'
}
Assert-PathAbsent (Join-Path $integrationPrefix 'kettle.exe') `
    'kettle.exe after profile ADS preflight'

$brokenProfile = @"
# user profile sentinel
# >>> kettle shell-integration (managed by install.ps1)
"@
[System.IO.File]::WriteAllText(
    $profilePath,
    $brokenProfile,
    (New-Object System.Text.UTF8Encoding($false))
)
$brokenProfileBytes = [System.IO.File]::ReadAllBytes($profilePath)
$brokenRejected = $false
try {
    & (Join-Path $repo 'scripts\install.ps1') `
        -IntegrationTestRoot $integrationRoot -WithShellIntegration |
        Out-Null
} catch {
    $brokenRejected = (
        $_.Exception.Message -like
            '*ambiguous or unbalanced Kettle managed markers*'
    )
}
if (-not $brokenRejected) {
    throw 'a broken PowerShell profile managed block was not rejected'
}
Assert-PathAbsent (Join-Path $integrationPrefix 'kettle.exe') `
    'kettle.exe after broken-profile preflight'
Assert-Equal (
    [Convert]::ToBase64String(
        [System.IO.File]::ReadAllBytes($profilePath)
    )
) (
    [Convert]::ToBase64String($brokenProfileBytes)
) 'broken PowerShell profile preservation'

$profileUserText = "# user profile sentinel`n# no trailing newline"
$profileEncoding = New-Object System.Text.UnicodeEncoding(
    $false,
    $true,
    $true
)
[System.IO.File]::WriteAllText(
    $profilePath,
    $profileUserText,
    $profileEncoding
)
$profileAcl = Get-Acl -LiteralPath $profilePath
$profileAcl.SetAccessRuleProtection($true, $true)
Set-Acl -LiteralPath $profilePath -AclObject $profileAcl
$profileItem = Get-Item -LiteralPath $profilePath -Force
$profileItem.Attributes = (
    [System.IO.FileAttributes]::Hidden -bor
    [System.IO.FileAttributes]::ReadOnly
)
$profileBytesBefore = [System.IO.File]::ReadAllBytes($profilePath)
$profileAclBeforeObject = Get-Acl -LiteralPath $profilePath
$profileAclBefore = $profileAclBeforeObject.Sddl
$profileAttributesBefore = (
    Get-Item -LiteralPath $profilePath -Force
).Attributes
$profileCreationBefore = [System.IO.File]::GetCreationTimeUtc($profilePath)
$profileWriteBefore = [System.IO.File]::GetLastWriteTimeUtc($profilePath)
[System.IO.File]::SetAttributes(
    $profilePath,
    [System.IO.FileAttributes](
        [int]$profileAttributesBefore -band
        (-bnot [int][System.IO.FileAttributes]::ReadOnly)
    )
)
$retainedProfile =
    [KettleInstaller.NativeFileSystemV1]::CaptureProfile(
        $profilePath,
        4194304
)
$concurrentWriteRejected = $false
try {
    try {
        [System.IO.File]::WriteAllText(
            $profilePath,
            'concurrent replacement must not win during retained publication'
        )
    } catch {
        $concurrentWriteRejected = $true
    }
} finally {
    $retainedProfile.Dispose()
    [System.IO.File]::SetAttributes($profilePath, $profileAttributesBefore)
}
if (-not $concurrentWriteRejected) {
    throw 'a concurrent PowerShell profile replacement was not blocked'
}
Assert-Equal (
    [Convert]::ToBase64String(
        [System.IO.File]::ReadAllBytes($profilePath)
    )
) (
    [Convert]::ToBase64String($profileBytesBefore)
) 'retained PowerShell profile identity'

try {
    New-Item -ItemType Directory -Force -Path $startMenuDir | Out-Null
    $ws = New-Object -ComObject WScript.Shell
    $stale = $ws.CreateShortcut($shortcutPath)
    $stale.TargetPath = (Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe')
    $stale.Arguments = '-NoProfile -WindowStyle Hidden -File "kettle-rec.ps1"'
    $stale.WorkingDirectory = $tempRoot
    $stale.Save()

    & (Join-Path $repo 'scripts\install.ps1') -IntegrationTestRoot $integrationRoot -WithShellIntegration |
        Tee-Object -FilePath (Join-Path $tempRoot 'kettle-windows-default-install.out')

    Assert-PathPresent $shortcutPath 'Start menu shortcut'
    Assert-PathPresent $testUninstallKey 'isolated Add/Remove Programs entry'
    Assert-PathPresent (Join-Path $integrationPrefix '.kettle-install.json') 'default self-update ownership marker'
    $integrationMarker = Get-Content -LiteralPath (Join-Path $integrationPrefix '.kettle-install.json') -Raw | ConvertFrom-Json
    Assert-Equal $integrationMarker.channel 'local-dev' 'default repo install marker channel'
    $shortcut = $ws.CreateShortcut($shortcutPath)
    Assert-Equal $shortcut.TargetPath (Join-Path $integrationPrefix 'kettle.exe') 'shortcut target'
    Assert-Equal $shortcut.Arguments '' 'shortcut arguments'
    Assert-Equal $shortcut.WorkingDirectory $integrationPrefix 'shortcut working directory'
    # Exercise the updater-only metadata refresh path while the installed
    # executable remains in place. This must not attempt a self-copy.
    & (Join-Path $integrationPrefix 'install.ps1') -RefreshIntegration -IntegrationTestRoot $integrationRoot |
        Tee-Object -FilePath (Join-Path $tempRoot 'kettle-windows-refresh-integration.out')
    Assert-PathPresent $shortcutPath 'refreshed Start menu shortcut'
    Assert-PathPresent $testUninstallKey 'refreshed Add/Remove Programs entry'
    if ((Get-Content $profilePath -Raw) -notmatch 'kettle shell-integration \(managed by install\.ps1\)') {
        throw 'default install did not write isolated PowerShell profile integration'
    }
    $profileBytesInstalled = [System.IO.File]::ReadAllBytes($profilePath)
    if (
        $profileBytesInstalled[0] -ne 0xFF -or
        $profileBytesInstalled[1] -ne 0xFE
    ) {
        throw 'profile integration did not preserve the UTF-16LE BOM'
    }
    Assert-Equal (
        ConvertTo-KettleComparableDaclSddl (
            Get-Acl -LiteralPath $profilePath
        ).Sddl
    ) (
        ConvertTo-KettleComparableDaclSddl $profileAclBefore
    ) 'PowerShell profile DACL preservation'
    Assert-Equal (
        (Get-Acl -LiteralPath $profilePath).AreAccessRulesProtected.ToString()
    ) $profileAclBeforeObject.AreAccessRulesProtected.ToString() `
        'PowerShell profile DACL protection'
    Assert-Equal (
        (Get-Item -LiteralPath $profilePath -Force).Attributes
    ) $profileAttributesBefore 'PowerShell profile attribute preservation'
    Assert-Equal (
        [System.IO.File]::GetCreationTimeUtc($profilePath).Ticks
    ) $profileCreationBefore.Ticks 'PowerShell profile creation-time preservation'
    Assert-Equal (
        [System.IO.File]::GetLastWriteTimeUtc($profilePath).Ticks
    ) $profileWriteBefore.Ticks 'PowerShell profile write-time preservation'

    # Model a user edit after installation where the original profile did not
    # end in a newline. Uninstall must preserve a separator between the
    # original final line and this appended suffix.
    $installedProfileText = [System.IO.File]::ReadAllText(
        $profilePath,
        $profileEncoding
    )
    [System.IO.File]::SetAttributes(
        $profilePath,
        [System.IO.FileAttributes](
            [int]$profileAttributesBefore -band
            (-bnot [int][System.IO.FileAttributes]::ReadOnly)
        )
    )
    $profileSuffix = '# appended after the managed block'
    [byte[]]$profileUserEditBytes = @(
        $profileEncoding.GetPreamble()
    ) + @(
        $profileEncoding.GetBytes($installedProfileText + $profileSuffix)
    )
    $profileUserEditStream = [System.IO.File]::Open(
        $profilePath,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Write,
        [System.IO.FileShare]::Read
    )
    try {
        $profileUserEditStream.SetLength(0)
        $profileUserEditStream.Write(
            $profileUserEditBytes,
            0,
            $profileUserEditBytes.Length
        )
        $profileUserEditStream.Flush($true)
    } finally {
        $profileUserEditStream.Dispose()
        [System.IO.File]::SetAttributes(
            $profilePath,
            $profileAttributesBefore
        )
    }
    $profileAclAfterUserEdit = (Get-Acl -LiteralPath $profilePath).Sddl
    $profileWriteAfterUserEdit = [System.IO.File]::GetLastWriteTimeUtc($profilePath)

    & (Join-Path $integrationPrefix 'install.ps1') -Uninstall -IntegrationTestRoot $integrationRoot |
        Tee-Object -FilePath (Join-Path $tempRoot 'kettle-windows-default-uninstall.out')

    Assert-PathAbsent (Join-Path $integrationPrefix 'kettle.exe') 'default-install kettle.exe'
    Assert-PathAbsent $shortcutPath 'Start menu shortcut'
    Assert-PathAbsent $testUninstallKey 'isolated Add/Remove Programs entry'
    if ((Get-Content $profilePath -Raw) -match 'kettle shell-integration \(managed by install\.ps1\)') {
        throw 'default uninstall left isolated PowerShell profile integration behind'
    }
    Assert-Equal (
        [System.IO.File]::ReadAllText($profilePath, $profileEncoding)
    ) (
        $profileUserText + "`n" + $profileSuffix
    ) 'PowerShell profile appended-suffix separation'
    Assert-Equal (
        ConvertTo-KettleComparableDaclSddl (
            Get-Acl -LiteralPath $profilePath
        ).Sddl
    ) (
        ConvertTo-KettleComparableDaclSddl $profileAclAfterUserEdit
    ) 'PowerShell profile uninstall DACL preservation'
    Assert-Equal (
        (Get-Item -LiteralPath $profilePath -Force).Attributes
    ) $profileAttributesBefore 'PowerShell profile uninstall attribute preservation'
    Assert-Equal (
        [System.IO.File]::GetLastWriteTimeUtc($profilePath).Ticks
    ) $profileWriteAfterUserEdit.Ticks 'PowerShell profile uninstall write-time preservation'
    Write-Output 'windows-installer check: stale-shortcut upgrade repair OK'
} finally {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $integrationRoot
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $testUninstallKey
}

# Recreate the extracted release-zip layout separately from repo mode. Only
# this layout may opt into the stable self-update channel.
$zipRoot = Join-Path $tempRoot "kettle-windows-zip-fixture"
$zipInstallRoot = Join-Path $tempRoot "kettle-windows-zip-install"
$zipPrefix = Join-Path $zipInstallRoot 'kettle'
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue `
    $zipRoot, $zipInstallRoot
try {
    New-Item -ItemType Directory -Force -Path $zipRoot | Out-Null
    Copy-Item (Join-Path $repo 'target\release\kettle.exe') (Join-Path $zipRoot 'kettle.exe')
    Copy-Item (Join-Path $repo 'target\release\kettle-console.exe') (Join-Path $zipRoot 'kettle.com')
    Copy-Item (Join-Path $repo 'scripts\install.ps1') (Join-Path $zipRoot 'install.ps1')
    Copy-Item (Join-Path $repo 'packaging\windows\kettle.ico') `
        (Join-Path $zipRoot 'kettle.ico')
    Copy-Item (Join-Path $repo 'shell-integration') `
        (Join-Path $zipRoot 'shell-integration') -Recurse
    Copy-Item (Join-Path $repo 'README.md') (Join-Path $zipRoot 'README.md')
    $zipVersionMatch = [regex]::Match(
        ((& (Join-Path $zipRoot 'kettle.exe') --version | Out-String).Trim()),
        (
            '^kettle ([0-9]+\.[0-9]+\.[0-9]+)' +
            '(?: \([0-9a-f]{12}(?:\+dirty)?\))?$'
        )
    )
    if (-not $zipVersionMatch.Success) {
        throw 'Could not determine the release-zip fixture version.'
    }
    $unsignedPrefix = Join-Path $zipInstallRoot 'unsigned\kettle'
    try {
        & (Join-Path $zipRoot 'install.ps1') -Prefix $unsignedPrefix `
            -IntegrationTestRoot $integrationRoot
        throw 'A release layout without complete manifest provenance was accepted.'
    } catch {
        if (
            $_.Exception.Message -notlike '*Release package manifest*' -and
            $_.Exception.Message -notlike '*release package manifest*'
        ) {
            throw
        }
    }
    Assert-PathAbsent $unsignedPrefix 'unsigned release install prefix'
    & python (Join-Path $repo 'scripts\package-manifest.py') generate `
        --root $zipRoot --target x86_64-pc-windows-msvc `
        --version $zipVersionMatch.Groups[1].Value
    if ($LASTEXITCODE -ne 0) {
        throw 'Could not generate the release-zip provenance fixture.'
    }
    $hardKillShell = (Get-Process -Id $PID).Path
    foreach ($hardKillPhase in @(
        'initial-journal',
        'shell-directory',
        'stage',
        'publication-journal',
        'destination',
        'prefix-marker',
        'ownership-marker',
        'after-package-commit'
    )) {
        $hardKillRoot = Join-Path $zipInstallRoot (
            'hard-kill-' + $hardKillPhase
        )
        $hardKillPrefix = Join-Path $hardKillRoot 'kettle'
        [void][System.IO.Directory]::CreateDirectory($hardKillRoot)
        $env:KETTLE_INSTALLER_HARD_KILL_PHASE = $hardKillPhase
        try {
            $hardKillChild = Start-Process -FilePath $hardKillShell `
                -ArgumentList @(
                    '-NoLogo',
                    '-NoProfile',
                    '-File',
                    (Join-Path $zipRoot 'install.ps1'),
                    '-Prefix',
                    $hardKillPrefix,
                    '-IntegrationTestRoot',
                    $hardKillRoot
                ) -NoNewWindow -PassThru -Wait
        } finally {
            Remove-Item Env:\KETTLE_INSTALLER_HARD_KILL_PHASE `
                -ErrorAction SilentlyContinue
        }
        if ($hardKillChild.ExitCode -ne 197) {
            throw (
                "hard-kill phase $hardKillPhase exited " +
                "$($hardKillChild.ExitCode), expected 197"
            )
        }
        $hardKillChild.Dispose()
        if ($hardKillPhase -cne 'after-package-commit') {
            Assert-PathPresent ($hardKillPrefix + '.install-transaction') `
                "hard-kill transaction $hardKillPhase"
        }
        if ($hardKillPhase -ceq 'prefix-marker') {
            Assert-PathPresent (
                Join-Path $hardKillPrefix '.kettle-install-prefix'
            ) 'post-publication prefix marker'
            Assert-PathAbsent (
                Join-Path $hardKillPrefix '.kettle-install.json'
            ) 'pre-publication ownership marker'
        } elseif ($hardKillPhase -ceq 'ownership-marker') {
            Assert-PathPresent (
                Join-Path $hardKillPrefix '.kettle-install-prefix'
            ) 'post-publication prefix marker before ownership checkpoint'
            Assert-PathPresent (
                Join-Path $hardKillPrefix '.kettle-install.json'
            ) 'post-publication ownership marker'
        }
        if (
            $hardKillPhase -ceq 'after-package-commit' -and (
                -not (Test-Path -LiteralPath (
                    Join-Path $hardKillPrefix '.kettle-install-prefix'
                ) -PathType Leaf) -or
                -not (Test-Path -LiteralPath (
                    Join-Path $hardKillPrefix '.kettle-install.json'
                ) -PathType Leaf)
            )
        ) {
            throw 'post-commit hard kill did not retain both ownership markers'
        }
        if ($hardKillPhase -cne 'after-package-commit') {
            $env:KETTLE_INSTALLER_TEST_RECOVER_ONLY = '1'
            try {
                & (Join-Path $zipRoot 'install.ps1') `
                    -Prefix $hardKillPrefix `
                    -IntegrationTestRoot $hardKillRoot | Out-Null
            } finally {
                Remove-Item Env:\KETTLE_INSTALLER_TEST_RECOVER_ONLY `
                    -ErrorAction SilentlyContinue
            }
            Assert-PathAbsent ($hardKillPrefix + '.install-transaction') `
                "recovery-only transaction $hardKillPhase"
            $recoveredFirstInstallChildren = @(
                Get-ChildItem -LiteralPath $hardKillPrefix -Force |
                    Where-Object {
                        $_.Name -cnotin @(
                            '.kettle-update.lock',
                            '.kettle-running.lock'
                        )
                    }
            )
            if ($recoveredFirstInstallChildren.Count -ne 0) {
                throw (
                    'first-install recovery retained managed payload after ' +
                    "$hardKillPhase"
                )
            }
        }
        & (Join-Path $zipRoot 'install.ps1') -Prefix $hardKillPrefix `
            -IntegrationTestRoot $hardKillRoot | Out-Null
        Assert-PathAbsent ($hardKillPrefix + '.install-transaction') `
            "recovered hard-kill transaction $hardKillPhase"
        Assert-PathPresent (
            Join-Path $hardKillPrefix '.kettle-install-prefix'
        ) "recovered prefix marker $hardKillPhase"
        Assert-PathPresent (
            Join-Path $hardKillPrefix '.kettle-install.json'
        ) "recovered ownership marker $hardKillPhase"
        $hardKillTemps = @(
            Get-ChildItem -LiteralPath $hardKillPrefix -Force -Recurse |
                Where-Object {
                    $_.Name -cmatch
                        '^\.kettle-install-tmp-[0-9a-f]{32}$'
                }
        )
        if ($hardKillTemps.Count -ne 0) {
            throw "hard-kill recovery left a temporary file: $hardKillPhase"
        }
        & (Join-Path $zipRoot 'install.ps1') -Uninstall `
            -Prefix $hardKillPrefix -IntegrationTestRoot $hardKillRoot |
            Out-Null
    }
    Write-Output 'windows-installer check: subprocess hard-kill recovery OK'

    # The two marker phases fire after atomic publication. Exercise them
    # separately over an existing install and prove recovery restores every
    # prior byte rather than accepting the partially upgraded marker set.
    $upgradeKillRoot = Join-Path $zipInstallRoot 'hard-kill-upgrade'
    $upgradeKillPrefix = Join-Path $upgradeKillRoot 'kettle'
    [void][System.IO.Directory]::CreateDirectory($upgradeKillRoot)
    & (Join-Path $zipRoot 'install.ps1') -Prefix $upgradeKillPrefix `
        -IntegrationTestRoot $upgradeKillRoot | Out-Null
    foreach ($upgradeKillPhase in @('prefix-marker', 'ownership-marker')) {
        $upgradeReadme = Join-Path $upgradeKillPrefix 'README.md'
        $upgradeOwnership = Join-Path $upgradeKillPrefix '.kettle-install.json'
        $upgradeSentinel = (
            "pre-upgrade sentinel for $upgradeKillPhase"
        )
        [System.IO.File]::WriteAllBytes(
            $upgradeReadme,
            (New-Object System.Text.UTF8Encoding($false)).GetBytes(
                $upgradeSentinel
            )
        )
        $oldOwnershipObject = Get-Content -LiteralPath $upgradeOwnership `
            -Raw | ConvertFrom-Json
        $oldOwnershipObject.version = '0.0.0'
        [System.IO.File]::WriteAllBytes(
            $upgradeOwnership,
            (New-Object System.Text.UTF8Encoding($false)).GetBytes(
                (($oldOwnershipObject | ConvertTo-Json) + "`n")
            )
        )
        $upgradeReadmeBefore =
            [System.IO.File]::ReadAllBytes($upgradeReadme)
        $upgradeOwnershipBefore =
            [System.IO.File]::ReadAllBytes($upgradeOwnership)
        $env:KETTLE_INSTALLER_HARD_KILL_PHASE = $upgradeKillPhase
        try {
            $upgradeKillChild = Start-Process -FilePath $hardKillShell `
                -ArgumentList @(
                    '-NoLogo',
                    '-NoProfile',
                    '-File',
                    (Join-Path $zipRoot 'install.ps1'),
                    '-Prefix',
                    $upgradeKillPrefix,
                    '-IntegrationTestRoot',
                    $upgradeKillRoot
                ) -NoNewWindow -PassThru -Wait
        } finally {
            Remove-Item Env:\KETTLE_INSTALLER_HARD_KILL_PHASE `
                -ErrorAction SilentlyContinue
        }
        if ($upgradeKillChild.ExitCode -ne 197) {
            throw (
                "upgrade hard-kill phase $upgradeKillPhase exited " +
                "$($upgradeKillChild.ExitCode), expected 197"
            )
        }
        $upgradeKillChild.Dispose()
        $publishedOwnership =
            [System.IO.File]::ReadAllBytes($upgradeOwnership)
        $ownershipChanged = (
            [Convert]::ToBase64String($publishedOwnership) -cne
            [Convert]::ToBase64String($upgradeOwnershipBefore)
        )
        if (
            ($upgradeKillPhase -ceq 'prefix-marker' -and $ownershipChanged) -or
            ($upgradeKillPhase -ceq 'ownership-marker' -and -not $ownershipChanged)
        ) {
            throw "marker publication boundary was incorrect: $upgradeKillPhase"
        }
        $env:KETTLE_INSTALLER_TEST_RECOVER_ONLY = '1'
        try {
            & (Join-Path $zipRoot 'install.ps1') `
                -Prefix $upgradeKillPrefix `
                -IntegrationTestRoot $upgradeKillRoot | Out-Null
        } finally {
            Remove-Item Env:\KETTLE_INSTALLER_TEST_RECOVER_ONLY `
                -ErrorAction SilentlyContinue
        }
        Assert-Equal (
            [Convert]::ToBase64String(
                [System.IO.File]::ReadAllBytes($upgradeReadme)
            )
        ) ([Convert]::ToBase64String($upgradeReadmeBefore)) `
            "upgrade README rollback $upgradeKillPhase"
        Assert-Equal (
            [Convert]::ToBase64String(
                [System.IO.File]::ReadAllBytes($upgradeOwnership)
            )
        ) ([Convert]::ToBase64String($upgradeOwnershipBefore)) `
            "upgrade ownership rollback $upgradeKillPhase"
        Assert-PathAbsent ($upgradeKillPrefix + '.install-transaction') `
            "upgrade hard-kill transaction $upgradeKillPhase"
    }
    & (Join-Path $zipRoot 'install.ps1') -Uninstall `
        -Prefix $upgradeKillPrefix -IntegrationTestRoot $upgradeKillRoot |
        Out-Null
    Write-Output 'windows-installer check: post-publication marker rollback OK'

    $env:KETTLE_INSTALLER_FAULT_AFTER_JOURNAL = '1'
    $env:KETTLE_INSTALLER_TEST_LEAVE_TRANSACTION = '1'
    $journalFaultObserved = $false
    try {
        & (Join-Path $zipRoot 'install.ps1') -Prefix $zipPrefix `
            -IntegrationTestRoot $integrationRoot | Out-Null
    } catch {
        $journalFaultObserved = (
            $_.Exception.Message -like
                '*Injected installer journal failure before publication 1*'
        )
    } finally {
        Remove-Item Env:\KETTLE_INSTALLER_FAULT_AFTER_JOURNAL `
            -ErrorAction SilentlyContinue
        Remove-Item Env:\KETTLE_INSTALLER_TEST_LEAVE_TRANSACTION `
            -ErrorAction SilentlyContinue
    }
    if (-not $journalFaultObserved) {
        throw 'the write-ahead package journal checkpoint was not observed'
    }
    $writeAheadTransaction = $zipPrefix + '.install-transaction'
    Assert-PathPresent $writeAheadTransaction `
        'write-ahead package transaction evidence'
    $writeAheadTransactionLock =
        [KettleInstaller.NativeFileSystemV1]::LockPrivateDirectory(
            $writeAheadTransaction
        )
    $writeAheadTransactionLock.Dispose()
    if (-not (Get-Acl -LiteralPath $writeAheadTransaction).AreAccessRulesProtected) {
        throw 'write-ahead transaction DACL was not protected'
    }
    $writeAheadJournal = Get-Content -LiteralPath (
        Join-Path $writeAheadTransaction 'journal.json'
    ) -Raw | ConvertFrom-Json
    Assert-Equal $writeAheadJournal.published.ToString() '1' `
        'write-ahead package journal coverage'
    $prePublicationPayload = @(
        Get-ChildItem -LiteralPath $zipPrefix -Force |
            Where-Object {
                $_.Name -cnotin @(
                    '.kettle-update.lock',
                    '.kettle-running.lock'
                )
            }
    )
    if ($prePublicationPayload.Count -ne 0) {
        throw 'write-ahead journal checkpoint published a package file early'
    }
    & (Join-Path $zipRoot 'install.ps1') -Prefix $zipPrefix `
        -IntegrationTestRoot $integrationRoot | Out-Null
    Assert-PathAbsent $writeAheadTransaction `
        'recovered write-ahead package transaction evidence'
    & (Join-Path $zipRoot 'install.ps1') -Uninstall -Prefix $zipPrefix `
        -IntegrationTestRoot $integrationRoot | Out-Null
    Assert-PathAbsent (Join-Path $zipPrefix 'kettle.exe') `
        'write-ahead recovery fixture kettle.exe'
    Write-Output 'windows-installer check: write-ahead package recovery OK'

    foreach ($faultCheckpoint in 1..4) {
        $env:KETTLE_INSTALLER_FAULT_AFTER_PUBLICATIONS =
            $faultCheckpoint.ToString()
        $faultObserved = $false
        try {
            & (Join-Path $zipRoot 'install.ps1') -Prefix $zipPrefix `
                -IntegrationTestRoot $integrationRoot | Out-Null
        } catch {
            $faultObserved = (
                $_.Exception.Message -like
                    '*Injected installer publication failure*'
            )
        } finally {
            Remove-Item Env:\KETTLE_INSTALLER_FAULT_AFTER_PUBLICATIONS `
                -ErrorAction SilentlyContinue
        }
        if (-not $faultObserved) {
            throw "installer fault checkpoint $faultCheckpoint was not observed"
        }
        $publishedPayload = @(
            Get-ChildItem -LiteralPath $zipPrefix -Force |
                Where-Object {
                    $_.Name -cnotin @(
                        '.kettle-update.lock',
                        '.kettle-running.lock'
                    )
                }
        )
        if ($publishedPayload.Count -ne 0) {
            throw "installer fault checkpoint $faultCheckpoint left partial payload"
        }
        Assert-PathAbsent ($zipPrefix + '.install-transaction') `
            "installer transaction after fault checkpoint $faultCheckpoint"
    }
    $env:KETTLE_INSTALLER_FAULT_AFTER_PUBLICATIONS = '2'
    $env:KETTLE_INSTALLER_TEST_LEAVE_TRANSACTION = '1'
    try {
        & (Join-Path $zipRoot 'install.ps1') -Prefix $zipPrefix `
            -IntegrationTestRoot $integrationRoot | Out-Null
        throw 'the interrupted installer transaction fixture did not fail'
    } catch {
        if (
            $_.Exception.Message -notlike
                '*Injected installer publication failure*'
        ) {
            throw
        }
    } finally {
        Remove-Item Env:\KETTLE_INSTALLER_FAULT_AFTER_PUBLICATIONS `
            -ErrorAction SilentlyContinue
        Remove-Item Env:\KETTLE_INSTALLER_TEST_LEAVE_TRANSACTION `
            -ErrorAction SilentlyContinue
    }
    Assert-PathPresent ($zipPrefix + '.install-transaction') `
        'interrupted installer transaction evidence'
    & (Join-Path $zipRoot 'install.ps1') -Prefix $zipPrefix -IntegrationTestRoot $integrationRoot |
        Tee-Object -FilePath (Join-Path $tempRoot 'kettle-windows-zip-install.out')
    Assert-PathAbsent ($zipPrefix + '.install-transaction') `
        'recovered installer transaction evidence'
    $zipMarker = Get-Content -LiteralPath (Join-Path $zipPrefix '.kettle-install.json') -Raw | ConvertFrom-Json
    Assert-Equal $zipMarker.channel 'stable' 'release zip install marker channel'
    $zipBytesBeforeFaults = @{}
    foreach ($item in Get-ChildItem -LiteralPath $zipPrefix -File -Recurse) {
        $relative = $item.FullName.Substring($zipPrefix.Length)
        $zipBytesBeforeFaults[$relative] = (
            Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256
        ).Hash
    }
    foreach ($faultCheckpoint in 1..4) {
        $env:KETTLE_INSTALLER_FAULT_AFTER_PUBLICATIONS =
            $faultCheckpoint.ToString()
        try {
            & (Join-Path $zipRoot 'install.ps1') -Prefix $zipPrefix `
                -IntegrationTestRoot $integrationRoot | Out-Null
            throw "upgrade fault checkpoint $faultCheckpoint did not fail"
        } catch {
            if (
                $_.Exception.Message -notlike
                    '*Injected installer publication failure*'
            ) {
                throw
            }
        } finally {
            Remove-Item Env:\KETTLE_INSTALLER_FAULT_AFTER_PUBLICATIONS `
                -ErrorAction SilentlyContinue
        }
        $zipFilesAfterFault = @(
            Get-ChildItem -LiteralPath $zipPrefix -File -Recurse
        )
        if ($zipFilesAfterFault.Count -ne $zipBytesBeforeFaults.Count) {
            throw "upgrade rollback changed the file set at checkpoint $faultCheckpoint"
        }
        foreach ($item in $zipFilesAfterFault) {
            $relative = $item.FullName.Substring($zipPrefix.Length)
            if (
                -not $zipBytesBeforeFaults.ContainsKey($relative) -or
                $zipBytesBeforeFaults[$relative] -cne (
                    Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256
                ).Hash
            ) {
                throw "upgrade rollback changed $relative at checkpoint $faultCheckpoint"
            }
        }
        Assert-PathAbsent ($zipPrefix + '.install-transaction') `
            "upgrade transaction after fault checkpoint $faultCheckpoint"
    }
    & (Join-Path $zipPrefix 'install.ps1') -IntegrationTestRoot $integrationRoot |
        Tee-Object -FilePath (Join-Path $tempRoot 'kettle-windows-saved-helper-rerun.out')
    $rerunMarker = Get-Content -LiteralPath (Join-Path $zipPrefix '.kettle-install.json') -Raw | ConvertFrom-Json
    Assert-Equal $rerunMarker.channel 'stable' 'saved helper preserves install marker channel'
    & (Join-Path $zipPrefix 'install.ps1') -Uninstall -IntegrationTestRoot $integrationRoot |
        Tee-Object -FilePath (Join-Path $tempRoot 'kettle-windows-zip-uninstall.out')
    Assert-PathAbsent (Join-Path $zipPrefix 'kettle.exe') 'release zip kettle.exe'
    Write-Output 'windows-installer check: release zip stable channel OK'
} finally {
    Remove-Item Env:\KETTLE_INSTALLER_FAULT_AFTER_JOURNAL `
        -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue `
        $zipRoot, $zipInstallRoot
}
