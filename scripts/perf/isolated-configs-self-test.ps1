# GUI-free cross-engine contract tests for isolated comparator configuration.

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. "$PSScriptRoot\isolated-configs.ps1"

function Assert-KettlePerfIsolatedConfigTest {
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

function Assert-KettlePerfIsolatedConfigThrows {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseSingularNouns',
        '',
        Justification = 'Throws describes the behavior asserted by the helper.'
    )]
    param(
        [Parameter(Mandatory = $true)]
        [scriptblock]$Action,
        [Parameter(Mandatory = $true)]
        [string]$Pattern,
        [Parameter(Mandatory = $true)]
        [string]$Message
    )

    $caught = $null
    try {
        & $Action
    } catch {
        $caught = $_
    }
    if (-not $caught -or $caught.Exception.Message -notmatch $Pattern) {
        $actual = if ($caught) {
            $caught.Exception.Message
        } else {
            '<no exception>'
        }
        throw "$Message (actual: $actual)"
    }
}

function Assert-KettlePerfConfigContains {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseSingularNouns',
        '',
        Justification = 'Contains describes the relation asserted by the helper.'
    )]
    param(
        [Parameter(Mandatory = $true)]
        [string]$Text,
        [Parameter(Mandatory = $true)]
        [string[]]$Lines,
        [Parameter(Mandatory = $true)]
        [string]$Name
    )

    foreach ($line in $Lines) {
        if (-not $Text.Contains($line)) {
            throw "$Name config is missing: $line"
        }
    }
}

$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd(
    [IO.Path]::DirectorySeparatorChar,
    [IO.Path]::AltDirectorySeparatorChar
)
$testBase = Join-Path $tempRoot (
    'kettle-isolated-configs-self-test-' + [Guid]::NewGuid().ToString('N')
)
$baseLeaf = [IO.Path]::GetFileName($testBase)
if (
    -not $baseLeaf.StartsWith(
        'kettle-isolated-configs-self-test-',
        [StringComparison]::Ordinal
    ) -or
    -not (Test-KettlePerfPathWithinRoot -Path $testBase -Root $tempRoot)
) {
    throw "Refusing unsafe self-test directory: $testBase"
}
$null = New-Item -ItemType Directory -Path $testBase -ErrorAction Stop
$reparsePaths = [Collections.Generic.List[string]]::new()
$reparseTargetChildren = [Collections.Generic.List[string]]::new()

