[Diagnostics.CodeAnalysis.SuppressMessageAttribute(
    'PSAvoidUsingWriteHost',
    '',
    Justification = 'The self-test reports its explicit pass result.'
)]
param()

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\json-io.ps1"

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$targetRoot = Join-Path $repoRoot 'target'
New-Item -ItemType Directory -Force -Path $targetRoot | Out-Null
$scratch = Join-Path $targetRoot (
    'perf-sanitize-self-test-' + [Guid]::NewGuid().ToString('N')
)
$private = Join-Path $scratch 'private'
$public = Join-Path $scratch 'public'
$publicSecond = Join-Path $scratch 'public-second'
New-Item -ItemType Directory -Path $private | Out-Null

function Invoke-KettlePerfExpectedSanitizeFailure {
    param(
        [Parameter(Mandatory)]
        [scriptblock]$Action,
        [Parameter(Mandatory)]
        [string]$Description,
        [string]$ExpectedMessagePattern = ''
    )

    try {
        & $Action
    } catch {
        if (
            $ExpectedMessagePattern -and
            $_.Exception.Message -notmatch $ExpectedMessagePattern
        ) {
            throw (
                "Sanitizer rejection for '$Description' had an " +
                "unexpected message: $($_.Exception.Message)"
            )
        }
        return
    }
    throw "Expected sanitizer rejection did not occur: $Description"
}

