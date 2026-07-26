# Shared terminal discovery and launch contracts for the Windows performance
# harness. Keep one source of truth so startup, latency, throughput, and
# vtebench do not silently benchmark different binaries or argument shapes.

Set-StrictMode -Version Latest
. "$PSScriptRoot\process-capture.ps1"

function Get-KettlePerfExecutableSha256 {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Executable
    )

    $resolved = (Resolve-Path -LiteralPath $Executable -ErrorAction Stop).Path
    $item = Get-Item -LiteralPath $resolved -Force -ErrorAction Stop
    if (
        $item.PSIsContainer -or
        ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0
    ) {
        throw "Benchmark executable must be an ordinary file: $resolved"
    }
    return [string](
        Get-FileHash -LiteralPath $resolved -Algorithm SHA256 -ErrorAction Stop
    ).Hash
}

function Open-KettlePerfExecutableLease {
    param(
        [Parameter(Mandatory)]
        [string]$Executable,
        [Parameter(Mandatory)]
        [ValidatePattern('^[0-9A-Fa-f]{64}$')]
        [string]$ExpectedSha256
    )

    $resolved = (Resolve-Path -LiteralPath $Executable -ErrorAction Stop).Path
    $item = Get-Item -LiteralPath $resolved -Force -ErrorAction Stop
    if (
        $item.PSIsContainer -or
        ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0
    ) {
        throw "Benchmark executable must be an ordinary file: $resolved"
    }
    $stream = $null
    $algorithm = $null
    try {
        # FileShare.Read denies mutation, replacement, and deletion for the
        # lifetime of the lease while still permitting the OS loader to map it.
        $stream = [IO.FileStream]::new(
            $resolved,
            [IO.FileMode]::Open,
            [IO.FileAccess]::Read,
            [IO.FileShare]::Read
        )
        $algorithm = [Security.Cryptography.SHA256]::Create()
        $actualHash = [BitConverter]::ToString(
            $algorithm.ComputeHash($stream)
        ).Replace('-', '')
        $stream.Position = 0
        if (-not [StringComparer]::OrdinalIgnoreCase.Equals(
            $actualHash,
            $ExpectedSha256
        )) {
            throw "Benchmark executable changed before it could be leased: $resolved"
        }
        return [pscustomobject]@{
            Path = $resolved
            Sha256 = $actualHash
            Length = [long]$stream.Length
            Stream = $stream
        }
    } catch {
        if ($null -ne $stream) {
            $stream.Dispose()
        }
        throw
    } finally {
        if ($null -ne $algorithm) {
            $algorithm.Dispose()
        }
    }
}

function Close-KettlePerfExecutableLease {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'This only closes a retained read lease.'
    )]
    param(
        $Lease
    )

    if ($null -ne $Lease -and $null -ne $Lease.Stream) {
        $Lease.Stream.Dispose()
    }
}

function Find-KettlePerfExecutable {
    param(
        [string]$Explicit = '',
        [string]$EnvironmentVariable = '',
        [string[]]$Candidates = @()
    )

    function Resolve-ExecutableCandidate {
        param([string]$Candidate)

        if (Test-Path -LiteralPath $Candidate -PathType Leaf) {
            return (Resolve-Path -LiteralPath $Candidate).Path
        }
        $command = Get-Command $Candidate -CommandType Application -ErrorAction SilentlyContinue |
            Select-Object -First 1
        if ($command) {
            return $command.Source
        }
        return $null
    }

    if ($Explicit) {
        $resolved = Resolve-ExecutableCandidate $Explicit
        if (-not $resolved) {
            throw "Explicit benchmark executable was not found: $Explicit"
        }
        return $resolved
    }
    if ($EnvironmentVariable) {
        $fromEnvironment = [Environment]::GetEnvironmentVariable($EnvironmentVariable)
        if ($fromEnvironment) {
            $resolved = Resolve-ExecutableCandidate $fromEnvironment
            if (-not $resolved) {
                throw "$EnvironmentVariable points to a missing benchmark executable: $fromEnvironment"
            }
            return $resolved
        }
    }
    foreach ($candidate in $Candidates) {
        if ($candidate) {
            $resolved = Resolve-ExecutableCandidate $candidate
            if ($resolved) {
                return $resolved
            }
        }
    }
    return $null
}

