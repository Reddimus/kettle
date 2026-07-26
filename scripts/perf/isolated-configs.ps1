# Deterministic, run-local comparator configuration for the Windows performance
# harness. The caller owns the root and must provide a new, empty directory.

Set-StrictMode -Version Latest

function Test-KettlePerfSamePath {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Left,
        [Parameter(Mandatory = $true)]
        [string]$Right
    )

    $leftFull = [IO.Path]::GetFullPath($Left)
    $rightFull = [IO.Path]::GetFullPath($Right)
    $leftRoot = [IO.Path]::GetPathRoot($leftFull)
    $rightRoot = [IO.Path]::GetPathRoot($rightFull)
    if ($leftFull.Length -gt $leftRoot.Length) {
        $leftFull = $leftFull.TrimEnd(
            [IO.Path]::DirectorySeparatorChar,
            [IO.Path]::AltDirectorySeparatorChar
        )
    }
    if ($rightFull.Length -gt $rightRoot.Length) {
        $rightFull = $rightFull.TrimEnd(
            [IO.Path]::DirectorySeparatorChar,
            [IO.Path]::AltDirectorySeparatorChar
        )
    }
    return [StringComparer]::OrdinalIgnoreCase.Equals($leftFull, $rightFull)
}

function Test-KettlePerfPathWithinRoot {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [Parameter(Mandatory = $true)]
        [string]$Root
    )

    $rootFull = [IO.Path]::GetFullPath($Root).TrimEnd(
        [IO.Path]::DirectorySeparatorChar,
        [IO.Path]::AltDirectorySeparatorChar
    )
    $candidate = [IO.Path]::GetFullPath($Path)
    $prefix = $rootFull + [IO.Path]::DirectorySeparatorChar
    return $candidate.StartsWith(
        $prefix,
        [StringComparison]::OrdinalIgnoreCase
    )
}

function Assert-KettlePerfNoReparseAncestors {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseSingularNouns',
        '',
        Justification = 'The function checks every ancestor in a path chain.'
    )]
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    $current = [IO.Path]::GetFullPath($Path)
    while ($current) {
        $item = Get-Item -LiteralPath $current -Force -ErrorAction Stop
        if (
            ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0
        ) {
            throw "Isolated config path traverses a reparse point: $current"
        }
        $parent = [IO.Path]::GetDirectoryName($current)
        if (
            [string]::IsNullOrEmpty($parent) -or
            (Test-KettlePerfSamePath -Left $current -Right $parent)
        ) {
            break
        }
        $current = $parent
    }
}

function Get-KettlePerfUnsafeConfigRoots {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseSingularNouns',
        '',
        Justification = 'The function returns a collection of protected roots.'
    )]
    param()

    $roots = [Collections.Generic.List[string]]::new()
    $candidates = @(
        [IO.Path]::GetPathRoot([IO.Path]::GetFullPath($PSScriptRoot)),
        [IO.Path]::GetTempPath(),
        [Environment]::GetFolderPath(
            [Environment+SpecialFolder]::UserProfile
        ),
        [Environment]::GetFolderPath(
            [Environment+SpecialFolder]::ApplicationData
        ),
        [Environment]::GetFolderPath(
            [Environment+SpecialFolder]::LocalApplicationData
        ),
        [Environment]::GetFolderPath(
            [Environment+SpecialFolder]::CommonApplicationData
        ),
        [Environment]::GetFolderPath(
            [Environment+SpecialFolder]::Windows
        ),
        [Environment]::GetFolderPath(
            [Environment+SpecialFolder]::ProgramFiles
        ),
        [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..')),
        [IO.Path]::GetFullPath($PSScriptRoot)
    )
    if ((Get-Location).Provider.Name -eq 'FileSystem') {
        $candidates += (Get-Location).ProviderPath
    }
    foreach ($candidate in $candidates) {
        if (
            -not [string]::IsNullOrWhiteSpace($candidate) -and
            -not ($roots | Where-Object {
                Test-KettlePerfSamePath -Left $_ -Right $candidate
            })
        ) {
            $roots.Add([IO.Path]::GetFullPath($candidate))
        }
    }
    return $roots.ToArray()
}