try {
    $runId = [Guid]::NewGuid().ToString('D')
    Write-KettlePerfJsonFile `
        -Path (Join-Path $private 'benchmark-manifest.json') `
        -InputObject ([ordered]@{
            schema_version = 2
            run_id = $runId
            repository_commit = ('a' * 40)
            kettle_config = 'C:\Users\example\secret\config'
            kettle_config_sha256 = ('b' * 64)
            terminals = @(
                [ordered]@{
                    name = 'kettle'
                    launcher = 'C:\Users\example\bin\kettle.exe'
                    executable_sha256 = ('c' * 64)
                }
            )
            machine = [ordered]@{
                model = 'Surface Book 3'
                computer_name = 'WORKSTATION-SECRET'
                hostname = 'secret-host.example'
                user_name = 'private-user'
                username = 'other-private-user'
                display_topology = [ordered]@{
                    target_monitor_hardware_id = 'ABC123-PRIVATE'
                    desktop_screens = @(
                        [ordered]@{
                            device_name = '\\.\DISPLAY1'
                            monitor_device_id = 'MONITOR\ABC123\secret'
                            monitor_hardware_id = 'ABC123-PRIVATE'
                        }
                    )
                    active_physical_monitors = @(
                        [ordered]@{
                            hardware_id = 'ABC123-PRIVATE'
                            friendly_name = 'Example Display'
                            serial_number = 'SERIAL-SECRET'
                            instance_name = 'DISPLAY\ABC123\secret'
                            registry_edid_sha256 = ('d' * 64)
                            adapter_luid = 'deadbeef:01234567'
                            source_id = [int64]4294967294
                            target_id = 525252.25
                            connector_instance = $true
                        }
                    )
                }
            }
            sensitive_redaction_fixtures = [ordered]@{
                scalar = [ordered]@{
                    hardware_id = 'ABC123-PRIVATE'
                    adapter_luid = 'deadbeef:01234567'
                    source_id = [int64]4294967294
                    target_id = 525252.25
                    connector_instance = $true
                    registry_edid_sha256 = ('d' * 64)
                }
                type_domains = [ordered]@{
                    integer = [ordered]@{ source_id = 1 }
                    integer_string = [ordered]@{ source_id = '1' }
                    boolean = [ordered]@{ target_id = $true }
                    boolean_string = [ordered]@{ target_id = 'true' }
                }
                complex = [ordered]@{
                    object = [ordered]@{
                        hardware_id = [ordered]@{
                            private_device = 'COMPLEX-DEVICE-SECRET'
                            local_route = 987654321
                        }
                    }
                    array = [ordered]@{
                        target_id = @(
                            'ARRAY-DEVICE-SECRET',
                            876543210
                        )
                    }
                    null_value = [ordered]@{
                        connector_instance = $null
                    }
                }
            }
            key_redaction_fixtures = [ordered]@{
                api_token = 'CREDENTIAL-CORRELATION-PRIVATE'
                'api-key' = 'CREDENTIAL-CORRELATION-PRIVATE'
                ApiToken = 'CREDENTIAL-CORRELATION-PRIVATE'
                api_token_sha256 = ('f' * 64)
                PASSWORD = 'PASSWORD-VALUE-PRIVATE'
                password_hash = ('e' * 64)
                accessKeyId = 'ACCESS-KEY-PRIVATE'
                clientSecret = [ordered]@{
                    nested_value = 'SECRET-OBJECT-PRIVATE'
                    sequence = 616161
                }
                'refresh-token' = @(
                    'SECRET-ARRAY-PRIVATE',
                    717171
                )
                sessionToken = 818181
                authToken = $true
                nested = [ordered]@{
                    Authorization = 'Bearer AUTHORIZATION-PRIVATE'
                    COOKIE = 'COOKIE-VALUE-PRIVATE'
                    connectionString = (
                        'Server=private;Password=CONNECTION-PRIVATE'
                    )
                }
                metric_count = 12
            }
            hostile_strings = @(
                'prefix /mnt/c/Users/private/file.txt suffix'
                'under /opt/kettle/secrets/config.toml now'
                'workspace=/workspace/kettle/target/results.json'
                'home=/home/private/.config/kettle'
                'embedded C:\Users\example\private.txt after'
                (
                    'diagnostic C:\Users\Jane Doe\project secrets\' +
                    'trace.log after'
                )
                'temp=/var/tmp/private/result.json'
            )
        }) -Depth 12
    Write-KettlePerfJsonFile `
        -Path (Join-Path $private 'latency.json') `
        -InputObject ([ordered]@{
            kettle = [ordered]@{
                executable_sha256 = ('c' * 64)
                workload_command = (
                    'C:\Program Files\PowerShell\7\pwsh.exe -File ' +
                    'C:\Users\example\probe.ps1'
                )
                latency_ms_all = [double[]]@(10.0, 11.0, 12.0)
            }
        }) -Depth 8

    . "$PSScriptRoot\sanitize-results.ps1" `
        -ResultsDir $private -OutputDir $public
    foreach ($allowedName in @(
        'benchmark-manifest.json',
        'startup-idle.json',
        'latency.json',
        'vtebench-summary.json',
        'menu-hover.json',
        'native-display-menu-hover.json',
        'monitor-transition.json',
        'score.json',
        'throughput-kettle.json',
        'throughput-wt.json',
        'throughput-alacritty.json',
        'throughput-wezterm.json',
        'throughput-rio.json',
        'throughput-tabby.json'
    )) {
        if (-not (Test-KettlePerfPublicEvidenceSourceLeafName $allowedName)) {
            throw "Reviewed public evidence filename was rejected: $allowedName"
        }
    }
    foreach ($rejectedName in @(
        'Benchmark-manifest.json',
        'throughput-custom.json',
        'api-token-private.json'
    )) {
        if (Test-KettlePerfPublicEvidenceSourceLeafName $rejectedName) {
            throw "Unreviewed public evidence filename was accepted: $rejectedName"
        }
    }
    $combined = @(
        Get-ChildItem -LiteralPath $public -File |
            ForEach-Object { Get-Content -Raw -LiteralPath $_.FullName }
    ) -join "`n"
    foreach ($secret in @(
        'C:\Users\example',
        'SERIAL-SECRET',
        'DISPLAY\ABC123\secret',
        'MONITOR\ABC123\secret',
        'WORKSTATION-SECRET',
        'secret-host.example',
        'private-user',
        'other-private-user',
        'ABC123-PRIVATE',
        'deadbeef:01234567',
        ('d' * 64),
        'COMPLEX-DEVICE-SECRET',
        'ARRAY-DEVICE-SECRET',
        'CREDENTIAL-CORRELATION-PRIVATE',
        ('f' * 64),
        'PASSWORD-VALUE-PRIVATE',
        ('e' * 64),
        'ACCESS-KEY-PRIVATE',
        'SECRET-OBJECT-PRIVATE',
        'SECRET-ARRAY-PRIVATE',
        'AUTHORIZATION-PRIVATE',
        'COOKIE-VALUE-PRIVATE',
        'CONNECTION-PRIVATE',
        'Jane Doe',
        'project secrets',
        'trace.log after',
        '/mnt/c/Users/private/file.txt',
        '/opt/kettle/secrets/config.toml',
        '/workspace/kettle/target/results.json',
        '/home/private/.config/kettle',
        '/var/tmp/private/result.json'
    )) {
        if ($combined.Contains($secret)) {
            throw "Sanitized evidence retained secret fixture text: $secret"
        }
    }
    $sanitizedManifest = Get-Content -Raw -LiteralPath (
        Join-Path $public 'benchmark-manifest.json'
    ) | ConvertFrom-Json
    $fieldTokenPattern = '^<redacted-field:[0-9a-f]{16}>$'
    $publicDisplay = $sanitizedManifest.machine.display_topology
    $publicMonitor = $publicDisplay.active_physical_monitors[0]
    $publicScalar = $sanitizedManifest.sensitive_redaction_fixtures.scalar
    foreach ($value in @(
        $publicDisplay.target_monitor_hardware_id,
        $publicDisplay.desktop_screens[0].monitor_hardware_id,
        $publicMonitor.hardware_id,
        $publicMonitor.registry_edid_sha256,
        $publicMonitor.adapter_luid,
        $publicMonitor.source_id,
        $publicMonitor.target_id,
        $publicMonitor.connector_instance,
        $publicScalar.hardware_id,
        $publicScalar.adapter_luid,
        $publicScalar.source_id,
        $publicScalar.target_id,
        $publicScalar.connector_instance,
        $publicScalar.registry_edid_sha256
    )) {
        if ([string]$value -cnotmatch $fieldTokenPattern) {
            throw 'Sanitized evidence retained a sensitive scalar value'
        }
    }
    $hardwareTokens = @(
        @(
            [string]$publicDisplay.target_monitor_hardware_id,
            [string]$publicDisplay.desktop_screens[0].monitor_hardware_id,
            [string]$publicMonitor.hardware_id,
            [string]$publicScalar.hardware_id
        ) | Select-Object -Unique
    )
    if ($hardwareTokens.Count -ne 1) {
        throw 'Equivalent hardware identifiers did not retain token correlation'
    }
    $typeDomains = $sanitizedManifest.
        sensitive_redaction_fixtures.type_domains
    foreach ($value in @(
        $typeDomains.integer.source_id,
        $typeDomains.integer_string.source_id,
        $typeDomains.boolean.target_id,
        $typeDomains.boolean_string.target_id
    )) {
        if ([string]$value -cnotmatch $fieldTokenPattern) {
            throw 'Sanitized evidence retained a typed sensitive scalar'
        }
    }
    if (
        $typeDomains.integer.source_id -ceq
            $typeDomains.integer_string.source_id -or
        $typeDomains.boolean.target_id -ceq
            $typeDomains.boolean_string.target_id
    ) {
        throw 'Sensitive tokens were not separated by scalar type'
    }
    $complex = $sanitizedManifest.sensitive_redaction_fixtures.complex
    if (
        [string]$complex.object.hardware_id -cnotmatch $fieldTokenPattern -or
        [string]$complex.array.target_id -cnotmatch $fieldTokenPattern -or
        $null -ne $complex.null_value.connector_instance
    ) {
        throw 'Sanitized evidence did not safely handle complex or null fields'
    }
    $keyFixtures = $sanitizedManifest.key_redaction_fixtures
    $credentialTokens = @(
        $keyFixtures.api_token,
        $keyFixtures.PSObject.Properties['api-key'].Value,
        $keyFixtures.ApiToken,
        $keyFixtures.api_token_sha256,
        $keyFixtures.PASSWORD,
        $keyFixtures.password_hash,
        $keyFixtures.accessKeyId,
        $keyFixtures.clientSecret,
        $keyFixtures.PSObject.Properties['refresh-token'].Value,
        $keyFixtures.sessionToken,
        $keyFixtures.authToken,
        $keyFixtures.nested.Authorization,
        $keyFixtures.nested.COOKIE,
        $keyFixtures.nested.connectionString
    )
    foreach ($value in $credentialTokens) {
        if ([string]$value -cnotmatch $fieldTokenPattern) {
            throw 'Sanitized evidence retained a credential-like field'
        }
    }
    if (
        $keyFixtures.api_token -cne
            $keyFixtures.PSObject.Properties['api-key'].Value -or
        $keyFixtures.api_token -cne $keyFixtures.ApiToken -or
        $keyFixtures.metric_count -ne 12
    ) {
        throw (
            'Credential key normalization or safe non-credential ' +
            'preservation is invalid'
        )
    }
    if (
        $sanitizedManifest.repository_commit -ne ('a' * 40) -or
        $sanitizedManifest.kettle_config_sha256 -ne ('b' * 64) -or
        $sanitizedManifest.machine.model -ne 'Surface Book 3' -or
        $sanitizedManifest.machine.display_topology.
            active_physical_monitors[0].friendly_name -ne 'Example Display' -or
        [string]$sanitizedManifest.kettle_config -notmatch
            '^<redacted-path:[0-9a-f]{16}>$'
    ) {
        throw 'Sanitized evidence did not preserve safe facts and redact paths'
    }
    $publicIndex = Get-Content -Raw -LiteralPath (
        Join-Path $public 'public-evidence.json'
    ) | ConvertFrom-Json
    if (
        $publicIndex.schema_version -ne 2 -or
        $publicIndex.raw_artifacts_included -ne $false -or
        @($publicIndex.files).Count -ne 2
    ) {
        throw 'Public evidence index is incomplete'
    }
    & "$PSScriptRoot\sanitize-results.ps1" `
        -ResultsDir $private -OutputDir $publicSecond
    $secondManifest = Get-Content -Raw -LiteralPath (
        Join-Path $publicSecond 'benchmark-manifest.json'
    ) | ConvertFrom-Json
    $secondIndex = Get-Content -Raw -LiteralPath (
        Join-Path $publicSecond 'public-evidence.json'
    ) | ConvertFrom-Json
    $firstHardwareToken = [string]$publicDisplay.target_monitor_hardware_id
    $secondHardwareToken = [string](
        $secondManifest.machine.display_topology.target_monitor_hardware_id
    )
    if (
        $secondIndex.run_id -cne $publicIndex.run_id -or
        $secondHardwareToken -cnotmatch $fieldTokenPattern -or
        $secondHardwareToken -ceq $firstHardwareToken -or
        $secondHardwareToken -cne [string](
            $secondManifest.machine.display_topology.
                desktop_screens[0].monitor_hardware_id
        )
    ) {
        throw (
            'Redaction tokens were not bundle-secret, unpredictable, and ' +
            'internally correlated'
        )
    }

    Invoke-KettlePerfExpectedSanitizeFailure `
        -Description 'preexisting output directory' `
        -Action {
            & "$PSScriptRoot\sanitize-results.ps1" `
                -ResultsDir $private -OutputDir $public
        }

    $preexisting = Join-Path $scratch 'preexisting'
    New-Item -ItemType Directory -Path $preexisting | Out-Null
    $sentinel = Join-Path $preexisting 'sentinel.txt'
    [IO.File]::WriteAllText($sentinel, 'retain me')
    Invoke-KettlePerfExpectedSanitizeFailure `
        -Description 'nonempty preexisting output directory' `
        -Action {
            & "$PSScriptRoot\sanitize-results.ps1" `
                -ResultsDir $private -OutputDir $preexisting
        }
    if ((Get-Content -Raw -LiteralPath $sentinel) -ne 'retain me') {
        throw 'Sanitizer modified preexisting output data'
    }

    $hostileNamePrivate = Join-Path $scratch 'hostile-name-private'
    $hostileNameOutput = Join-Path $scratch 'hostile-name-public'
    New-Item -ItemType Directory -Path $hostileNamePrivate | Out-Null
    Copy-Item -LiteralPath (
        Join-Path $private 'benchmark-manifest.json'
    ) -Destination $hostileNamePrivate
    [IO.File]::WriteAllText(
        (
            Join-Path $hostileNamePrivate (
                'api-token-FILENAME-CREDENTIAL-PRIVATE.json'
            )
        ),
        '{}',
        [Text.UTF8Encoding]::new($false, $true)
    )
    Invoke-KettlePerfExpectedSanitizeFailure `
        -Description 'JSON leaf outside the public filename contract' `
        -ExpectedMessagePattern (
            '(?i)outside the reviewed public evidence filename contract'
        ) `
        -Action {
            & "$PSScriptRoot\sanitize-results.ps1" `
                -ResultsDir $hostileNamePrivate `
                -OutputDir $hostileNameOutput
        }
    if (Test-Path -LiteralPath $hostileNameOutput) {
        throw 'Hostile source filename was published'
    }

    $invalidUtf8Private = Join-Path $scratch 'invalid-utf8-private'
    New-Item -ItemType Directory -Path $invalidUtf8Private | Out-Null
    Copy-Item -LiteralPath (
        Join-Path $private 'benchmark-manifest.json'
    ) -Destination $invalidUtf8Private
    [IO.File]::WriteAllBytes(
        (Join-Path $invalidUtf8Private 'latency.json'),
        [byte[]]@(0x7B, 0x22, 0x78, 0x22, 0x3A, 0x22, 0xFF, 0x22, 0x7D)
    )
    $invalidUtf8Output = Join-Path $scratch 'invalid-utf8-public'
    Invoke-KettlePerfExpectedSanitizeFailure `
        -Description 'invalid UTF-8 JSON' `
        -Action {
            & "$PSScriptRoot\sanitize-results.ps1" `
                -ResultsDir $invalidUtf8Private `
                -OutputDir $invalidUtf8Output
        }
    if (Test-Path -LiteralPath $invalidUtf8Output) {
        throw 'Failed sanitizer run published a partial output directory'
    }
    if (
        @(Get-ChildItem -LiteralPath $scratch -Directory -Force |
            Where-Object Name -like '.kettle-sanitize-stage-*').Count -ne 0
    ) {
        throw 'Failed sanitizer run retained its staging directory'
    }

    $deepPrivate = Join-Path $scratch 'deep-private'
    $deepOutput = Join-Path $scratch 'deep-public'
    New-Item -ItemType Directory -Path $deepPrivate | Out-Null
    Copy-Item -LiteralPath (
        Join-Path $private 'benchmark-manifest.json'
    ) -Destination $deepPrivate
    $deepText = ('{"nested":' * 33) + '0' + ('}' * 33)
    [IO.File]::WriteAllText(
        (Join-Path $deepPrivate 'latency.json'),
        $deepText,
        [Text.UTF8Encoding]::new($false, $true)
    )
    Invoke-KettlePerfExpectedSanitizeFailure `
        -Description 'pre-parse JSON depth bound' `
        -ExpectedMessagePattern '(?i)depth bound' `
        -Action {
            & "$PSScriptRoot\sanitize-results.ps1" `
                -ResultsDir $deepPrivate -OutputDir $deepOutput
        }
    if (Test-Path -LiteralPath $deepOutput) {
        throw 'Depth-bound rejection published a partial bundle'
    }

    $widePrivate = Join-Path $scratch 'wide-private'
    $wideOutput = Join-Path $scratch 'wide-public'
    New-Item -ItemType Directory -Path $widePrivate | Out-Null
    Copy-Item -LiteralPath (
        Join-Path $private 'benchmark-manifest.json'
    ) -Destination $widePrivate
    $wideText = '[' + ('0,' * 249999) + '0]'
    [IO.File]::WriteAllText(
        (Join-Path $widePrivate 'latency.json'),
        $wideText,
        [Text.UTF8Encoding]::new($false, $true)
    )
    Invoke-KettlePerfExpectedSanitizeFailure `
        -Description 'pre-parse cumulative JSON node bound' `
        -ExpectedMessagePattern '(?i)node bound' `
        -Action {
            & "$PSScriptRoot\sanitize-results.ps1" `
                -ResultsDir $widePrivate -OutputDir $wideOutput
        }
    if (Test-Path -LiteralPath $wideOutput) {
        throw 'Node-bound rejection published a partial bundle'
    }

    $fileCountPrivate = Join-Path $scratch 'file-count-private'
    $fileCountOutput = Join-Path $scratch 'file-count-public'
    New-Item -ItemType Directory -Path $fileCountPrivate | Out-Null
    Copy-Item -LiteralPath (
        Join-Path $private 'benchmark-manifest.json'
    ) -Destination $fileCountPrivate
    foreach ($index in 1..100) {
        [IO.File]::WriteAllText(
            (
                Join-Path $fileCountPrivate (
                    'extra-{0:d3}.json' -f $index
                )
            ),
            '{}',
            [Text.UTF8Encoding]::new($false, $true)
        )
    }
    Invoke-KettlePerfExpectedSanitizeFailure `
        -Description 'bounded enumeration before sorting' `
        -ExpectedMessagePattern '(?i)file-count bound' `
        -Action {
            & "$PSScriptRoot\sanitize-results.ps1" `
                -ResultsDir $fileCountPrivate `
                -OutputDir $fileCountOutput
        }
    if (Test-Path -LiteralPath $fileCountOutput) {
        throw 'File-count rejection published a partial bundle'
    }

    if ($env:OS -eq 'Windows_NT') {
        $preplacedPrivate = Join-Path $scratch 'preplaced-private'
        $preplacedVictim = Join-Path $scratch 'preplaced-victim'
        $preplacedOutput = Join-Path $scratch 'preplaced-public'
        New-Item -ItemType Directory -Path $preplacedPrivate | Out-Null
        New-Item -ItemType Directory -Path $preplacedVictim | Out-Null
        Copy-Item -LiteralPath (
            Join-Path $private 'benchmark-manifest.json'
        ) -Destination $preplacedPrivate
        $preplacedSentinel = Join-Path $preplacedVictim 'sentinel.txt'
        [IO.File]::WriteAllText($preplacedSentinel, 'retain me')
        $preplacedLeaf = Join-Path $preplacedPrivate 'latency.json'
        New-Item -ItemType Junction -Path $preplacedLeaf `
            -Target $preplacedVictim -ErrorAction Stop | Out-Null
        try {
            Invoke-KettlePerfExpectedSanitizeFailure `
                -Description 'preplaced source reparse leaf' `
                -ExpectedMessagePattern '(?i)(ordinary|opening)' `
                -Action {
                    & "$PSScriptRoot\sanitize-results.ps1" `
                        -ResultsDir $preplacedPrivate `
                        -OutputDir $preplacedOutput
                }
            if (
                (Get-Content -Raw -LiteralPath $preplacedSentinel) -ne
                    'retain me' -or
                (Test-Path -LiteralPath $preplacedOutput)
            ) {
                throw 'Preplaced source reparse rejection modified external data'
            }
        } finally {
            [IO.Directory]::Delete($preplacedLeaf, $false)
        }

        $oversizePrivate = Join-Path $scratch 'oversize-private'
        $oversizeOutput = Join-Path $scratch 'oversize-public'
        New-Item -ItemType Directory -Path $oversizePrivate | Out-Null
        Copy-Item -LiteralPath (
            Join-Path $private 'benchmark-manifest.json'
        ) -Destination $oversizePrivate
        $oversizeStream = [IO.FileStream]::new(
            (Join-Path $oversizePrivate 'latency.json'),
            [IO.FileMode]::CreateNew,
            [IO.FileAccess]::Write,
            [IO.FileShare]::None
        )
        try {
            $oversizeStream.SetLength(64MB + 1)
        } finally {
            $oversizeStream.Dispose()
        }
        Invoke-KettlePerfExpectedSanitizeFailure `
            -Description 'per-file bytes before read' `
            -ExpectedMessagePattern '(?i)size.*snapshot bound' `
            -Action {
                & "$PSScriptRoot\sanitize-results.ps1" `
                    -ResultsDir $oversizePrivate `
                    -OutputDir $oversizeOutput
            }
        if (Test-Path -LiteralPath $oversizeOutput) {
            throw 'Per-file byte rejection published a partial bundle'
        }

        $totalPrivate = Join-Path $scratch 'total-private'
        $totalOutput = Join-Path $scratch 'total-public'
        New-Item -ItemType Directory -Path $totalPrivate | Out-Null
        Copy-Item -LiteralPath (
            Join-Path $private 'benchmark-manifest.json'
        ) -Destination $totalPrivate
        foreach ($leaf in @('latency.json', 'startup-idle.json')) {
            $totalStream = [IO.FileStream]::new(
                (Join-Path $totalPrivate $leaf),
                [IO.FileMode]::CreateNew,
                [IO.FileAccess]::Write,
                [IO.FileShare]::None
            )
            try {
                $totalStream.SetLength(64MB)
            } finally {
                $totalStream.Dispose()
            }
        }
        Invoke-KettlePerfExpectedSanitizeFailure `
            -Description 'cumulative bytes before any source read' `
            -ExpectedMessagePattern '(?i)size.*snapshot bound' `
            -Action {
                & "$PSScriptRoot\sanitize-results.ps1" `
                    -ResultsDir $totalPrivate -OutputDir $totalOutput
            }
        if (Test-Path -LiteralPath $totalOutput) {
            throw 'Cumulative byte rejection published a partial bundle'
        }

        $sourceSwapPrivate = Join-Path $scratch 'source-swap-private'
        $sourceSwapOutput = Join-Path $scratch 'source-swap-public'
        $sourceSwapMoved = Join-Path $scratch 'source-swap-moved'
        New-Item -ItemType Directory -Path $sourceSwapPrivate | Out-Null
        Copy-Item -LiteralPath (
            Join-Path $private 'benchmark-manifest.json'
        ) -Destination $sourceSwapPrivate
        Copy-Item -LiteralPath (
            Join-Path $private 'latency.json'
        ) -Destination $sourceSwapPrivate
        $sourceSwapFile = Join-Path $sourceSwapPrivate 'latency.json'
        $sourceSwapReplacement = Join-Path (
            $sourceSwapPrivate
        ) 'replacement.tmp'
        [IO.File]::WriteAllText(
            $sourceSwapReplacement,
            '{"generation":2}',
            [Text.UTF8Encoding]::new($false, $true)
        )
        $sourceSwapHook = {
            $writeBlocked = $false
            try {
                [IO.File]::WriteAllText(
                    $sourceSwapFile,
                    '{"generation":2}',
                    [Text.UTF8Encoding]::new($false, $true)
                )
            } catch {
                $writeBlocked = $true
            }
            if (-not $writeBlocked) {
                throw 'Retained source snapshot allowed a file write'
            }

            $replaceBlocked = $false
            try {
                [IO.File]::Replace(
                    $sourceSwapReplacement,
                    $sourceSwapFile,
                    $null
                )
            } catch {
                $replaceBlocked = $true
            }
            if (-not $replaceBlocked) {
                throw 'Retained source snapshot allowed a file replacement'
            }

            $rootMoveBlocked = $false
            try {
                [IO.Directory]::Move(
                    $sourceSwapPrivate,
                    $sourceSwapMoved
                )
            } catch {
                $rootMoveBlocked = $true
            }
            if (-not $rootMoveBlocked) {
                throw 'Retained source snapshot allowed a root move'
            }
        }.GetNewClosure()
        & "$PSScriptRoot\sanitize-results.ps1" `
            -ResultsDir $sourceSwapPrivate `
            -OutputDir $sourceSwapOutput `
            -BeforePublishSourceTestAction $sourceSwapHook
        if (
            -not (Test-Path -LiteralPath $sourceSwapFile -PathType Leaf) -or
            (Test-Path -LiteralPath $sourceSwapMoved) -or
            -not (Test-Path -LiteralPath $sourceSwapOutput -PathType Container)
        ) {
            throw 'Source swap regression did not preserve the held snapshot'
        }
        $sourceSwapPublished = Get-Content -Raw -LiteralPath (
            Join-Path $sourceSwapOutput 'latency.json'
        ) | ConvertFrom-Json
        if (
            $sourceSwapPublished.kettle.latency_ms_all[0] -ne 10.0 -or
            (
                Get-Content -Raw -LiteralPath $sourceSwapFile
            ) -match '"generation"'
        ) {
            throw 'Source swap regression published mixed source generations'
        }
        if (Test-Path -LiteralPath $sourceSwapReplacement) {
            [IO.File]::Delete($sourceSwapReplacement)
        }

        $trailingDotOutput = Join-Path $scratch 'trailing-dot.'
        Invoke-KettlePerfExpectedSanitizeFailure `
            -Description 'trailing-dot output alias' `
            -Action {
                & "$PSScriptRoot\sanitize-results.ps1" `
                    -ResultsDir $private -OutputDir $trailingDotOutput
            }

        $adsPath = $private + ':hostile'
        $adsOutput = Join-Path $scratch 'ads-public'
        Invoke-KettlePerfExpectedSanitizeFailure `
            -Description 'alternate data stream source alias' `
            -Action {
                & "$PSScriptRoot\sanitize-results.ps1" `
                    -ResultsDir $adsPath -OutputDir $adsOutput
            }

        $junctionRoot = Join-Path $scratch 'private-junction'
        New-Item -ItemType Junction -Path $junctionRoot `
            -Target $private -ErrorAction Stop | Out-Null
        $junctionOutput = Join-Path $scratch 'junction-public'
        Invoke-KettlePerfExpectedSanitizeFailure `
            -Description 'reparse-point source directory' `
            -Action {
                & "$PSScriptRoot\sanitize-results.ps1" `
                    -ResultsDir $junctionRoot -OutputDir $junctionOutput
            }

        $realOutputParent = Join-Path $scratch 'real-output-parent'
        $outputParentJunction = Join-Path $scratch 'output-parent-junction'
        New-Item -ItemType Directory -Path $realOutputParent | Out-Null
        New-Item -ItemType Junction -Path $outputParentJunction `
            -Target $realOutputParent -ErrorAction Stop | Out-Null
        Invoke-KettlePerfExpectedSanitizeFailure `
            -Description 'reparse-point output ancestor' `
            -Action {
                & "$PSScriptRoot\sanitize-results.ps1" `
                    -ResultsDir $private `
                    -OutputDir (Join-Path $outputParentJunction 'public')
            }

        $leaseParent = Join-Path $scratch 'lease-parent'
        New-Item -ItemType Directory -Path $leaseParent | Out-Null

        $lockedStageLeaf = (
            '.kettle-sanitize-stage-' +
            [Guid]::NewGuid().ToString('N')
        )
        $lockedStagePath = Join-Path $leaseParent $lockedStageLeaf
        $renamedStagePath = Join-Path $leaseParent 'attacker-renamed-stage'
        $lockedStageLease = [KettlePerfSanitize.StageLease]::Create(
            $leaseParent,
            $lockedStageLeaf
        )
        try {
            Invoke-KettlePerfExpectedSanitizeFailure `
                -Description 'rename of a held stage root' `
                -Action {
                    [IO.Directory]::Move(
                        $lockedStagePath,
                        $renamedStagePath
                    )
                }
            $lockedStageLease.VerifyCurrentPath()
            $lockedStageLease.DeleteEmptyDirectory()
        } finally {
            $lockedStageLease.Dispose()
        }
        if (
            (Test-Path -LiteralPath $lockedStagePath) -or
            (Test-Path -LiteralPath $renamedStagePath)
        ) {
            throw 'Held stage identity was renamed or retained after deletion'
        }

        $collisionStageLeaf = (
            '.kettle-sanitize-stage-' +
            [Guid]::NewGuid().ToString('N')
        )
        $collisionStagePath = Join-Path $leaseParent $collisionStageLeaf
        $collisionOutput = Join-Path $leaseParent 'collision-output'
        New-Item -ItemType Directory -Path $collisionOutput | Out-Null
        $collisionLease = [KettlePerfSanitize.StageLease]::Create(
            $leaseParent,
            $collisionStageLeaf
        )
        try {
            Invoke-KettlePerfExpectedSanitizeFailure `
                -Description 'held publication over an existing output' `
                -Action {
                    $collisionLease.MoveTo($collisionOutput)
                }
            $collisionLease.VerifyCurrentPath()
            if (
                -not (
                    Test-KettlePerfSameSanitizePath `
                        -Left $collisionLease.CurrentPath `
                        -Right $collisionStagePath
                )
            ) {
                throw 'Failed publication changed the held stage path'
            }
            $collisionLease.DeleteEmptyDirectory()
        } finally {
            $collisionLease.Dispose()
            [IO.Directory]::Delete($collisionOutput, $false)
        }

        $flatStageLeaf = (
            '.kettle-sanitize-stage-' +
            [Guid]::NewGuid().ToString('N')
        )
        $flatStagePath = Join-Path $leaseParent $flatStageLeaf
        $flatStageLease = [KettlePerfSanitize.StageLease]::Create(
            $leaseParent,
            $flatStageLeaf
        )
        $expectedFlatFile = Join-Path $flatStagePath 'expected.json'
        $unexpectedDirectory = Join-Path $flatStagePath 'unexpected'
        [IO.File]::WriteAllText(
            $expectedFlatFile,
            '{}',
            [Text.UTF8Encoding]::new($false)
        )
        New-Item -ItemType Directory -Path $unexpectedDirectory |
            Out-Null
        $unexpectedSentinel = Join-Path $unexpectedDirectory 'sentinel.txt'
        [IO.File]::WriteAllText($unexpectedSentinel, 'retain me')
        try {
            Invoke-KettlePerfExpectedSanitizeFailure `
                -Description 'unexpected staged child directory' `
                -Action {
                    Remove-KettlePerfSanitizeStage `
                        -Stage $flatStagePath `
                        -Parent $leaseParent `
                        -ExpectedNames @('expected.json') `
                        -PublicationPath (
                            Join-Path $leaseParent 'unused-public'
                        ) `
                        -Lease $flatStageLease
                }
            if (
                -not (Test-Path -LiteralPath $expectedFlatFile) -or
                (Get-Content -Raw -LiteralPath $unexpectedSentinel) -ne
                    'retain me'
            ) {
                throw 'Rejected flat cleanup modified staged child data'
            }
        } finally {
            $flatStageLease.Dispose()
            [IO.File]::Delete($expectedFlatFile)
            [IO.File]::Delete($unexpectedSentinel)
            [IO.Directory]::Delete($unexpectedDirectory, $false)
            [IO.Directory]::Delete($flatStagePath, $false)
        }

        $exactStageLeaf = (
            '.kettle-sanitize-stage-' +
            [Guid]::NewGuid().ToString('N')
        )
        $exactStagePath = Join-Path $leaseParent $exactStageLeaf
        $exactStageLease = [KettlePerfSanitize.StageLease]::Create(
            $leaseParent,
            $exactStageLeaf
        )
        [IO.File]::WriteAllText(
            (Join-Path $exactStagePath 'one.json'),
            '{}',
            [Text.UTF8Encoding]::new($false)
        )
        [IO.File]::WriteAllText(
            (Join-Path $exactStagePath 'two.json'),
            '{}',
            [Text.UTF8Encoding]::new($false)
        )
        try {
            Remove-KettlePerfSanitizeStage `
                -Stage $exactStagePath `
                -Parent $leaseParent `
                -ExpectedNames @('one.json', 'two.json') `
                -PublicationPath (
                    Join-Path $leaseParent 'unused-public'
                ) `
                -Lease $exactStageLease
        } finally {
            $exactStageLease.Dispose()
        }
        if (Test-Path -LiteralPath $exactStagePath) {
            throw 'Exact flat cleanup retained its empty stage directory'
        }

        $createStageLeaf = (
            '.kettle-sanitize-stage-' +
            [Guid]::NewGuid().ToString('N')
        )
        $createStagePath = Join-Path $leaseParent $createStageLeaf
        $createStageLease = [KettlePerfSanitize.StageLease]::Create(
            $leaseParent,
            $createStageLeaf
        )
        $preexistingChild = Join-Path $createStagePath 'expected.json'
        [IO.File]::WriteAllText(
            $preexistingChild,
            'retain me',
            [Text.UTF8Encoding]::new($false)
        )
        try {
            Invoke-KettlePerfExpectedSanitizeFailure `
                -Description 'preexisting staged destination leaf' `
                -Action {
                    $createStageLease.WriteNewRegularFile(
                        'expected.json',
                        [Text.Encoding]::UTF8.GetBytes('{}')
                    )
                }
            if (
                [IO.File]::ReadAllText($preexistingChild) -ne
                    'retain me'
            ) {
                throw 'CreateNew stage write modified a preexisting leaf'
            }
        } finally {
            $createStageLease.Dispose()
            [IO.File]::Delete($preexistingChild)
            [IO.Directory]::Delete($createStagePath, $false)
        }

        $tamperStageLeaf = (
            '.kettle-sanitize-stage-' +
            [Guid]::NewGuid().ToString('N')
        )
        $tamperStagePath = Join-Path $leaseParent $tamperStageLeaf
        $tamperStageLease = [KettlePerfSanitize.StageLease]::Create(
            $leaseParent,
            $tamperStageLeaf
        )
        $expectedTamperHash = $tamperStageLease.WriteNewRegularFile(
            'expected.json',
            [Text.Encoding]::UTF8.GetBytes('{}')
        )
        $tamperFile = Join-Path $tamperStagePath 'expected.json'
        [IO.File]::WriteAllText(
            $tamperFile,
            '{"tampered":true}',
            [Text.UTF8Encoding]::new($false)
        )
        $tamperHeldFile = $null
        try {
            $tamperHeldFile = $tamperStageLease.HoldRegularFile(
                $tamperFile
            )
            if ($tamperHeldFile.GetSha256() -eq $expectedTamperHash) {
                throw 'Held stage hash did not detect child tampering'
            }
        } finally {
            if ($null -ne $tamperHeldFile) {
                $tamperHeldFile.Dispose()
            }
            Remove-KettlePerfSanitizeStage `
                -Stage $tamperStagePath `
                -Parent $leaseParent `
                -ExpectedNames @('expected.json') `
                -PublicationPath (
                    Join-Path $leaseParent 'unused-public'
                ) `
                -Lease $tamperStageLease
            $tamperStageLease.Dispose()
        }

        $commitStageLeaf = (
            '.kettle-sanitize-stage-' +
            [Guid]::NewGuid().ToString('N')
        )
        $commitStagePath = Join-Path $leaseParent $commitStageLeaf
        $commitOutput = Join-Path $leaseParent 'commit-output'
        $commitLease = [KettlePerfSanitize.StageLease]::Create(
            $leaseParent,
            $commitStageLeaf
        )
        $commitHash = $commitLease.WriteNewRegularFile(
            'expected.json',
            [Text.Encoding]::UTF8.GetBytes('{}')
        )
        $commitFile = Join-Path $commitStagePath 'expected.json'
        try {
            Invoke-KettlePerfExpectedSanitizeFailure `
                -Description 'close-to-rename child mutation' `
                -Action {
                    Publish-KettlePerfSanitizeStage `
                        -Stage $commitStagePath `
                        -Output $commitOutput `
                        -ExpectedNames @('expected.json') `
                        -ExpectedHashes @{
                            'expected.json' = $commitHash
                        } `
                        -Lease $commitLease `
                        -BeforeMoveTestAction {
                            [IO.File]::WriteAllText(
                                $commitFile,
                                '{"tampered":true}',
                                [Text.UTF8Encoding]::new($false)
                            )
                        }
                }
            if (
                (Test-Path -LiteralPath $commitOutput) -or
                -not (
                    Test-KettlePerfSanitizePathWithin `
                        -Path $commitLease.CurrentPath `
                        -Root $leaseParent
                ) -or
                (Split-Path -Leaf $commitLease.CurrentPath) -notmatch
                    '^\.kettle-sanitize-stage-[0-9a-f]{32}$'
            ) {
                throw 'Failed publication left a tampered public output'
            }
            Remove-KettlePerfSanitizeStage `
                -Stage $commitLease.CurrentPath `
                -Parent $leaseParent `
                -ExpectedNames @('expected.json') `
                -PublicationPath $commitOutput `
                -Lease $commitLease
        } finally {
            $commitLease.Dispose()
        }

        $rootSwapStageLeaf = (
            '.kettle-sanitize-stage-' +
            [Guid]::NewGuid().ToString('N')
        )
        $rootSwapStagePath = Join-Path (
            $leaseParent
        ) $rootSwapStageLeaf
        $rootSwapMovedPath = Join-Path $leaseParent 'root-swap-moved'
        $rootSwapOutput = Join-Path $leaseParent 'root-swap-output'
        $rootSwapLease = [KettlePerfSanitize.StageLease]::Create(
            $leaseParent,
            $rootSwapStageLeaf
        )
        $rootSwapHash = $rootSwapLease.WriteNewRegularFile(
            'expected.json',
            [Text.Encoding]::UTF8.GetBytes('{}')
        )
        try {
            Publish-KettlePerfSanitizeStage `
                -Stage $rootSwapStagePath `
                -Output $rootSwapOutput `
                -ExpectedNames @('expected.json') `
                -ExpectedHashes @{
                    'expected.json' = $rootSwapHash
                } `
                -Lease $rootSwapLease `
                -BeforeRootMoveTestAction {
                    Invoke-KettlePerfExpectedSanitizeFailure `
                        -Description 'root substitution during publication' `
                        -Action {
                            [IO.Directory]::Move(
                                $rootSwapStagePath,
                                $rootSwapMovedPath
                            )
                        }
                    if (
                        -not (Test-Path -LiteralPath $rootSwapStagePath) -or
                        (Test-Path -LiteralPath $rootSwapMovedPath)
                    ) {
                        throw 'Held stage root changed during swap attempt'
                    }
                }
            if (
                -not (Test-Path -LiteralPath (
                    Join-Path $rootSwapOutput 'expected.json'
                ) -PathType Leaf) -or
                (Test-Path -LiteralPath $rootSwapMovedPath) -or
                (Test-Path -LiteralPath $rootSwapStagePath)
            ) {
                throw 'Retained stage handle allowed root substitution'
            }
        } finally {
            $rootSwapLease.Dispose()
            [IO.File]::Delete(
                (Join-Path $rootSwapOutput 'expected.json')
            )
            [IO.Directory]::Delete($rootSwapOutput, $false)
        }

        $streamStageLeaf = (
            '.kettle-sanitize-stage-' +
            [Guid]::NewGuid().ToString('N')
        )
        $streamStagePath = Join-Path $leaseParent $streamStageLeaf
        $streamStageLease = [KettlePerfSanitize.StageLease]::Create(
            $leaseParent,
            $streamStageLeaf
        )
        $streamFile = Join-Path $streamStagePath 'expected.json'
        [IO.File]::WriteAllText(
            $streamFile,
            '{}',
            [Text.UTF8Encoding]::new($false)
        )
        Set-Content -LiteralPath $streamFile -Stream hidden `
            -Value 'retain me' -NoNewline
        try {
            Invoke-KettlePerfExpectedSanitizeFailure `
                -Description 'staged alternate data stream' `
                -Action {
                    Assert-KettlePerfSanitizeExactFileSet `
                        -Stage $streamStagePath `
                        -ExpectedNames @('expected.json') `
                        -Lease $streamStageLease
                }
            if (
                (Get-Content -Raw -LiteralPath $streamFile -Stream hidden) -ne
                    'retain me'
            ) {
                throw 'Alternate-stream rejection modified staged data'
            }
        } finally {
            $streamStageLease.Dispose()
            [IO.File]::Delete($streamFile)
            [IO.Directory]::Delete($streamStagePath, $false)
        }

        $junctionVictim = Join-Path $scratch 'junction-victim'
        New-Item -ItemType Directory -Path $junctionVictim | Out-Null
        $junctionSentinel = Join-Path $junctionVictim 'sentinel.txt'
        [IO.File]::WriteAllText($junctionSentinel, 'retain me')
        $swappedStage = Join-Path $leaseParent (
            '.kettle-sanitize-stage-' +
            [Guid]::NewGuid().ToString('N')
        )
        $junctionCreated = $false
        try {
            New-Item -ItemType Junction -Path $swappedStage `
                -Target $junctionVictim -ErrorAction Stop | Out-Null
            $junctionCreated = $true
        } catch {
            Write-Warning (
                'SKIP sanitizer root-junction regression: ' +
                $_.Exception.Message
            )
        }
        if ($junctionCreated) {
            try {
                Invoke-KettlePerfExpectedSanitizeFailure `
                    -Description 'swapped root junction cleanup' `
                    -Action {
                        Remove-KettlePerfSanitizeStage `
                            -Stage $swappedStage `
                            -Parent $leaseParent `
                            -ExpectedNames @() `
                            -PublicationPath (
                                Join-Path $leaseParent 'unused-public'
                            )
                    }
                if (
                    (Get-Content -Raw -LiteralPath $junctionSentinel) -ne
                        'retain me'
                ) {
                    throw 'Root-junction rejection modified external data'
                }
            } finally {
                [IO.Directory]::Delete($swappedStage, $false)
            }
        }

        $reparseLeafStageLeaf = (
            '.kettle-sanitize-stage-' +
            [Guid]::NewGuid().ToString('N')
        )
        $reparseLeafStagePath = Join-Path (
            $leaseParent
        ) $reparseLeafStageLeaf
        $reparseLeafLease = [KettlePerfSanitize.StageLease]::Create(
            $leaseParent,
            $reparseLeafStageLeaf
        )
        $reparseDestination = Join-Path (
            $reparseLeafStagePath
        ) 'expected.json'
        $reparseDestinationCreated = $false
        try {
            New-Item -ItemType Junction -Path $reparseDestination `
                -Target $junctionVictim -ErrorAction Stop | Out-Null
            $reparseDestinationCreated = $true
        } catch {
            Write-Warning (
                'SKIP sanitizer destination-junction regression: ' +
                $_.Exception.Message
            )
        }
        try {
            if ($reparseDestinationCreated) {
                Invoke-KettlePerfExpectedSanitizeFailure `
                    -Description 'preexisting staged destination junction' `
                    -Action {
                        $reparseLeafLease.WriteNewRegularFile(
                            'expected.json',
                            [Text.Encoding]::UTF8.GetBytes('{}')
                        )
                    }
                if (
                    (Get-Content -Raw -LiteralPath $junctionSentinel) -ne
                        'retain me'
                ) {
                    throw 'CreateNew stage write traversed a destination junction'
                }
            }
        } finally {
            $reparseLeafLease.Dispose()
            if ($reparseDestinationCreated) {
                [IO.Directory]::Delete($reparseDestination, $false)
            }
            [IO.Directory]::Delete($reparseLeafStagePath, $false)
        }

        $childReparseStageLeaf = (
            '.kettle-sanitize-stage-' +
            [Guid]::NewGuid().ToString('N')
        )
        $childReparseStagePath = Join-Path (
            $leaseParent
        ) $childReparseStageLeaf
        $childReparseLease = [KettlePerfSanitize.StageLease]::Create(
            $leaseParent,
            $childReparseStageLeaf
        )
        $childReparse = Join-Path $childReparseStagePath 'unexpected-link'
        $childJunctionCreated = $false
        try {
            New-Item -ItemType Junction -Path $childReparse `
                -Target $junctionVictim -ErrorAction Stop | Out-Null
            $childJunctionCreated = $true
        } catch {
            Write-Warning (
                'SKIP sanitizer child-junction regression: ' +
                $_.Exception.Message
            )
        }
        try {
            if ($childJunctionCreated) {
                Invoke-KettlePerfExpectedSanitizeFailure `
                    -Description 'staged child junction cleanup' `
                    -Action {
                        Remove-KettlePerfSanitizeStage `
                            -Stage $childReparseStagePath `
                            -Parent $leaseParent `
                            -ExpectedNames @() `
                            -PublicationPath (
                                Join-Path $leaseParent 'unused-public'
                            ) `
                            -Lease $childReparseLease
                    }
                if (
                    (Get-Content -Raw -LiteralPath $junctionSentinel) -ne
                        'retain me'
                ) {
                    throw 'Child-junction rejection modified external data'
                }
            }
        } finally {
            $childReparseLease.Dispose()
            if ($childJunctionCreated) {
                [IO.Directory]::Delete($childReparse, $false)
            }
            [IO.Directory]::Delete($childReparseStagePath, $false)
        }

        $lockedPrivate = Join-Path $scratch 'locked-private'
        New-Item -ItemType Directory -Path $lockedPrivate | Out-Null
        Copy-Item -LiteralPath (
            Join-Path $private 'benchmark-manifest.json'
        ) -Destination $lockedPrivate
        Copy-Item -LiteralPath (
            Join-Path $private 'latency.json'
        ) -Destination $lockedPrivate
        $lockedFile = Join-Path $lockedPrivate 'latency.json'
        $sharingHandle = [IO.FileStream]::new(
            $lockedFile,
            [IO.FileMode]::Open,
            [IO.FileAccess]::Write,
            [IO.FileShare]::ReadWrite
        )
        try {
            $lockedOutput = Join-Path $scratch 'locked-public'
            Invoke-KettlePerfExpectedSanitizeFailure `
                -Description 'source file with an active writer' `
                -Action {
                    & "$PSScriptRoot\sanitize-results.ps1" `
                        -ResultsDir $lockedPrivate -OutputDir $lockedOutput
                }
        } finally {
            $sharingHandle.Dispose()
        }
    }

    # Free text is the sanitizer's blind spot: it tokenizes by PROPERTY NAME,
    # so a machine-identifying value pasted into a message escapes untouched.
    # The display-topology probe used to interpolate a monitor's device
    # instance path into an `issues` string while the same value was tokenized
    # under its own `instance_name` property, and `issues` was published
    # verbatim. Guard the source: no message the probe builds may carry one.
    $win32Source = Get-Content -LiteralPath (
        Join-Path $PSScriptRoot 'lib-win32.ps1'
    ) -Raw
    $issueAdds = [regex]::Matches(
        $win32Source, '\[void\]\$issues\.Add\((?<body>[\s\S]*?)\r?\n\s*\)')
    if ($issueAdds.Count -lt 1) {
        throw (
            'no $issues.Add(...) sites found in lib-win32.ps1 — this guard ' +
            'is looking in the wrong place and cannot fail'
        )
    }
    foreach ($match in $issueAdds) {
        # Using the identity as a dictionary KEY is fine — what must never
        # reach the message is the identity's VALUE. Blank the lookup first so
        # the check is about interpolation, not about the word appearing.
        $body = $match.Groups['body'].Value -replace `
            '\$identityCounts\[\$identity\]', '<count>'
        if (
            $body -match '\$identity\b' -or
            $body -match 'instance_name' -or
            $body -match 'serial_number' -or
            $body -match 'monitor_device_id'
        ) {
            throw (
                'a display-topology issue message interpolates a ' +
                'machine-identifying value; issues is free text, escapes ' +
                "property-name tokenization, and is published verbatim:`n$body"
            )
        }
    }

    Write-Host 'sanitize-results self-test: PASS'
} finally {
    $scratchFull = [IO.Path]::GetFullPath($scratch)
    $targetPrefix = [IO.Path]::GetFullPath($targetRoot).TrimEnd(
        [char[]]@('\', '/')
    ) + [IO.Path]::DirectorySeparatorChar
    if (
        $scratchFull.StartsWith(
            $targetPrefix,
            [StringComparison]::OrdinalIgnoreCase
        ) -and
        (Split-Path -Leaf $scratchFull) -like
            'perf-sanitize-self-test-*'
    ) {
        Remove-Item -LiteralPath $scratchFull -Recurse -Force `
            -ErrorAction SilentlyContinue
    }
}