function Find-KettlePerfComparator {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Root,
        [Parameter(Mandatory = $true)]
        [string]$Leaf
    )

    if (-not (Test-Path -LiteralPath $Root -PathType Container)) {
        return $null
    }
    return Get-ChildItem -LiteralPath $Root -Recurse -File -Filter $Leaf -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTimeUtc -Descending |
        Select-Object -First 1 -ExpandProperty FullName
}

function Get-KettlePerfRioCandidates {
    param(
        [Parameter(Mandatory = $true)]
        [string]$ProgramFilesRoot,
        [Parameter(Mandatory = $true)]
        [string]$LocalAppDataRoot,
        [AllowEmptyString()]
        [string]$Portable = ''
    )

    return [string[]]@(
        'rio.exe',
        (Join-Path $ProgramFilesRoot 'Rio\rio.exe'),
        (Join-Path $ProgramFilesRoot 'Rio\bin\rio.exe'),
        (Join-Path $LocalAppDataRoot 'Programs\Rio\rio.exe'),
        (Join-Path $LocalAppDataRoot 'Programs\Rio\bin\rio.exe'),
        $Portable
    )
}

function Get-KettlePerfCliExecutable {
    param(
        [Parameter(Mandatory = $true)]
        [string]$GuiExecutable
    )

    $directory = Split-Path -Parent $GuiExecutable
    foreach ($leaf in @('kettle-console.exe', 'kettle.com')) {
        $candidate = Join-Path $directory $leaf
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            return (Resolve-Path -LiteralPath $candidate).Path
        }
    }
    # Source-tree/debug layouts can temporarily have only the GUI binary.
    # Capturing its output still works in many hosts; callers initialize
    # LASTEXITCODE before invoking it because GUI-subsystem processes do not
    # guarantee that PowerShell sets the automatic variable.
    return $GuiExecutable
}

function Get-KettlePerfVersion {
    param(
        [Parameter(Mandatory = $true)]
        $Spec
    )

    if (-not $Spec.Available) {
        return $null
    }
    if ($Spec.Name -eq 'kettle' -and [bool]$Spec.HasReliableCli) {
        try {
            $capture = Invoke-KettlePerfBoundedProcess `
                -FilePath $Spec.CliExe -ArgumentList @('--version') `
                -TimeoutMs 10000 -MaxStdoutBytes 65536 -MaxStderrBytes 65536
            $line = @(
                $capture.StandardOutput -split "\r?\n" |
                    Where-Object { $_ }
            ) | Select-Object -First 1
            if ($capture.ExitCode -eq 0 -and $line) {
                return [string]$line
            }
        } catch {
            Write-Verbose "Kettle version probe failed: $($_.Exception.Message)"
        }
    }
    if ($Spec.Name -eq 'wt') {
        try {
            $package = Get-AppxPackage -Name Microsoft.WindowsTerminal -ErrorAction Stop |
                Sort-Object Version -Descending |
                Select-Object -First 1
            if ($package) {
                return [string]$package.Version
            }
        } catch {
            Write-Verbose "Windows Terminal package version probe failed: $($_.Exception.Message)"
        }
    }
    if ($Spec.Name -eq 'wezterm') {
        try {
            $version = (Get-Item -LiteralPath $Spec.Exe -ErrorAction Stop).VersionInfo
            if ($version.ProductVersion) {
                return $version.ProductVersion
            }
        } catch {
            Write-Verbose "WezTerm file version probe failed: $($_.Exception.Message)"
        }
    }
    if ($Spec.Name -in @('alacritty', 'wezterm', 'rio')) {
        try {
            $capture = Invoke-KettlePerfBoundedProcess `
                -FilePath $Spec.Exe -ArgumentList @('--version') `
                -TimeoutMs 10000 -MaxStdoutBytes 65536 -MaxStderrBytes 65536
            $line = @(
                $capture.StandardOutput -split "\r?\n" |
                    Where-Object { $_ }
            ) | Select-Object -First 1
            if ($capture.ExitCode -eq 0 -and $line) {
                return [string]$line
            }
        } catch {
            Write-Verbose "$($Spec.Name) CLI version probe failed: $($_.Exception.Message)"
        }
    }
    try {
        $version = (Get-Item -LiteralPath $Spec.Exe -ErrorAction Stop).VersionInfo
        if ($version.ProductVersion) {
            return $version.ProductVersion
        }
        if ($version.FileVersion) {
            return $version.FileVersion
        }
    } catch {
        Write-Verbose "$($Spec.Name) file version probe failed: $($_.Exception.Message)"
    }
    return $null
}