function Assert-KettlePerfIsolatedConfigRoot {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Root
    )

    if (-not (Test-Path -LiteralPath $Root)) {
        throw "Isolated config root does not exist: $Root"
    }
    $item = Get-Item -LiteralPath $Root -Force -ErrorAction Stop
    if ($item.PSProvider.Name -ne 'FileSystem' -or -not $item.PSIsContainer) {
        throw "Isolated config root is not a filesystem directory: $Root"
    }
    $full = [IO.Path]::GetFullPath($item.FullName)
    $volumeRoot = [IO.Path]::GetPathRoot($full)
    if (Test-KettlePerfSamePath -Left $full -Right $volumeRoot) {
        throw "A filesystem root is not a safe isolated config root: $full"
    }
    Assert-KettlePerfNoReparseAncestors -Path $full
    foreach ($unsafe in Get-KettlePerfUnsafeConfigRoots) {
        if (Test-KettlePerfSamePath -Left $full -Right $unsafe) {
            throw "Refusing unsafe isolated config root: $full"
        }
    }
    if (
        @(Get-ChildItem -LiteralPath $full -Force -ErrorAction Stop).Count -ne
        0
    ) {
        throw "Isolated config root must be empty: $full"
    }
    return $full
}

function New-KettlePerfIsolatedConfigDirectory {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'The public generator validates and owns the empty root.'
    )]
    param(
        [Parameter(Mandatory = $true)]
        [string]$Root,
        [Parameter(Mandatory = $true)]
        [ValidatePattern('^[a-z0-9-]+$')]
        [string]$Name
    )

    Assert-KettlePerfNoReparseAncestors -Path $Root
    $directory = [IO.Path]::GetFullPath((Join-Path $Root $Name))
    if (-not (Test-KettlePerfPathWithinRoot -Path $directory -Root $Root)) {
        throw "Generated config directory escaped its root: $directory"
    }
    if (Test-Path -LiteralPath $directory) {
        throw "Generated config directory already exists: $directory"
    }
    $null = [IO.Directory]::CreateDirectory($directory)
    Assert-KettlePerfNoReparseAncestors -Path $directory
    return $directory
}

function ConvertTo-KettlePerfIsolatedConfigText {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyString()]
        [string]$Text
    )

    $normalized = $Text.Replace("`r`n", "`n").Replace("`r", "`n")
    return $normalized.TrimEnd([char[]]"`n") + "`n"
}

function Write-KettlePerfIsolatedConfigFile {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Root,
        [Parameter(Mandatory = $true)]
        [string]$Directory,
        [Parameter(Mandatory = $true)]
        [string]$Leaf,
        [Parameter(Mandatory = $true)]
        [string]$RelativePath,
        [Parameter(Mandatory = $true)]
        [AllowEmptyString()]
        [string]$Text
    )

    Assert-KettlePerfNoReparseAncestors -Path $Root
    Assert-KettlePerfNoReparseAncestors -Path $Directory
    $path = [IO.Path]::GetFullPath((Join-Path $Directory $Leaf))
    if (-not (Test-KettlePerfPathWithinRoot -Path $path -Root $Root)) {
        throw "Generated config file escaped its root: $path"
    }
    $normalized = ConvertTo-KettlePerfIsolatedConfigText -Text $Text
    $encoding = [Text.UTF8Encoding]::new($false, $true)
    $bytes = $encoding.GetBytes($normalized)
    $stream = [IO.FileStream]::new(
        $path,
        [IO.FileMode]::CreateNew,
        [IO.FileAccess]::Write,
        [IO.FileShare]::None
    )
    try {
        $stream.Write($bytes, 0, $bytes.Length)
        $stream.Flush($true)
    } finally {
        $stream.Dispose()
    }
    $item = Get-Item -LiteralPath $path -Force -ErrorAction Stop
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Generated config file became a reparse point: $path"
    }
    $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
    return [pscustomobject][ordered]@{
        relative_path = $RelativePath
        path = $path
        bytes = [long]$bytes.LongLength
        hash_algorithm = 'SHA256'
        sha256 = $hash
        encoding = 'utf-8'
        utf8_bom = $false
        line_endings = 'lf'
    }
}