try {
    $firstRoot = Join-Path $testBase 'first'
    $secondRoot = Join-Path $testBase 'second'
    $null = New-Item -ItemType Directory -Path $firstRoot,$secondRoot
    $first = New-KettlePerfIsolatedConfigs -Root $firstRoot
    $second = New-KettlePerfIsolatedConfigs -Root $secondRoot

    Assert-KettlePerfIsolatedConfigTest `
        -Condition ($first.schema_version -eq 1) `
        -Message 'isolated config schema version drifted'
    Assert-KettlePerfIsolatedConfigTest `
        -Condition (
            Test-KettlePerfSamePath -Left $first.root -Right $firstRoot
        ) `
        -Message 'isolated config result reports the wrong root'
    Assert-KettlePerfIsolatedConfigTest -Condition (
        $first.benchmark_profile.font_family -eq 'Cascadia Mono' -and
        $first.benchmark_profile.font_size_points -eq 13.0 -and
        $first.benchmark_profile.background -eq '#101010' -and
        $first.benchmark_profile.foreground -eq '#f4f4f4' -and
        $first.benchmark_profile.scrollback_lines -eq 10000 -and
        $first.benchmark_profile.padding_pixels -eq 0 -and
        $first.benchmark_profile.opacity -eq 1.0 -and
        $first.benchmark_profile.visible_tabs -eq 1 -and
        $first.benchmark_profile.palette.Count -eq 16
    ) -Message 'common benchmark profile drifted'

    $names = [string[]]@(
        $first.terminals.PSObject.Properties.Name
    )
    Assert-KettlePerfIsolatedConfigTest -Condition (
        ($names -join ',') -eq 'kettle,alacritty,wezterm,rio,tabby'
    ) -Message 'terminal set or order drifted'
    Assert-KettlePerfIsolatedConfigTest `
        -Condition (-not ($names -contains 'wt')) `
        -Message 'Windows Terminal must not receive an isolated config'
    Assert-KettlePerfIsolatedConfigTest `
        -Condition ($first.files.Count -eq 5) `
        -Message 'isolated config result must contain five file records'

    foreach ($name in $names) {
        $terminal = $first.terminals.PSObject.Properties[$name].Value
        $other = $second.terminals.PSObject.Properties[$name].Value
        Assert-KettlePerfIsolatedConfigTest `
            -Condition ($terminal.name -eq $name) `
            -Message "$name result has the wrong typed name"
        $expectedKind = if ($name -in @('rio', 'tabby')) {
            'directory'
        } else {
            'file'
        }
        Assert-KettlePerfIsolatedConfigTest `
            -Condition ($terminal.config_kind -eq $expectedKind) `
            -Message "$name result has the wrong config kind"
        Assert-KettlePerfIsolatedConfigTest -Condition (
            $terminal.config_file -is [string] -and
            $terminal.config_directory -is [string] -and
            (Test-Path -LiteralPath $terminal.config_file -PathType Leaf) -and
            (
                Test-Path -LiteralPath $terminal.config_directory `
                    -PathType Container
            ) -and
            (
                Test-KettlePerfPathWithinRoot `
                    -Path $terminal.config_file -Root $first.root
            ) -and
            (
                Test-KettlePerfPathWithinRoot `
                    -Path $terminal.config_directory -Root $first.root
            )
        ) -Message "$name config paths are not typed, present, and contained"
        Assert-KettlePerfIsolatedConfigTest `
            -Condition (
                @(
                    Get-ChildItem -LiteralPath $terminal.config_directory `
                        -Force
                ).Count -eq 1
            ) `
            -Message "$name config directory contains unexpected state"

        $bytes = [IO.File]::ReadAllBytes($terminal.config_file)
        $hasBom = (
            $bytes.Length -ge 3 -and
            $bytes[0] -eq 0xEF -and
            $bytes[1] -eq 0xBB -and
            $bytes[2] -eq 0xBF
        )
        Assert-KettlePerfIsolatedConfigTest `
            -Condition (-not $hasBom -and -not $terminal.evidence.utf8_bom) `
            -Message "$name config contains a UTF-8 BOM"
        $strictUtf8 = [Text.UTF8Encoding]::new($false, $true)
        $text = $strictUtf8.GetString($bytes)
        Assert-KettlePerfIsolatedConfigTest `
            -Condition (
                -not $text.Contains("`r") -and $text.EndsWith("`n")
            ) `
            -Message "$name config is not canonical LF text"
        $hash = (Get-FileHash -LiteralPath $terminal.config_file `
            -Algorithm SHA256).Hash
        Assert-KettlePerfIsolatedConfigTest -Condition (
            $terminal.evidence.path -eq $terminal.config_file -and
            $terminal.evidence.bytes -eq $bytes.LongLength -and
            $terminal.evidence.hash_algorithm -eq 'SHA256' -and
            $terminal.evidence.sha256 -eq $hash -and
            $terminal.evidence.encoding -eq 'utf-8' -and
            $terminal.evidence.line_endings -eq 'lf'
        ) -Message "$name file evidence does not match the bytes"
        Assert-KettlePerfIsolatedConfigTest -Condition (
            $terminal.evidence.relative_path -eq $other.evidence.relative_path -and
            $terminal.evidence.sha256 -eq $other.evidence.sha256 -and
            [Linq.Enumerable]::SequenceEqual(
                [byte[]]$bytes,
                [byte[]][IO.File]::ReadAllBytes($other.config_file)
            )
        ) -Message "$name config is not deterministic across roots"
    }

    Assert-KettlePerfIsolatedConfigTest -Condition (
        ($first.terminals.kettle.arguments -join "`0") -eq (
            "--config`0$($first.terminals.kettle.config_file)"
        ) -and
        ($first.terminals.alacritty.arguments -join "`0") -eq (
            "--config-file`0$($first.terminals.alacritty.config_file)"
        ) -and
        ($first.terminals.wezterm.arguments -join "`0") -eq (
            "--config-file`0$($first.terminals.wezterm.config_file)"
        ) -and
        $first.terminals.rio.arguments.Count -eq 0 -and
        $first.terminals.tabby.arguments.Count -eq 0 -and
        $first.terminals.rio.environment.RIO_CONFIG_HOME -eq (
            $first.terminals.rio.config_directory
        ) -and
        $first.terminals.tabby.environment.TABBY_CONFIG_DIRECTORY -eq (
            $first.terminals.tabby.config_directory
        ) -and
        $first.terminals.tabby.environment.TABBY_PLUGINS -eq ''
    ) -Message 'terminal activation contracts drifted'

    $kettle = [IO.File]::ReadAllText(
        $first.terminals.kettle.config_file,
        [Text.UTF8Encoding]::new($false, $true)
    )
    Assert-KettlePerfConfigContains -Text $kettle -Name 'Kettle' -Lines @(
        'font-family = Cascadia Mono',
        'font-size = 13',
        'scrollback = 10000',
        'scrollback-bytes = 0',
        'window-padding-x = 0',
        'window-padding-y = 0',
        'background-type = solid',
        'background-opacity = 1.0',
        'background-animation = off',
        'cursor-blink = false',
        'tab-bar = always',
        'restore-session = false',
        'update-policy = off',
        'record = off',
        'agent-server = off'
    )

    $alacritty = [IO.File]::ReadAllText(
        $first.terminals.alacritty.config_file,
        [Text.UTF8Encoding]::new($false, $true)
    )
    Assert-KettlePerfConfigContains -Text $alacritty -Name 'Alacritty' -Lines @(
        'live_config_reload = false',
        'padding = { x = 0, y = 0 }',
        'opacity = 1.0',
        'history = 10000',
        'family = "Cascadia Mono"',
        'size = 13.0',
        'style = { shape = "Block", blinking = "Never" }',
        'duration = 0'
    )

    $wezterm = [IO.File]::ReadAllText(
        $first.terminals.wezterm.config_file,
        [Text.UTF8Encoding]::new($false, $true)
    )
    Assert-KettlePerfConfigContains -Text $wezterm -Name 'WezTerm' -Lines @(
        "font = wezterm.font('Cascadia Mono'",
        'font_size = 13.0',
        'scrollback_lines = 10000',
        'window_padding = { left = 0, right = 0, top = 0, bottom = 0 }',
        'window_background_opacity = 1.0',
        "win32_system_backdrop = 'Disable'",
        'hide_tab_bar_if_only_one_tab = false',
        'cursor_blink_rate = 0',
        "audible_bell = 'Disabled'",
        'automatically_reload_config = false',
        'check_for_updates = false'
    )
    Assert-KettlePerfIsolatedConfigTest `
        -Condition (-not $wezterm.Contains('hide_tab_bar_if_only_one =')) `
        -Message 'WezTerm config uses the obsolete tab-bar key'

    $rio = [IO.File]::ReadAllText(
        $first.terminals.rio.config_file,
        [Text.UTF8Encoding]::new($false, $true)
    )
    Assert-KettlePerfConfigContains -Text $rio -Name 'Rio' -Lines @(
        'scrollback-history-limit = 10000',
        'margin = [0]',
        'family = "Cascadia Mono"',
        'size = 13.0',
        'hide-if-single = false',
        'padding = [0]',
        'custom-mouse-cursor = false',
        'trail-cursor = false',
        'opacity = 1.0',
        'blur = false',
        'enable-log-file = false'
    )

    $tabby = [IO.File]::ReadAllText(
        $first.terminals.tabby.config_file,
        [Text.UTF8Encoding]::new($false, $true)
    )
    Assert-KettlePerfConfigContains -Text $tabby -Name 'Tabby' -Lines @(
        '  animations: false',
        '  tabsLocation: top',
        '  opacity: 1.0',
        '  autoOpen: true',
        '  font: Cascadia Mono',
        '  fontSize: 13',
        '  linePadding: 0',
        '  cursorBlink: false',
        '  scrollbackLines: 10000',
        'recoverTabs: false',
        'enableAnalytics: false',
        'enableWelcomeTab: false',
        'enableAutomaticUpdates: false'
    )

    Assert-KettlePerfIsolatedConfigThrows `
        -Action {
            New-KettlePerfIsolatedConfigs -Root $firstRoot | Out-Null
        } -Pattern 'must be empty' `
        -Message 'a second generation accepted a nonempty root'

    $nonemptyRoot = Join-Path $testBase 'nonempty'
    $null = New-Item -ItemType Directory -Path $nonemptyRoot
    $sentinel = Join-Path $nonemptyRoot 'sentinel.txt'
    [IO.File]::WriteAllText(
        $sentinel,
        'keep',
        [Text.UTF8Encoding]::new($false)
    )
    Assert-KettlePerfIsolatedConfigThrows `
        -Action {
            New-KettlePerfIsolatedConfigs -Root $nonemptyRoot | Out-Null
        } -Pattern 'must be empty' `
        -Message 'generator accepted a caller root containing data'
    Assert-KettlePerfIsolatedConfigTest -Condition (
        @(Get-ChildItem -LiteralPath $nonemptyRoot -Force).Count -eq 1 -and
        [IO.File]::ReadAllText($sentinel) -eq 'keep'
    ) -Message 'nonempty-root rejection mutated caller data'

    $missingRoot = Join-Path $testBase 'missing'
    Assert-KettlePerfIsolatedConfigThrows `
        -Action {
            New-KettlePerfIsolatedConfigs -Root $missingRoot | Out-Null
        } -Pattern 'does not exist' `
        -Message 'generator accepted a missing root'
    Assert-KettlePerfIsolatedConfigThrows `
        -Action {
            New-KettlePerfIsolatedConfigs -Root $tempRoot | Out-Null
        } -Pattern 'unsafe' `
        -Message 'generator accepted the shared temporary root'
    $volumeRoot = [IO.Path]::GetPathRoot($testBase)
    Assert-KettlePerfIsolatedConfigThrows `
        -Action {
            New-KettlePerfIsolatedConfigs -Root $volumeRoot | Out-Null
        } -Pattern 'filesystem root' `
        -Message 'generator accepted a filesystem root'

    if ($env:OS -eq 'Windows_NT') {
        $junctionTarget = Join-Path $testBase 'junction-target'
        $junctionRoot = Join-Path $testBase 'junction-root'
        $null = New-Item -ItemType Directory -Path $junctionTarget
        $null = New-Item -ItemType Junction -Path $junctionRoot `
            -Target $junctionTarget
        $reparsePaths.Add($junctionRoot)
        Assert-KettlePerfIsolatedConfigThrows `
            -Action {
                New-KettlePerfIsolatedConfigs -Root $junctionRoot | Out-Null
            } -Pattern 'reparse point' `
            -Message 'generator accepted a reparse-point root'

        $ancestorTarget = Join-Path $testBase 'ancestor-target'
        $ancestorLink = Join-Path $testBase 'ancestor-link'
        $null = New-Item -ItemType Directory -Path $ancestorTarget
        $null = New-Item -ItemType Junction -Path $ancestorLink `
            -Target $ancestorTarget
        $reparsePaths.Add($ancestorLink)
        $rootThroughJunction = Join-Path $ancestorLink 'child'
        $null = New-Item -ItemType Directory -Path $rootThroughJunction
        $reparseTargetChildren.Add((Join-Path $ancestorTarget 'child'))
        Assert-KettlePerfIsolatedConfigThrows `
            -Action {
                New-KettlePerfIsolatedConfigs `
                    -Root $rootThroughJunction | Out-Null
            } -Pattern 'reparse point' `
            -Message 'generator accepted a reparse-point ancestor'
    }

    Write-Output (
        'isolated config self-test passed: five deterministic configs, ' +
        'BOM-free SHA256 evidence, activation shape, and root safety'
    )
} finally {
    $canRemoveBase = $true
    foreach ($child in $reparseTargetChildren) {
        if (
            (Test-Path -LiteralPath $child) -and
            (Test-KettlePerfPathWithinRoot -Path $child -Root $testBase)
        ) {
            try {
                Remove-Item -LiteralPath $child -Force -ErrorAction Stop
            } catch {
                $canRemoveBase = $false
                Write-Warning (
                    "Could not remove self-test junction target child $child; " +
                    'recursive cleanup was skipped'
                )
            }
        }
    }
    foreach ($reparse in $reparsePaths) {
        if (Test-Path -LiteralPath $reparse) {
            try {
                Remove-Item -LiteralPath $reparse -Force -ErrorAction Stop
            } catch {
                $canRemoveBase = $false
                Write-Warning (
                    "Could not remove self-test reparse point $reparse; " +
                    'recursive cleanup was skipped'
                )
            }
        }
    }
    if (
        $canRemoveBase -and
        (Test-Path -LiteralPath $testBase) -and
        (Test-KettlePerfPathWithinRoot -Path $testBase -Root $tempRoot) -and
        [IO.Path]::GetFileName($testBase).StartsWith(
            'kettle-isolated-configs-self-test-',
            [StringComparison]::Ordinal
        )
    ) {
        Remove-Item -LiteralPath $testBase -Recurse -Force
    }
}