function Get-KettlePerfIsolatedConfigEntry {
    param(
        $ConfigProfile,
        [Parameter(Mandatory = $true)]
        [ValidateSet('kettle', 'wt', 'alacritty', 'wezterm', 'rio', 'tabby')]
        [string]$Name
    )

    if ($null -eq $ConfigProfile) {
        return $null
    }
    if (
        [int]$ConfigProfile.schema_version -ne 1 -or
        $null -eq $ConfigProfile.terminals
    ) {
        throw 'Isolated benchmark configuration has an unsupported schema'
    }
    $property = $ConfigProfile.terminals.PSObject.Properties[$Name]
    if ($null -eq $property) {
        return $null
    }
    return $property.Value
}

function Resolve-KettlePerfTerminal {
    param(
        [Parameter(Mandatory = $true)]
        [ValidateSet('kettle', 'wt', 'alacritty', 'wezterm', 'rio', 'tabby')]
        [string]$Name,
        [string]$KettleExe = '',
        [string]$KettleConfig = '',
        [string]$AlacrittyExe = '',
        [string]$WeztermExe = '',
        [string]$RioExe = '',
        [string]$TabbyExe = '',
        $IsolatedConfig = $null
    )

    $benchRoot = Join-Path $env:LOCALAPPDATA 'KettleBench\comparators'
    $helperBinaries = [object[]]@()
    $commandShell = $null
    $commandPowerShell = $null
    $launchEnvironment = [ordered]@{}
    $configurationMode = 'uncontrolled'
    $configurationEvidence = $null
    if ($Name -eq 'kettle' -and $KettleConfig -and $null -ne $IsolatedConfig) {
        throw 'KettleConfig and IsolatedConfig are mutually exclusive'
    }
    switch ($Name) {
        'kettle' {
            $exe = Find-KettlePerfExecutable -Explicit $KettleExe `
                -EnvironmentVariable 'KETTLE_PERF_KETTLE_EXE' `
                -Candidates @((Join-Path $PSScriptRoot '..\..\target\release\kettle.exe'), 'kettle.exe')
            $configArgs = @()
            if ($KettleConfig) {
                if (-not (Test-Path -LiteralPath $KettleConfig -PathType Leaf)) {
                    throw "Kettle benchmark config not found: $KettleConfig"
                }
                $configArgs = @('--config', (Resolve-Path -LiteralPath $KettleConfig).Path)
            }
            # Bare Kettle launches may hand the request to an existing primary
            # process. Benchmarks need an attributable process and a genuinely
            # new renderer/device lifecycle for every sample.
            $startup = @('--new-process') + $configArgs
            $command = @('--new-process') + $configArgs + @('-e')
            $supportsCommand = $true
            $commandConfirmation = $null
            $windowProcessNames = @('kettle')
        }
        'wt' {
            $exe = Find-KettlePerfExecutable -EnvironmentVariable 'KETTLE_PERF_WT_EXE' `
                -Candidates @('wt.exe')
            # A new top-level window is essential for startup timing. Windows
            # Terminal may still host it in an existing process; callers detect
            # that and decline per-process memory/CPU attribution.
            $startup = @('-w', 'new')
            $command = @('-w', 'new')
            $supportsCommand = $true
            $commandConfirmation = $null
            $windowProcessNames = @('WindowsTerminal', 'wt')
        }
        'alacritty' {
            $exe = Find-KettlePerfExecutable -Explicit $AlacrittyExe `
                -EnvironmentVariable 'KETTLE_PERF_ALACRITTY_EXE' `
                -Candidates @(
                    'alacritty.exe',
                    (Join-Path $env:ProgramFiles 'Alacritty\alacritty.exe'),
                    (Join-Path $env:LOCALAPPDATA 'Programs\Alacritty\alacritty.exe')
                )
            $startup = @()
            $command = @('-e')
            $supportsCommand = $true
            $commandConfirmation = $null
            $windowProcessNames = @('alacritty')
        }
        'wezterm' {
            $portable = Find-KettlePerfComparator `
                -Root (Join-Path $benchRoot 'wezterm-nightly') -Leaf 'wezterm-gui.exe'
            $exe = Find-KettlePerfExecutable -Explicit $WeztermExe `
                -EnvironmentVariable 'KETTLE_PERF_WEZTERM_EXE' `
                -Candidates @(
                    'wezterm-gui.exe',
                    (Join-Path $env:ProgramFiles 'WezTerm\wezterm-gui.exe'),
                    (Join-Path $env:LOCALAPPDATA 'Programs\WezTerm\wezterm-gui.exe'),
                    $portable
                )
            # Avoid the normal mux-server handoff so each sample has an
            # attributable process tree.
            $startup = @('start', '--always-new-process')
            $command = @('start', '--always-new-process', '--')
            $supportsCommand = $true
            $commandConfirmation = $null
            $windowProcessNames = @('wezterm-gui')
        }
        'rio' {
            $portable = Find-KettlePerfComparator -Root $benchRoot -Leaf 'rio.exe'
            $exe = Find-KettlePerfExecutable -Explicit $RioExe `
                -EnvironmentVariable 'KETTLE_PERF_RIO_EXE' `
                -Candidates (Get-KettlePerfRioCandidates `
                    -ProgramFilesRoot $env:ProgramFiles `
                    -LocalAppDataRoot $env:LOCALAPPDATA `
                    -Portable $portable)
            $startup = @()
            $command = @('-e')
            $supportsCommand = $true
            $commandConfirmation = $null
            $windowProcessNames = @('rio')
        }
        'tabby' {
            $exe = Find-KettlePerfExecutable -Explicit $TabbyExe `
                -EnvironmentVariable 'KETTLE_PERF_TABBY_EXE' `
                -Candidates @(
                    'Tabby.exe',
                    (Join-Path $env:LOCALAPPDATA 'Programs\Tabby\Tabby.exe'),
                    (Join-Path $env:ProgramFiles 'Tabby\Tabby.exe')
            )
            $startup = @()
            $command = @('run')
            # Tabby intentionally confirms `run` before starting the requested
            # command. Command probes accept only this known native dialog,
            # outside their timed regions, after proving it belongs to a newly
            # spawned Tabby process.
            $supportsCommand = $true
            $commandConfirmation = 'tabby-run'
            $windowProcessNames = @('Tabby')
            $commandShell = (Resolve-Path -LiteralPath $env:ComSpec).Path
            $commandPowerShell = (Resolve-Path -LiteralPath (
                Join-Path $env:SystemRoot (
                    'System32\WindowsPowerShell\v1.0\powershell.exe'
                )
            )).Path
            $helperBinaries = @(
                [ordered]@{
                    role = 'command-shell'
                    path = $commandShell
                    sha256 = Get-KettlePerfExecutableSha256 $commandShell
                },
                [ordered]@{
                    role = 'command-launcher'
                    path = $commandPowerShell
                    sha256 = Get-KettlePerfExecutableSha256 $commandPowerShell
                }
            )
        }
    }

    if ($null -ne $IsolatedConfig) {
        if (
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                [string]$IsolatedConfig.name,
                $Name
            )
        ) {
            throw "Isolated config for $($IsolatedConfig.name) cannot configure $Name"
        }
        $configFile = [string]$IsolatedConfig.config_file
        $evidence = $IsolatedConfig.evidence
        if (
            -not $configFile -or
            -not (Test-Path -LiteralPath $configFile -PathType Leaf) -or
            $null -eq $evidence
        ) {
            throw "$Name isolated benchmark config is missing its file evidence"
        }
        $configFile = (Resolve-Path -LiteralPath $configFile).Path
        if (-not [StringComparer]::OrdinalIgnoreCase.Equals(
            [string]$evidence.path,
            $configFile
        )) {
            throw "$Name isolated benchmark config path differs from its evidence"
        }
        $item = Get-Item -LiteralPath $configFile -Force
        $actualHash = (Get-FileHash -LiteralPath $configFile -Algorithm SHA256).Hash
        if (
            ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
            [long]$evidence.bytes -ne $item.Length -or
            -not [StringComparer]::OrdinalIgnoreCase.Equals(
                [string]$evidence.sha256,
                $actualHash
            )
        ) {
            throw "$Name isolated benchmark config changed after generation"
        }
        $configArgs = [string[]]@($IsolatedConfig.arguments)
        if ($configArgs.Count -gt 16) {
            throw "$Name isolated benchmark config has too many launch arguments"
        }
        foreach ($configArg in $configArgs) {
            if ($null -eq $configArg -or $configArg.Contains([char]0)) {
                throw "$Name isolated benchmark config has an invalid launch argument"
            }
        }
        if (
            $null -ne $IsolatedConfig.environment -and
            $IsolatedConfig.environment -isnot [System.Collections.IDictionary]
        ) {
            throw "$Name isolated benchmark environment is not a dictionary"
        }
        if ($null -ne $IsolatedConfig.environment) {
            foreach ($entry in $IsolatedConfig.environment.GetEnumerator()) {
                $launchEnvironment[[string]$entry.Key] = [string]$entry.Value
            }
        }
        $startup = @($configArgs) + @($startup)
        $command = @($configArgs) + @($command)
        $configurationMode = 'benchmark-isolated'
        $configurationEvidence = $evidence
    } elseif ($Name -eq 'kettle' -and $KettleConfig) {
        $configurationMode = 'explicit'
        $configurationEvidence = [ordered]@{
            path = (Resolve-Path -LiteralPath $KettleConfig).Path
            bytes = (Get-Item -LiteralPath $KettleConfig).Length
            sha256 = (
                Get-FileHash -LiteralPath $KettleConfig -Algorithm SHA256
            ).Hash
        }
    }

    $benchmarkExe = $exe
    if ($Name -eq 'wt' -and $exe) {
        $package = Get-AppxPackage -Name Microsoft.WindowsTerminal `
            -ErrorAction Stop |
            Sort-Object Version -Descending |
            Select-Object -First 1
        if (-not $package) {
            throw 'Windows Terminal package metadata was not found'
        }
        $hostCandidate = Join-Path $package.InstallLocation (
            'WindowsTerminal.exe'
        )
        if (-not (Test-Path -LiteralPath $hostCandidate -PathType Leaf)) {
            throw "Windows Terminal hosted executable not found: $hostCandidate"
        }
        $benchmarkExe = (Resolve-Path -LiteralPath $hostCandidate).Path
    }
    $cliExe = if ($name -eq 'kettle' -and $exe) {
        Get-KettlePerfCliExecutable -GuiExecutable $exe
    } else {
        $exe
    }
    $hasReliableCli = if ($name -eq 'kettle' -and $cliExe) {
        [IO.Path]::GetFileName($cliExe) -in @('kettle-console.exe', 'kettle.com')
    } else {
        [bool]$cliExe
    }
    if (
        $Name -eq 'kettle' -and
        $hasReliableCli -and
        -not [StringComparer]::OrdinalIgnoreCase.Equals(
            $cliExe,
            $benchmarkExe
        )
    ) {
        $helperBinaries += [ordered]@{
            role = 'kettle-cli'
            path = $cliExe
            sha256 = Get-KettlePerfExecutableSha256 $cliExe
        }
    }

    [pscustomobject]@{
        Name = $Name
        Exe = $exe
        BenchmarkExe = $benchmarkExe
        BenchmarkExeSha256 = if ($benchmarkExe) {
            Get-KettlePerfExecutableSha256 $benchmarkExe
        } else {
            $null
        }
        Available = [bool]$exe
        StartupArgs = @($startup)
        CommandPrefix = @($command)
        SupportsCommand = $supportsCommand
        CommandConfirmation = $commandConfirmation
        ProcessName = if ($exe) { [IO.Path]::GetFileNameWithoutExtension($exe) } else { $null }
        WindowProcessNames = @($windowProcessNames)
        CliExe = $cliExe
        # Windows Terminal is launched through an App Execution Alias reparse
        # point. The window owner is verified against BenchmarkExe instead;
        # hashing the mutable alias would misstate it as the hosted binary.
        CliExeSha256 = if ($cliExe -and $Name -ne 'wt') {
            Get-KettlePerfExecutableSha256 $cliExe
        } else {
            $null
        }
        HasReliableCli = $hasReliableCli
        CommandShell = $commandShell
        CommandPowerShell = $commandPowerShell
        # Windows PowerShell 5.1 serializes an untyped empty value stored in
        # an ordered dictionary as `{}`. Preserve the collection shape so
        # provenance JSON always contains an array.
        HelperBinaries = [object[]]@($helperBinaries)
        Environment = $launchEnvironment
        ConfigurationMode = $configurationMode
        ConfigurationEvidence = $configurationEvidence
    }
}

function Assert-KettlePerfTerminalSpecs {
    $known = @('kettle', 'wt', 'alacritty', 'wezterm', 'rio', 'tabby')
    foreach ($name in $known) {
        $spec = Resolve-KettlePerfTerminal -Name $name
        if ($spec.Name -ne $name) {
            throw "terminal spec name drifted for $name"
        }
        if (
            $name -eq 'tabby' -and (
                -not $spec.SupportsCommand -or
                $spec.CommandConfirmation -ne 'tabby-run' -or
                ($spec.CommandPrefix -join "`0") -ne 'run'
            )
        ) {
            throw 'Tabby command workloads must retain their bounded confirmation contract'
        }
    }
    $kettle = Resolve-KettlePerfTerminal -Name kettle
    if (
        ($kettle.StartupArgs -join "`0") -ne '--new-process' -or
        ($kettle.CommandPrefix -join "`0") -ne "--new-process`0-e"
    ) {
        throw 'Kettle benchmark launches must stay isolated from an existing primary process'
    }
    $windowsTerminal = Resolve-KettlePerfTerminal -Name wt
    if (($windowsTerminal.StartupArgs -join "`0") -ne "-w`0new") {
        throw 'Windows Terminal startup benchmarks must request a new top-level window'
    }
    $wezterm = Resolve-KettlePerfTerminal -Name wezterm
    if (
        ($wezterm.StartupArgs -join "`0") -ne "start`0--always-new-process" -or
        ($wezterm.CommandPrefix -join "`0") -ne "start`0--always-new-process`0--"
    ) {
        throw 'WezTerm benchmark launches must bypass an existing mux server'
    }
}