function New-KettlePerfIsolatedConfigs {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseSingularNouns',
        '',
        Justification = 'One call creates the complete five-config set.'
    )]
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSUseShouldProcessForStateChangingFunctions',
        '',
        Justification = 'This refuses all but a caller-created empty safe root.'
    )]
    [OutputType([pscustomobject])]
    param(
        [Parameter(Mandatory = $true)]
        [string]$Root
    )

    $rootFull = Assert-KettlePerfIsolatedConfigRoot -Root $Root
    $directories = [ordered]@{}
    foreach ($name in @('kettle', 'alacritty', 'wezterm', 'rio', 'tabby')) {
        $directories[$name] = New-KettlePerfIsolatedConfigDirectory `
            -Root $rootFull -Name $name
    }

    $palette = [string[]]@(
        '#101010',
        '#cd3131',
        '#0dbc79',
        '#e5e510',
        '#2472c8',
        '#bc3fbc',
        '#11a8cd',
        '#e5e5e5',
        '#666666',
        '#f14c4c',
        '#23d18b',
        '#f5f543',
        '#3b8eea',
        '#d670d6',
        '#29b8db',
        '#f4f4f4'
    )

    $kettleText = @'
# Generated by scripts/perf/isolated-configs.ps1.
font-family = Cascadia Mono
font-size = 13
font-feature = -liga
font-feature = -clig
font-feature = -calt
background = #101010
foreground = #f4f4f4
cursor-color = #f4f4f4
cursor-fg-color = #101010
selection-background = #3b8eea
selection-foreground = #f4f4f4
palette = 0=#101010
palette = 1=#cd3131
palette = 2=#0dbc79
palette = 3=#e5e510
palette = 4=#2472c8
palette = 5=#bc3fbc
palette = 6=#11a8cd
palette = 7=#e5e5e5
palette = 8=#666666
palette = 9=#f14c4c
palette = 10=#23d18b
palette = 11=#f5f543
palette = 12=#3b8eea
palette = 13=#d670d6
palette = 14=#29b8db
palette = 15=#f4f4f4
scrollback = 10000
scrollback-bytes = 0
window-padding-x = 0
window-padding-y = 0
background-type = solid
background-opacity = 1.0
background-animation = off
cursor-style = block
cursor-blink = false
bell = off
tab-bar = always
tab-bar-position = top
status-bar = off
scrollbar = never
resize-overlay = never
unfocused-split-opacity = 1.0
restore-session = false
update-policy = off
update-check = false
record = off
agent-server = off
shell-integration = false
accent-color = #2472c8
'@

    $alacrittyText = @'
# Generated by scripts/perf/isolated-configs.ps1.
[general]
live_config_reload = false
ipc_socket = false

[window]
padding = { x = 0, y = 0 }
dynamic_padding = false
decorations = "Full"
startup_mode = "Windowed"
opacity = 1.0
blur = false

[scrolling]
history = 10000

[font]
normal = { family = "Cascadia Mono", style = "Regular" }
bold = { family = "Cascadia Mono", style = "Bold" }
italic = { family = "Cascadia Mono", style = "Italic" }
bold_italic = { family = "Cascadia Mono", style = "Bold Italic" }
size = 13.0
builtin_box_drawing = true

[colors.primary]
background = "#101010"
foreground = "#f4f4f4"

[colors.cursor]
text = "#101010"
cursor = "#f4f4f4"

[colors.selection]
text = "#f4f4f4"
background = "#3b8eea"

[colors.normal]
black = "#101010"
red = "#cd3131"
green = "#0dbc79"
yellow = "#e5e510"
blue = "#2472c8"
magenta = "#bc3fbc"
cyan = "#11a8cd"
white = "#e5e5e5"

[colors.bright]
black = "#666666"
red = "#f14c4c"
green = "#23d18b"
yellow = "#f5f543"
blue = "#3b8eea"
magenta = "#d670d6"
cyan = "#29b8db"
white = "#f4f4f4"

[cursor]
style = { shape = "Block", blinking = "Never" }
unfocused_hollow = false

[bell]
animation = "Linear"
duration = 0
'@

    $weztermText = @'
-- Generated by scripts/perf/isolated-configs.ps1.
local wezterm = require 'wezterm'

return {
  font = wezterm.font('Cascadia Mono', { weight = 'Regular' }),
  font_size = 13.0,
  harfbuzz_features = { 'liga=0', 'clig=0', 'calt=0' },
  color_scheme_dirs = {},
  colors = {
    foreground = '#f4f4f4',
    background = '#101010',
    cursor_bg = '#f4f4f4',
    cursor_fg = '#101010',
    cursor_border = '#f4f4f4',
    selection_fg = '#f4f4f4',
    selection_bg = '#3b8eea',
    ansi = {
      '#101010', '#cd3131', '#0dbc79', '#e5e510',
      '#2472c8', '#bc3fbc', '#11a8cd', '#e5e5e5',
    },
    brights = {
      '#666666', '#f14c4c', '#23d18b', '#f5f543',
      '#3b8eea', '#d670d6', '#29b8db', '#f4f4f4',
    },
  },
  scrollback_lines = 10000,
  window_padding = { left = 0, right = 0, top = 0, bottom = 0 },
  window_background_opacity = 1.0,
  text_background_opacity = 1.0,
  background = {},
  macos_window_background_blur = 0,
  win32_system_backdrop = 'Disable',
  enable_tab_bar = true,
  hide_tab_bar_if_only_one_tab = false,
  use_fancy_tab_bar = false,
  show_tabs_in_tab_bar = true,
  show_tab_index_in_tab_bar = false,
  show_new_tab_button_in_tab_bar = false,
  show_close_tab_button_in_tabs = false,
  cursor_blink_rate = 0,
  default_cursor_style = 'SteadyBlock',
  audible_bell = 'Disabled',
  visual_bell = {
    fade_in_duration_ms = 0,
    fade_out_duration_ms = 0,
  },
  automatically_reload_config = false,
  check_for_updates = false,
  window_close_confirmation = 'NeverPrompt',
}
'@

    $rioText = @'
# Generated by scripts/perf/isolated-configs.ps1.
scrollback-history-limit = 10000
margin = [0]
confirm-before-quit = false
enable-scroll-bar = false

[fonts]
family = "Cascadia Mono"
size = 13.0
features = []

[colors]
background = '#101010'
foreground = '#f4f4f4'
cursor = '#f4f4f4'
selection-background = '#3b8eea'
selection-foreground = '#f4f4f4'
black = '#101010'
red = '#cd3131'
green = '#0dbc79'
yellow = '#e5e510'
blue = '#2472c8'
magenta = '#bc3fbc'
cyan = '#11a8cd'
white = '#e5e5e5'
light-black = '#666666'
light-red = '#f14c4c'
light-green = '#23d18b'
light-yellow = '#f5f543'
light-blue = '#3b8eea'
light-magenta = '#d670d6'
light-cyan = '#29b8db'
light-white = '#f4f4f4'

[cursor]
shape = "Block"
blinking = false

[bell]
audio = false

[navigation]
mode = "Tab"
hide-if-single = false
unfocused-split-opacity = 1.0

[panel]
margin = [0]
padding = [0]
row-gap = 0
column-gap = 0
border-width = 0
border-radius = 0

[effects]
custom-mouse-cursor = false
trail-cursor = false

[window]
mode = "Windowed"
opacity = 1.0
opacity-cells = false
blur = false
decorations = "Enabled"

[developer]
enable-fps-counter = false
log-level = "OFF"
enable-log-file = false
'@

    $tabbyText = @'
# Generated by scripts/perf/isolated-configs.ps1.
version: 1
accessibility:
  animations: false
appearance:
  tabsLocation: top
  flexTabs: false
  opacity: 1.0
  vibrancy: false
  lastTabClosesWindow: false
  spaciness: 1
  colorSchemeMode: dark
terminal:
  autoOpen: true
  profile: local:cmd
  frontend: xterm-webgl
  font: Cascadia Mono
  fontSize: 13
  fontWeight: 400
  fontWeightBold: 700
  linePadding: 0
  ligatures: false
  cursor: block
  cursorBlink: false
  bell: off
  background: colorScheme
  scrollbackLines: 10000
  colorScheme:
    name: Kettle Benchmark
    foreground: '#f4f4f4'
    background: '#101010'
    cursor: '#f4f4f4'
    cursorAccent: '#101010'
    selection: '#3b8eea'
    colors:
      - '#101010'
      - '#cd3131'
      - '#0dbc79'
      - '#e5e510'
      - '#2472c8'
      - '#bc3fbc'
      - '#11a8cd'
      - '#e5e5e5'
      - '#666666'
      - '#f14c4c'
      - '#23d18b'
      - '#f5f543'
      - '#3b8eea'
      - '#d670d6'
      - '#29b8db'
      - '#f4f4f4'
  customColorSchemes: []
recoverTabs: false
enableAnalytics: false
enableWelcomeTab: false
enableAutomaticUpdates: false
hideTray: true
hacks:
  disableGPU: false
  disableVibrancyWhileDragging: false
  enableFluentBackground: false
'@

    $evidence = [ordered]@{}
    $evidence.kettle = Write-KettlePerfIsolatedConfigFile `
        -Root $rootFull -Directory $directories.kettle -Leaf 'config' `
        -RelativePath 'kettle/config' -Text $kettleText
    $evidence.alacritty = Write-KettlePerfIsolatedConfigFile `
        -Root $rootFull -Directory $directories.alacritty `
        -Leaf 'alacritty.toml' -RelativePath 'alacritty/alacritty.toml' `
        -Text $alacrittyText
    $evidence.wezterm = Write-KettlePerfIsolatedConfigFile `
        -Root $rootFull -Directory $directories.wezterm -Leaf 'wezterm.lua' `
        -RelativePath 'wezterm/wezterm.lua' -Text $weztermText
    $evidence.rio = Write-KettlePerfIsolatedConfigFile `
        -Root $rootFull -Directory $directories.rio -Leaf 'config.toml' `
        -RelativePath 'rio/config.toml' -Text $rioText
    $evidence.tabby = Write-KettlePerfIsolatedConfigFile `
        -Root $rootFull -Directory $directories.tabby -Leaf 'config.yaml' `
        -RelativePath 'tabby/config.yaml' -Text $tabbyText

    $terminals = [pscustomobject][ordered]@{
        kettle = [pscustomobject][ordered]@{
            name = 'kettle'
            config_kind = 'file'
            config_file = $evidence.kettle.path
            config_directory = $directories.kettle
            arguments = [string[]]@('--config', $evidence.kettle.path)
            environment = [ordered]@{}
            evidence = $evidence.kettle
        }
        alacritty = [pscustomobject][ordered]@{
            name = 'alacritty'
            config_kind = 'file'
            config_file = $evidence.alacritty.path
            config_directory = $directories.alacritty
            arguments = [string[]]@(
                '--config-file',
                $evidence.alacritty.path
            )
            environment = [ordered]@{}
            evidence = $evidence.alacritty
        }
        wezterm = [pscustomobject][ordered]@{
            name = 'wezterm'
            config_kind = 'file'
            config_file = $evidence.wezterm.path
            config_directory = $directories.wezterm
            arguments = [string[]]@(
                '--config-file',
                $evidence.wezterm.path
            )
            environment = [ordered]@{}
            evidence = $evidence.wezterm
        }
        rio = [pscustomobject][ordered]@{
            name = 'rio'
            config_kind = 'directory'
            config_file = $evidence.rio.path
            config_directory = $directories.rio
            arguments = [string[]]@()
            environment = [ordered]@{
                RIO_CONFIG_HOME = $directories.rio
            }
            evidence = $evidence.rio
        }
        tabby = [pscustomobject][ordered]@{
            name = 'tabby'
            config_kind = 'directory'
            config_file = $evidence.tabby.path
            config_directory = $directories.tabby
            arguments = [string[]]@()
            environment = [ordered]@{
                TABBY_CONFIG_DIRECTORY = $directories.tabby
                TABBY_PLUGINS = ''
            }
            evidence = $evidence.tabby
        }
    }

    return [pscustomobject][ordered]@{
        schema_version = 1
        root = $rootFull
        benchmark_profile = [pscustomobject][ordered]@{
            font_family = 'Cascadia Mono'
            font_size_points = 13.0
            foreground = '#f4f4f4'
            background = '#101010'
            selection_background = '#3b8eea'
            scrollback_lines = 10000
            padding_pixels = 0
            opacity = 1.0
            visible_tabs = 1
            palette = $palette
        }
        terminals = $terminals
        files = [object[]]@(
            $evidence.kettle,
            $evidence.alacritty,
            $evidence.wezterm,
            $evidence.rio,
            $evidence.tabby
        )
    }
}
