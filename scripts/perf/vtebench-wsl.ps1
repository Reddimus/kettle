# Run the pinned alacritty/vtebench suite inside each terminal's WSL session.
# The source is checked out and built on WSL's Linux filesystem so Windows
# checkout line endings cannot silently turn every benchmark loader into empty
# data. DAT output is parsed using vtebench's column-oriented format.
param(
    [string[]]$Terminals = @(
        'kettle', 'wt', 'alacritty', 'wezterm', 'rio', 'tabby'
    ),
    [string]$ResultsDir = '',
    [string]$VtebenchRepo = '',
    [ValidatePattern('^[0-9a-fA-F]{40}$')]
    [string]$VtebenchRevision = 'ead80032e57dee2e75f0b51f2ea67528647d9944',
    [string]$KettleExe = '',
    [string]$KettleConfig = '',
    [string]$AlacrittyExe = '',
    [string]$WeztermExe = '',
    [string]$RioExe = '',
    [string]$TabbyExe = '',
    [string]$PowerShellExe = '',
    [string]$WslExe = '',
    [string]$WslDistribution = '',
    [ValidateSet(
        'program-files-wsl-then-system32-v1',
        'explicit-override-v1'
    )]
    [string]$WslResolutionPolicy = '',
    $IsolatedProfile = $null,
    [string]$TargetScreenDevice = '',
    [ValidatePattern('^[0-9a-fA-F-]{36}$')]
    [string]$RunId = '',
    [ValidateRange(320, 16384)]
    [int]$WindowW = 1280,
    [ValidateRange(240, 16384)]
    [int]$WindowH = 800,
    [ValidateRange(1, 86400)]
    [int]$TimeoutSec = 900,
    [ValidateRange(60, 7200)]
    [int]$SetupTimeoutSec = 1800,
    [ValidateRange(5, 300)]
    [int]$CleanupTimeoutSec = 30,
    [switch]$SetupOnly
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\lib-win32.ps1"
. "$PSScriptRoot\terminal-specs.ps1"
. "$PSScriptRoot\json-io.ps1"
. "$PSScriptRoot\vtebench-channel.ps1"
. "$PSScriptRoot\wsl-launcher.ps1"
if (-not $ResultsDir) {
    $ResultsDir = Join-Path $PSScriptRoot '..\..\target\perf-results'
}
if (-not $KettleExe) {
    $KettleExe = Join-Path $PSScriptRoot '..\..\target\release\kettle.exe'
}
if (-not $RunId) {
    $RunId = [Guid]::NewGuid().ToString('D')
}
if (-not $PowerShellExe) {
    $PowerShellExe = Get-Command pwsh.exe -CommandType Application `
        -ErrorAction Stop |
        Select-Object -First 1 -ExpandProperty Source
}
if (-not (Test-Path -LiteralPath $PowerShellExe -PathType Leaf)) {
    throw "PowerShell 7 vtebench wrapper runner not found: $PowerShellExe"
}
$PowerShellExe = (Resolve-Path -LiteralPath $PowerShellExe).Path
$wrapper = (
    Resolve-Path -LiteralPath (
        Join-Path $PSScriptRoot 'vtebench-inside.ps1'
    )
).Path
New-Item -ItemType Directory -Force $ResultsDir | Out-Null
$resolvedResults = (Resolve-Path -LiteralPath $ResultsDir).Path
$wslLauncherArguments = @{
    Path = $WslExe
}
if ($WslResolutionPolicy) {
    $wslLauncherArguments.ResolutionPolicy = $WslResolutionPolicy
}
$wslLauncherEvidence = Open-KettlePerfWslLauncherEvidence `
    @wslLauncherArguments
$WslExe = $wslLauncherEvidence.Path
$WslDistribution = Resolve-KettlePerfWslDistribution `
    -WslExe $WslExe -Name $WslDistribution
$wslDistributionEvidence = Get-KettlePerfWslDistributionEvidence `
    -WslExe $WslExe -Distribution $WslDistribution
$buildRootWsl = $null
try {

function ConvertTo-WslPath {
    param([Parameter(Mandatory = $true)][string]$WindowsPath)

    $portablePath = $WindowsPath.Replace('\', '/')
    $pathBytes = [Text.UTF8Encoding]::new(
        $false,
        $true
    ).GetBytes($portablePath)
    try {
        $encodedPath = [Convert]::ToBase64String($pathBytes)
    } finally {
        [Array]::Clear($pathBytes, 0, $pathBytes.Length)
    }
    $pathCommand = (
        "value=`$(printf '%s' '$encodedPath' | base64 --decode)`n" +
        'wslpath -u -a -- "$value"'
    )
    $converted = (
        Invoke-KettlePerfWslBashCapture `
            -WslExe $WslExe -Distribution $WslDistribution `
            -Script $pathCommand -TimeoutSec 10 `
            -MaximumOutputBytes 8192
    ).Trim()
    if (-not $converted -or -not $converted.StartsWith('/')) {
        throw "WSL could not translate Windows path: $WindowsPath"
    }
    return [string]$converted
}

function ConvertTo-BashLiteral {
    param([Parameter(Mandatory = $true)][string]$Value)

    $singleQuoteEscape = "'" + '"' + "'" + '"' + "'"
    return "'" + $Value.Replace("'", $singleQuoteEscape) + "'"
}

$environmentRepo = [Environment]::GetEnvironmentVariable(
    'KETTLE_PERF_VTEBENCH_REPO'
)
if (
    $VtebenchRepo -and
    -not (Test-Path -LiteralPath $VtebenchRepo -PathType Container)
) {
    throw "Explicit vtebench repository was not found: $VtebenchRepo"
}
if (
    $environmentRepo -and
    -not (Test-Path -LiteralPath $environmentRepo -PathType Container)
) {
    throw (
        'KETTLE_PERF_VTEBENCH_REPO points to a missing directory: ' +
        $environmentRepo
    )
}
$repoCandidates = @(
    $VtebenchRepo,
    $environmentRepo,
    (Join-Path $PSScriptRoot '..\..\..\research\vtebench'),
    (Join-Path $env:LOCALAPPDATA 'KettleBench\sources\vtebench')
)
$resolvedRepo = $repoCandidates |
    Where-Object {
        $_ -and (Test-Path -LiteralPath $_ -PathType Container)
    } |
    ForEach-Object { (Resolve-Path -LiteralPath $_).Path } |
    Select-Object -First 1
if (-not $resolvedRepo) {
    throw (
        'vtebench repository not found; set -VtebenchRepo or ' +
        'KETTLE_PERF_VTEBENCH_REPO'
    )
}
$revisionExpression = "$VtebenchRevision^{commit}"
$LASTEXITCODE = 0
$resolvedRevision = (
    & git -C $resolvedRepo rev-parse $revisionExpression 2>$null |
        Select-Object -First 1
) -join ''
if (
    $LASTEXITCODE -ne 0 -or
    $resolvedRevision -notmatch '^[0-9a-fA-F]{40}$'
) {
    throw "Could not resolve a pinned vtebench commit from $resolvedRepo"
}
$resolvedRevision = $resolvedRevision.ToLowerInvariant()
$sourceOrigin = (
    & git -C $resolvedRepo remote get-url origin 2>$null |
        Select-Object -First 1
) -join ''
$repoWsl = ConvertTo-WslPath $resolvedRepo

# Clone from Git objects, not the Windows worktree. A WSL-local checkout keeps
# benchmark/setup scripts executable and LF-only, then an atomic stage rename
# prevents a partial cache from being treated as a valid pinned checkout.
$setupTemplate = @'
set -euo pipefail
source_repo=__SOURCE_REPO__
revision=__REVISION__
generator_timeout=__GENERATOR_TIMEOUT__
cargo_fetch_timeout=__CARGO_FETCH_TIMEOUT__
cargo_build_timeout=__CARGO_BUILD_TIMEOUT__
preflight_timeout=__PREFLIGHT_TIMEOUT__
cache_parent="$HOME/.cache/kettle-perf"
cache="$cache_parent/vtebench-source-v2-$revision"
mkdir -p -- "$cache_parent"
if [[ ! -d "$cache/.git" ]]; then
    stage="$(mktemp -d "$cache_parent/vtebench-stage.XXXXXX")"
    trap 'rm -rf -- "$stage"' EXIT
    git clone --no-checkout -- "$source_repo" "$stage/repo" >&2
    git -C "$stage/repo" -c core.autocrlf=false checkout --detach "$revision" >&2
    mv -- "$stage/repo" "$cache"
    rmdir -- "$stage"
    trap - EXIT
fi
[[ "$(git -C "$cache" rev-parse HEAD)" == "$revision" ]]
[[ -z "$(git -C "$cache" -c core.excludesFile=/dev/null \
    status --porcelain=v1 --untracked-files=all --ignored=matching)" ]]
git -C "$cache" diff --no-ext-diff --quiet "$revision" --
git -C "$cache" diff --no-ext-diff --cached --quiet "$revision" --
cr="$(printf '\r')"
if grep -IRl --include=benchmark --include=setup -- "$cr" "$cache/benchmarks" |
    grep -q .
then
    echo "vtebench cache contains CRLF benchmark scripts" >&2
    exit 65
fi
cd -- "$cache"
timeout_path="$(realpath -e -- "$(command -v timeout)")"
[[ -f "$timeout_path" && -x "$timeout_path" && ! -L "$timeout_path" ]]
setsid_path="$(realpath -e -- "$(command -v setsid)")"
[[ -f "$setsid_path" && -x "$setsid_path" && ! -L "$setsid_path" ]]
script_path="$(realpath -e -- "$(command -v script)")"
[[ -f "$script_path" && -x "$script_path" && ! -L "$script_path" ]]
benchmark_scripts=()
while IFS= read -r -d '' tracked_path; do
    if [[ "$tracked_path" == */benchmark ]]; then
        benchmark_scripts+=("$tracked_path")
    fi
done < <(git ls-tree -r -z --name-only "$revision" -- benchmarks)
if (( ${#benchmark_scripts[@]} == 0 )); then
    echo "vtebench source contains no benchmark scripts" >&2
    exit 66
fi
for script in "${benchmark_scripts[@]}"; do
    mode="$(git ls-files --stage -- "$script" | awk 'NR == 1 {print $1}')"
    if [[ "$mode" == 120000 ]]; then
        resolved_script="$(realpath -e -- "$script")"
        case "$resolved_script" in
            "$cache"/benchmarks/*) ;;
            *)
                echo "tracked generator symlink escapes benchmarks: $script" >&2
                exit 68
                ;;
        esac
        resolved_relative="${resolved_script#"$cache"/}"
        resolved_mode="$(
            git ls-files --stage -- "$resolved_relative" |
                awk 'NR == 1 {print $1}'
        )"
        [[ "$resolved_mode" == 100755 ]]
    else
        [[ "$mode" == 100755 && ! -L "$script" ]]
    fi
    [[ -f "$script" && -x "$script" ]]
    bytes="$(
        TERM=xterm-256color BENCHMARK="$script" \
            "$timeout_path" --foreground --signal=TERM --kill-after=5s \
            "$generator_timeout" "$script_path" -qefc \
            'stty cols 120 rows 40; exec "$BENCHMARK"' \
            /dev/null </dev/null |
            wc -c
    )"
    if (( bytes <= 0 )); then
        echo "vtebench generator produced no data: $script" >&2
        exit 67
    fi
done
build_root="$(mktemp -d "$cache_parent/vtebench-build-$revision.XXXXXX")"
trap 'rm -rf --one-file-system -- "$build_root"' EXIT
cargo_home="$build_root/cargo-home"
target="$build_root/target"
mkdir -p -- "$cargo_home" "$target"
rustup_path="$(realpath -e -- "$(command -v rustup)")"
[[ -f "$rustup_path" && -x "$rustup_path" && ! -L "$rustup_path" ]]
cargo_candidate="$(
    "$timeout_path" --foreground --signal=TERM --kill-after=2s \
        10 "$rustup_path" which cargo
)"
cargo_path="$(realpath -e -- "$cargo_candidate")"
[[ -f "$cargo_path" && -x "$cargo_path" && ! -L "$cargo_path" ]]
"$timeout_path" --foreground --signal=TERM --kill-after=5s \
    "$cargo_fetch_timeout" \
    env -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER \
    CARGO_HOME="$cargo_home" CARGO_TARGET_DIR="$target" \
    "$cargo_path" fetch --locked >&2
"$timeout_path" --foreground --signal=TERM --kill-after=5s \
    "$cargo_build_timeout" \
    env -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER \
    CARGO_HOME="$cargo_home" CARGO_TARGET_DIR="$target" \
    "$cargo_path" build --release --frozen >&2
binary="$(realpath -e -- "$target/release/vtebench")"
[[ -f "$binary" && -x "$binary" && ! -L "$binary" ]]
[[ -z "$(git -c core.excludesFile=/dev/null \
    status --porcelain=v1 --untracked-files=all --ignored=matching)" ]]
git diff --no-ext-diff --quiet "$revision" --
git diff --no-ext-diff --cached --quiet "$revision" --
preflight_dat="$(mktemp "$cache_parent/vtebench-preflight.XXXXXX.dat")"
trap 'rm -f -- "$preflight_dat"; rm -rf --one-file-system -- "$build_root"' EXIT
preflight_dir="$(dirname -- "${benchmark_scripts[0]}")"
TERM=xterm-256color VTEBENCH_BINARY="$binary" \
    PREFLIGHT_DIR="$preflight_dir" PREFLIGHT_DAT="$preflight_dat" \
    "$timeout_path" --foreground --signal=TERM --kill-after=5s \
    "$preflight_timeout" "$script_path" -qefc \
    'stty cols 120 rows 40; exec "$VTEBENCH_BINARY" --silent \
        --benchmarks "$PREFLIGHT_DIR" --warmup 0 --min-bytes 1024 \
        --max-samples 1 --max-secs 1 --dat "$PREFLIGHT_DAT"' \
    /dev/null </dev/null >/dev/null
awk '
    NR == 1 { if (NF != 1) exit 1; next }
    NR == 2 { if (NF != 1 || $1 !~ /^[0-9]+$/) exit 1; ok = 1 }
    END { if (!ok) exit 1 }
' "$preflight_dat"
rm -f -- "$preflight_dat"
trap - EXIT
printf 'CACHE=%s\n' "$cache"
printf 'BUILD_ROOT=%s\n' "$build_root"
printf 'BINARY=%s\n' "$binary"
printf 'EXPECTED=%s\n' "${#benchmark_scripts[@]}"
printf 'BENCHMARK_TREE=%s\n' "$(git rev-parse "$revision:benchmarks")"
printf 'BINARY_SHA256=%s\n' "$(sha256sum "$binary" | awk '{print $1}')"
printf 'LOCK_SHA256=%s\n' "$(sha256sum Cargo.lock | awk '{print $1}')"
printf 'CARGO_PATH=%s\n' "$cargo_path"
printf 'CARGO_SHA256=%s\n' "$(sha256sum "$cargo_path" | awk '{print $1}')"
printf 'CARGO_VERSION=%s\n' "$(
    "$timeout_path" --foreground --signal=TERM --kill-after=2s \
        10 "$cargo_path" --version
)"
printf 'RUSTUP_PATH=%s\n' "$rustup_path"
printf 'RUSTUP_SHA256=%s\n' "$(sha256sum "$rustup_path" | awk '{print $1}')"
printf 'RUSTUP_VERSION=%s\n' "$(
    "$timeout_path" --foreground --signal=TERM --kill-after=2s \
        10 "$rustup_path" --version
)"
printf 'TIMEOUT_PATH=%s\n' "$timeout_path"
printf 'TIMEOUT_SHA256=%s\n' "$(sha256sum "$timeout_path" | awk '{print $1}')"
printf 'TIMEOUT_VERSION=%s\n' "$(
    "$timeout_path" --foreground --signal=TERM --kill-after=2s \
        10 "$timeout_path" --version |
        head -n1
)"
printf 'SETSID_PATH=%s\n' "$setsid_path"
printf 'SETSID_SHA256=%s\n' "$(sha256sum "$setsid_path" | awk '{print $1}')"
printf 'SETSID_VERSION=%s\n' "$(
    "$timeout_path" --foreground --signal=TERM --kill-after=2s \
        10 "$setsid_path" --version |
        head -n1
)"
printf 'SCRIPT_PATH=%s\n' "$script_path"
printf 'SCRIPT_SHA256=%s\n' "$(sha256sum "$script_path" | awk '{print $1}')"
printf 'SCRIPT_VERSION=%s\n' "$(
    "$timeout_path" --foreground --signal=TERM --kill-after=2s \
        10 "$script_path" --version |
        head -n1
)"
'@
$setupCommand = $setupTemplate.Replace(
    '__SOURCE_REPO__',
    (ConvertTo-BashLiteral $repoWsl)
).Replace(
    '__REVISION__',
    (ConvertTo-BashLiteral $resolvedRevision)
).Replace(
    '__GENERATOR_TIMEOUT__',
    '30'
).Replace(
    '__CARGO_FETCH_TIMEOUT__',
    '600'
).Replace(
    '__CARGO_BUILD_TIMEOUT__',
    '1200'
).Replace(
    '__PREFLIGHT_TIMEOUT__',
    '120'
)
$setupMarker = 'kettle-vtebench-' + (
    New-KettlePerfThroughputChannelRandomHex -ByteCount 32
)
$setupSucceeded = $false
try {
    $setupText = Invoke-KettlePerfWslBashCapture `
        -WslExe $WslExe -Distribution $WslDistribution `
        -Script $setupCommand -Marker $setupMarker `
        -TimeoutSec $SetupTimeoutSec `
        -MaximumOutputBytes 65536 -MaximumErrorBytes 0
    $setupSucceeded = $true
} finally {
    if (-not $setupSucceeded) {
        try {
            Stop-KettlePerfWslMarkedProcess `
                -WslExe $WslExe -Distribution $WslDistribution `
                -Marker $setupMarker
        } catch {
            Write-Warning (
                'vtebench setup descendant cleanup failed: ' +
                $_.Exception.Message
            )
        }
    }
}
$setupOutput = [object[]]@(
    $setupText.Replace("`r`n", "`n").Split([char]10) |
        Where-Object { $_ }
)
function Get-VtebenchSetupValue {
    param(
        [Parameter(Mandatory = $true)][object[]]$Lines,
        [Parameter(Mandatory = $true)][string]$Name
    )

    $prefix = "$Name="
    $line = @(
        $Lines | Where-Object { [string]$_ -like "$prefix*" }
    ) | Select-Object -Last 1
    if ($null -eq $line) {
        throw "WSL-local vtebench setup omitted the $Name marker"
    }
    $text = [string]$line
    if ($text.Length -le $prefix.Length) {
        throw "WSL-local vtebench setup returned an empty $Name marker"
    }
    return $text.Substring($prefix.Length)
}

$cacheWsl = Get-VtebenchSetupValue $setupOutput 'CACHE'
$buildRootWsl = Get-VtebenchSetupValue $setupOutput 'BUILD_ROOT'
$binaryWsl = Get-VtebenchSetupValue $setupOutput 'BINARY'
$expectedColumnsText = Get-VtebenchSetupValue $setupOutput 'EXPECTED'
$benchmarkTree = Get-VtebenchSetupValue $setupOutput 'BENCHMARK_TREE'
$wslBinarySha256 = Get-VtebenchSetupValue $setupOutput 'BINARY_SHA256'
$cargoLockSha256 = Get-VtebenchSetupValue $setupOutput 'LOCK_SHA256'
$wslCargoPath = Get-VtebenchSetupValue $setupOutput 'CARGO_PATH'
$wslCargoSha256 = Get-VtebenchSetupValue $setupOutput 'CARGO_SHA256'
$wslCargoVersion = Get-VtebenchSetupValue $setupOutput 'CARGO_VERSION'
$wslRustupPath = Get-VtebenchSetupValue $setupOutput 'RUSTUP_PATH'
$wslRustupSha256 = Get-VtebenchSetupValue $setupOutput 'RUSTUP_SHA256'
$wslRustupVersion = Get-VtebenchSetupValue $setupOutput 'RUSTUP_VERSION'
$wslTimeoutPath = Get-VtebenchSetupValue $setupOutput 'TIMEOUT_PATH'
$wslTimeoutSha256 = Get-VtebenchSetupValue $setupOutput 'TIMEOUT_SHA256'
$wslTimeoutVersion = Get-VtebenchSetupValue $setupOutput 'TIMEOUT_VERSION'
$wslSetsidPath = Get-VtebenchSetupValue $setupOutput 'SETSID_PATH'
$wslSetsidSha256 = Get-VtebenchSetupValue $setupOutput 'SETSID_SHA256'
$wslSetsidVersion = Get-VtebenchSetupValue $setupOutput 'SETSID_VERSION'
$wslScriptPath = Get-VtebenchSetupValue $setupOutput 'SCRIPT_PATH'
$wslScriptSha256 = Get-VtebenchSetupValue $setupOutput 'SCRIPT_SHA256'
$wslScriptVersion = Get-VtebenchSetupValue $setupOutput 'SCRIPT_VERSION'
$expectedColumns = 0
if (
    -not [int]::TryParse($expectedColumnsText, [ref]$expectedColumns) -or
    $expectedColumns -le 0 -or
    -not $cacheWsl -or
    -not $buildRootWsl -or
    -not $binaryWsl -or
    $benchmarkTree -notmatch '^[0-9a-f]{40}$' -or
    $wslBinarySha256 -notmatch '^[0-9a-f]{64}$' -or
    $cargoLockSha256 -notmatch '^[0-9a-f]{64}$' -or
    -not $wslCargoPath -or
    $wslCargoSha256 -notmatch '^[0-9a-f]{64}$' -or
    -not $wslCargoVersion -or
    -not $wslRustupPath -or
    $wslRustupSha256 -notmatch '^[0-9a-f]{64}$' -or
    -not $wslRustupVersion -or
    -not $wslTimeoutPath -or
    $wslTimeoutSha256 -notmatch '^[0-9a-f]{64}$' -or
    -not $wslTimeoutVersion -or
    -not $wslSetsidPath -or
    $wslSetsidSha256 -notmatch '^[0-9a-f]{64}$' -or
    -not $wslSetsidVersion -or
    -not $wslScriptPath -or
    $wslScriptSha256 -notmatch '^[0-9a-f]{64}$' -or
    -not $wslScriptVersion
) {
    throw 'WSL-local vtebench setup returned invalid provenance'
}

function Get-VtebenchSourceState {
    $stateTemplate = @'
set -euo pipefail
cache=__CACHE__
build_root=__BUILD_ROOT__
binary=__BINARY__
cargo_path=__CARGO_PATH__
rustup_path=__RUSTUP_PATH__
timeout_path=__TIMEOUT_PATH__
setsid_path=__SETSID_PATH__
script_path=__SCRIPT_PATH__
revision=__REVISION__
cache_real="$(realpath -e -- "$cache")"
build_real="$(realpath -e -- "$build_root")"
binary_real="$(realpath -e -- "$binary")"
cargo_real="$(realpath -e -- "$cargo_path")"
rustup_real="$(realpath -e -- "$rustup_path")"
timeout_real="$(realpath -e -- "$timeout_path")"
setsid_real="$(realpath -e -- "$setsid_path")"
script_real="$(realpath -e -- "$script_path")"
[[ "$cache_real" == "$cache" ]]
[[ "$build_real" == "$build_root" ]]
[[ "$binary_real" == "$binary" ]]
[[ "$cargo_real" == "$cargo_path" ]]
[[ "$rustup_real" == "$rustup_path" ]]
[[ "$timeout_real" == "$timeout_path" ]]
[[ "$setsid_real" == "$setsid_path" ]]
[[ "$script_real" == "$script_path" ]]
[[ -d "$cache_real" && ! -L "$cache" ]]
[[ -d "$build_real" && ! -L "$build_root" ]]
[[ -f "$binary_real" && -x "$binary_real" && ! -L "$binary" ]]
[[ -f "$cargo_real" && -x "$cargo_real" && ! -L "$cargo_path" ]]
[[ -f "$rustup_real" && -x "$rustup_real" && ! -L "$rustup_path" ]]
[[ -f "$timeout_real" && -x "$timeout_real" && ! -L "$timeout_path" ]]
[[ -f "$setsid_real" && -x "$setsid_real" && ! -L "$setsid_path" ]]
[[ -f "$script_real" && -x "$script_real" && ! -L "$script_path" ]]
[[ -f "$cache_real/Cargo.lock" && ! -L "$cache_real/Cargo.lock" ]]
[[ "$(git -C "$cache_real" rev-parse HEAD)" == "$revision" ]]
[[ -z "$(git -C "$cache_real" -c core.excludesFile=/dev/null \
    status --porcelain=v1 --untracked-files=all --ignored=matching)" ]]
git -C "$cache_real" diff --no-ext-diff --quiet "$revision" --
git -C "$cache_real" diff --no-ext-diff --cached --quiet "$revision" --
emit() {
    printf '%s=' "$1"
    printf '%s' "$2" | base64 -w0
    printf '\n'
}
emit CACHE "$cache_real"
emit BUILD_ROOT "$build_real"
emit BINARY "$binary_real"
emit REVISION "$(git -C "$cache_real" rev-parse HEAD)"
emit BENCHMARK_TREE "$(git -C "$cache_real" rev-parse "$revision:benchmarks")"
emit BINARY_SHA256 "$(sha256sum "$binary_real" | awk '{print $1}')"
emit LOCK_SHA256 "$(sha256sum "$cache_real/Cargo.lock" | awk '{print $1}')"
emit CARGO_PATH "$cargo_real"
emit CARGO_SHA256 "$(sha256sum "$cargo_real" | awk '{print $1}')"
emit CARGO_VERSION "$(
    "$timeout_real" --foreground --signal=TERM --kill-after=2s \
        10 "$cargo_real" --version
)"
emit RUSTUP_PATH "$rustup_real"
emit RUSTUP_SHA256 "$(sha256sum "$rustup_real" | awk '{print $1}')"
emit RUSTUP_VERSION "$(
    "$timeout_real" --foreground --signal=TERM --kill-after=2s \
        10 "$rustup_real" --version
)"
emit TIMEOUT_PATH "$timeout_real"
emit TIMEOUT_SHA256 "$(sha256sum "$timeout_real" | awk '{print $1}')"
emit TIMEOUT_VERSION "$(
    "$timeout_real" --foreground --signal=TERM --kill-after=2s \
        10 "$timeout_real" --version |
        head -n1
)"
emit SETSID_PATH "$setsid_real"
emit SETSID_SHA256 "$(sha256sum "$setsid_real" | awk '{print $1}')"
emit SETSID_VERSION "$(
    "$timeout_real" --foreground --signal=TERM --kill-after=2s \
        10 "$setsid_real" --version |
        head -n1
)"
emit SCRIPT_PATH "$script_real"
emit SCRIPT_SHA256 "$(sha256sum "$script_real" | awk '{print $1}')"
emit SCRIPT_VERSION "$(
    "$timeout_real" --foreground --signal=TERM --kill-after=2s \
        10 "$script_real" --version |
        head -n1
)"
'@
    $stateCommand = $stateTemplate.Replace(
        '__CACHE__',
        (ConvertTo-BashLiteral $cacheWsl)
    ).Replace(
        '__BUILD_ROOT__',
        (ConvertTo-BashLiteral $buildRootWsl)
    ).Replace(
        '__BINARY__',
        (ConvertTo-BashLiteral $binaryWsl)
    ).Replace(
        '__CARGO_PATH__',
        (ConvertTo-BashLiteral $wslCargoPath)
    ).Replace(
        '__RUSTUP_PATH__',
        (ConvertTo-BashLiteral $wslRustupPath)
    ).Replace(
        '__TIMEOUT_PATH__',
        (ConvertTo-BashLiteral $wslTimeoutPath)
    ).Replace(
        '__SETSID_PATH__',
        (ConvertTo-BashLiteral $wslSetsidPath)
    ).Replace(
        '__SCRIPT_PATH__',
        (ConvertTo-BashLiteral $wslScriptPath)
    ).Replace(
        '__REVISION__',
        (ConvertTo-BashLiteral $resolvedRevision)
    )
    $marker = 'kettle-vtebench-' + (
        New-KettlePerfThroughputChannelRandomHex -ByteCount 32
    )
    $succeeded = $false
    try {
        $stateText = Invoke-KettlePerfWslBashCapture `
            -WslExe $WslExe -Distribution $WslDistribution `
            -Script $stateCommand -Marker $marker `
            -TimeoutSec 30 -MaximumOutputBytes 65536
        $succeeded = $true
    } finally {
        if (-not $succeeded) {
            try {
                Stop-KettlePerfWslMarkedProcess `
                    -WslExe $WslExe `
                    -Distribution $WslDistribution -Marker $marker
            } catch {
                Write-Warning (
                    'vtebench source-state cleanup failed: ' +
                    $_.Exception.Message
                )
            }
        }
    }
    $stateLines = [string[]]@(
        $stateText.Replace("`r`n", "`n").Split([char]10) |
            Where-Object { $_ }
    )
    $state = [ordered]@{
        cache = Get-KettlePerfWslBase64MarkerValue `
            $stateLines 'CACHE'
        build_root = Get-KettlePerfWslBase64MarkerValue `
            $stateLines 'BUILD_ROOT'
        binary = Get-KettlePerfWslBase64MarkerValue `
            $stateLines 'BINARY'
        revision = Get-KettlePerfWslBase64MarkerValue `
            $stateLines 'REVISION'
        benchmark_tree = Get-KettlePerfWslBase64MarkerValue `
            $stateLines 'BENCHMARK_TREE'
        binary_sha256 = Get-KettlePerfWslBase64MarkerValue `
            $stateLines 'BINARY_SHA256'
        cargo_lock_sha256 = Get-KettlePerfWslBase64MarkerValue `
            $stateLines 'LOCK_SHA256'
        cargo_path = Get-KettlePerfWslBase64MarkerValue `
            $stateLines 'CARGO_PATH'
        cargo_sha256 = Get-KettlePerfWslBase64MarkerValue `
            $stateLines 'CARGO_SHA256'
        cargo_version = Get-KettlePerfWslBase64MarkerValue `
            $stateLines 'CARGO_VERSION'
        rustup_path = Get-KettlePerfWslBase64MarkerValue `
            $stateLines 'RUSTUP_PATH'
        rustup_sha256 = Get-KettlePerfWslBase64MarkerValue `
            $stateLines 'RUSTUP_SHA256'
        rustup_version = Get-KettlePerfWslBase64MarkerValue `
            $stateLines 'RUSTUP_VERSION'
        timeout_path = Get-KettlePerfWslBase64MarkerValue `
            $stateLines 'TIMEOUT_PATH'
        timeout_sha256 = Get-KettlePerfWslBase64MarkerValue `
            $stateLines 'TIMEOUT_SHA256'
        timeout_version = Get-KettlePerfWslBase64MarkerValue `
            $stateLines 'TIMEOUT_VERSION'
        setsid_path = Get-KettlePerfWslBase64MarkerValue `
            $stateLines 'SETSID_PATH'
        setsid_sha256 = Get-KettlePerfWslBase64MarkerValue `
            $stateLines 'SETSID_SHA256'
        setsid_version = Get-KettlePerfWslBase64MarkerValue `
            $stateLines 'SETSID_VERSION'
        script_path = Get-KettlePerfWslBase64MarkerValue `
            $stateLines 'SCRIPT_PATH'
        script_sha256 = Get-KettlePerfWslBase64MarkerValue `
            $stateLines 'SCRIPT_SHA256'
        script_version = Get-KettlePerfWslBase64MarkerValue `
            $stateLines 'SCRIPT_VERSION'
    }
    $expectedState = [ordered]@{
        cache = $cacheWsl
        build_root = $buildRootWsl
        binary = $binaryWsl
        revision = $resolvedRevision
        benchmark_tree = $benchmarkTree
        binary_sha256 = $wslBinarySha256
        cargo_lock_sha256 = $cargoLockSha256
        cargo_path = $wslCargoPath
        cargo_sha256 = $wslCargoSha256
        cargo_version = $wslCargoVersion
        rustup_path = $wslRustupPath
        rustup_sha256 = $wslRustupSha256
        rustup_version = $wslRustupVersion
        timeout_path = $wslTimeoutPath
        timeout_sha256 = $wslTimeoutSha256
        timeout_version = $wslTimeoutVersion
        setsid_path = $wslSetsidPath
        setsid_sha256 = $wslSetsidSha256
        setsid_version = $wslSetsidVersion
        script_path = $wslScriptPath
        script_sha256 = $wslScriptSha256
        script_version = $wslScriptVersion
    }
    $signatureText = [Text.StringBuilder]::new()
    foreach ($name in $expectedState.Keys) {
        if (
            -not [StringComparer]::Ordinal.Equals(
                [string]$state[$name],
                [string]$expectedState[$name]
            )
        ) {
            throw "vtebench WSL source state changed: $name"
        }
        [void]$signatureText.Append($name)
        [void]$signatureText.Append([char]0)
        [void]$signatureText.Append([string]$state[$name])
        [void]$signatureText.Append("`n")
    }
    $signatureBytes = [Text.UTF8Encoding]::new(
        $false,
        $true
    ).GetBytes($signatureText.ToString())
    try {
        $signature = Get-KettlePerfVtebenchBytesSha256 `
            -Bytes $signatureBytes
    } finally {
        [Array]::Clear(
            $signatureBytes,
            0,
            $signatureBytes.Length
        )
    }
    return [pscustomobject]@{
        Values = $state
        Signature = $signature
    }
}

$initialSourceState = Get-VtebenchSourceState

$powerShellLock = $null
$wrapperLock = $null
try {
    $powerShellLock = [IO.File]::Open(
        $PowerShellExe,
        [IO.FileMode]::Open,
        [IO.FileAccess]::Read,
        [IO.FileShare]::Read
    )
    $wrapperLock = [IO.File]::Open(
        $wrapper,
        [IO.FileMode]::Open,
        [IO.FileAccess]::Read,
        [IO.FileShare]::Read
    )
    $wrapperPowerShellVersion = (
        & $PowerShellExe -NoLogo -NoProfile -NonInteractive -Command `
            '[Console]::Out.Write($PSVersionTable.PSVersion.ToString())'
    ) -join ''
    if ($LASTEXITCODE -ne 0 -or -not $wrapperPowerShellVersion) {
        throw 'Could not identify the vtebench wrapper PowerShell version'
    }
    $workloadRunner = [ordered]@{
        schema = 'kettle-vtebench-runner-v1'
        powershell = [ordered]@{
            path = $PowerShellExe
            sha256 = (
                Get-FileHash -LiteralPath $PowerShellExe `
                    -Algorithm SHA256
            ).Hash
            version = $wrapperPowerShellVersion
        }
        script = [ordered]@{
            path = $wrapper
            sha256 = (
                Get-FileHash -LiteralPath $wrapper -Algorithm SHA256
            ).Hash
        }
    }
    if ($SetupOnly) {
        Write-Host (
            "vtebench setup valid: revision=$resolvedRevision " +
            "benchmarks=$expectedColumns binary=$binaryWsl " +
            "sha256=$wslBinarySha256"
        )
        return
    }

    $terminalSummaries = [ordered]@{}
    foreach ($terminal in $Terminals) {
    $dat = Join-Path $resolvedResults "vtebench-$terminal.dat"
    $isolatedConfig = Get-KettlePerfIsolatedConfigEntry `
        -ConfigProfile $IsolatedProfile -Name $terminal
    if ($terminal -eq 'kettle' -and $KettleConfig) {
        $isolatedConfig = $null
    }
    $spec = Resolve-KettlePerfTerminal -Name $terminal `
        -KettleExe $KettleExe -KettleConfig $KettleConfig `
        -AlacrittyExe $AlacrittyExe -WeztermExe $WeztermExe `
        -RioExe $RioExe -TabbyExe $TabbyExe `
        -IsolatedConfig $isolatedConfig
    if (-not $spec.Available) {
        Write-Warning "$terminal executable not found - skipping"
        continue
    }
    if (-not $spec.SupportsCommand) {
        Write-Warning (
            "$terminal has no command-launch contract - skipping vtebench"
        )
        continue
    }
    if (Test-Path -LiteralPath $dat) {
        throw (
            "$terminal vtebench DAT output is preplaced; " +
            'refusing to overwrite it'
        )
    }

    Write-Host ">> $terminal : running pinned WSL-local vtebench"
    $before = Get-VisibleWindowSet
    $prePids = Get-PidSet
    $launched = $null
    $channelDescriptor = $null
    $channelResult = $null
    $parsed = $null
    $publication = $null
    $sourceStateBefore = $null
    $sourceStateAfter = $null
    try {
        $sourceStateBefore = Get-VtebenchSourceState
        $channelDescriptor = New-KettlePerfVtebenchChannelDescriptor
        $inner = @(
            $PowerShellExe,
            '-NoLogo', '-NoProfile', '-ExecutionPolicy', 'Bypass',
            '-File', $wrapper,
            '-PipeName', $channelDescriptor.PipeName,
            '-PipeNonce', $channelDescriptor.Nonce,
            '-CacheWsl', $cacheWsl,
            '-BinaryWsl', $binaryWsl,
            '-BuildRootWsl', $buildRootWsl,
            '-SetsidWsl', $wslSetsidPath,
            '-SetsidSha256', $wslSetsidSha256,
            '-WslExe', $WslExe,
            '-WslDistribution', $WslDistribution,
            '-TimeoutSec', [string]$TimeoutSec
        )
        $launched = Start-KettlePerfCommandWindow -Spec $spec `
            -Command $inner -BeforeWindows $before `
            -PreexistingPids $prePids `
            -CommandWrapperDirectory $resolvedResults
        $proc = $launched.Process
        $hwnd = $launched.Hwnd
        $winPid = $launched.WindowPid
        Start-Sleep -Milliseconds 600
        Set-WindowSize $hwnd $WindowW $WindowH $TargetScreenDevice
        $channelResult = Receive-KettlePerfVtebenchChannelResult `
            -Descriptor $channelDescriptor `
            -ExpectedWorkloadPid ([int]$launched.TargetPid) `
            -ExpectedTerminalPid ([int]$launched.WindowPid) `
            -ExpectedColumns $expectedColumns `
            -ConnectTimeoutMs ([int]($TimeoutSec * 1000))
        $sourceStateAfter = Get-VtebenchSourceState
        $parsed = $channelResult.Parsed
        $publication = Publish-KettlePerfVtebenchDat `
            -Path $dat -ResultsDirectory $resolvedResults `
            -Bytes $channelResult.DatBytes
        $benchmarks = [ordered]@{}
        foreach ($name in $parsed.Names) {
            $samples = @($parsed.Samples[$name])
            $benchmarks[$name] = [ordered]@{
                samples_ms = $samples
                sample_count = $samples.Count
                median_ms = [Math]::Round(
                    (Get-KettlePerfMedian $samples),
                    3
                )
            }
        }
        $terminalSummaries[$terminal] = [ordered]@{
            run_id = $RunId
            launcher = $spec.Exe
            executable = $spec.BenchmarkExe
            executable_sha256 = Get-KettlePerfExecutableSha256 (
                $spec.BenchmarkExe
            )
            product_version = Get-KettlePerfVersion $spec
            configuration_mode = $spec.ConfigurationMode
            configuration_evidence = $spec.ConfigurationEvidence
            helper_binaries = [object[]]@($launched.HelperBinaries)
            workload_pid = $launched.TargetPid
            workload_executable = $launched.TargetExecutable
            source_state_before_sha256 = $sourceStateBefore.Signature
            source_state_after_sha256 = $sourceStateAfter.Signature
            dat_path = $publication.Path
            dat_sha256 = $publication.Sha256
            benchmark_count = $parsed.Names.Count
            sample_rows = $parsed.SampleRows
            benchmarks = $benchmarks
        }
        Write-Host (
            ">> $terminal : valid $($parsed.Names.Count)-column DAT written"
        )
    } finally {
        if (
            $null -ne $channelDescriptor -and
            $null -eq $channelResult
        ) {
            try {
                Stop-KettlePerfWslMarkedProcess `
                    -WslExe $WslExe `
                    -Distribution $WslDistribution `
                    -Marker ('kettle-vtebench-' + $channelDescriptor.Nonce)
            } catch {
                Write-Warning (
                    'parent vtebench WSL descendant cleanup failed: ' +
                    $_.Exception.Message
                )
            }
        }
        Close-KettlePerfThroughputChannel $channelDescriptor
        if (
            $null -ne $channelResult -and
            $null -ne $channelResult.DatBytes
        ) {
            [Array]::Clear(
                $channelResult.DatBytes,
                0,
                $channelResult.DatBytes.Length
            )
        }
        if ($null -ne $launched) {
            [void](Close-SpawnedTerminal -Hwnd $launched.Hwnd `
                -ExpectedPid $launched.WindowPid `
                -PreexistingPids $prePids)
            try {
                if (-not $launched.Process.HasExited) {
                    Stop-Process -Id $launched.Process.Id -Force
                }
            } catch {
                Write-Verbose (
                    'vtebench launcher cleanup raced process exit: ' +
                    $_.Exception.Message
                )
            }
            if ($null -ne $launched.CommandWrapper) {
                Close-KettlePerfCommandWrapper `
                    $launched.CommandWrapper
            }
            if ($null -ne $launched.ExecutableLease) {
                Close-KettlePerfExecutableLease `
                    $launched.ExecutableLease
            }
        }
    }
    Start-Sleep -Seconds 1
}

    $summary = [ordered]@{
        schema_version = 2
        run_id = $RunId
        transport_schema = $script:KettlePerfVtebenchChannelSchema
        workload_runner = $workloadRunner
        source = [ordered]@{
            origin = $sourceOrigin
            windows_repository = $resolvedRepo
            revision = $resolvedRevision
            benchmark_tree = $benchmarkTree
            wsl_cache = $cacheWsl
            wsl_build_root = $buildRootWsl
            wsl_binary = $binaryWsl
            wsl_binary_sha256 = $wslBinarySha256
            cargo_lock_sha256 = $cargoLockSha256
            cargo_path = $wslCargoPath
            cargo_sha256 = $wslCargoSha256
            cargo_version = $wslCargoVersion
            rustup_path = $wslRustupPath
            rustup_sha256 = $wslRustupSha256
            rustup_version = $wslRustupVersion
            timeout_path = $wslTimeoutPath
            timeout_sha256 = $wslTimeoutSha256
            timeout_version = $wslTimeoutVersion
            setsid_path = $wslSetsidPath
            setsid_sha256 = $wslSetsidSha256
            setsid_version = $wslSetsidVersion
            script_path = $wslScriptPath
            script_sha256 = $wslScriptSha256
            script_version = $wslScriptVersion
            source_state_schema = 'kettle-vtebench-source-state-v1'
            source_state_sha256 = $initialSourceState.Signature
            deadlines_seconds = [ordered]@{
                setup = $SetupTimeoutSec
                generator = 30
                cargo_fetch = 600
                cargo_build = 1200
                preflight = 120
                source_validation = 30
                workload = $TimeoutSec
                cleanup = $CleanupTimeoutSec
            }
            expected_benchmark_count = $expectedColumns
            wsl_launcher = [ordered]@{
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
        }
        terminals = $terminalSummaries
    }
    Write-KettlePerfJsonFile `
        -Path (Join-Path $resolvedResults 'vtebench-summary.json') `
        -InputObject $summary -Depth 10
    Write-Host 'done - validated vtebench samples in vtebench-summary.json'
} finally {
    if ($null -ne $wrapperLock) {
        $wrapperLock.Dispose()
    }
    if ($null -ne $powerShellLock) {
        $powerShellLock.Dispose()
    }
    if ($buildRootWsl) {
        $cleanupTemplate = @'
set -euo pipefail
root=__BUILD_ROOT__
revision=__REVISION__
parent="$(realpath -e -- "$HOME/.cache/kettle-perf")"
resolved="$(realpath -e -- "$root")"
case "$resolved" in
    "$parent"/vtebench-build-"$revision".*) ;;
    *)
        echo "refusing to remove unexpected vtebench build root: $resolved" >&2
        exit 70
        ;;
esac
[[ -d "$resolved" && ! -L "$root" ]]
rm -rf --one-file-system -- "$resolved"
'@
        $cleanupCommand = $cleanupTemplate.Replace(
            '__BUILD_ROOT__',
            (ConvertTo-BashLiteral $buildRootWsl)
        ).Replace(
            '__REVISION__',
            (ConvertTo-BashLiteral $resolvedRevision)
        )
        $cleanupMarker = 'kettle-vtebench-' + (
            New-KettlePerfThroughputChannelRandomHex -ByteCount 32
        )
        $cleanupSucceeded = $false
        try {
            [void](Invoke-KettlePerfWslBashCapture `
                -WslExe $WslExe -Distribution $WslDistribution `
                -Script $cleanupCommand -Marker $cleanupMarker `
                -TimeoutSec $CleanupTimeoutSec `
                -MaximumOutputBytes 4096 -MaximumErrorBytes 4096)
            $cleanupSucceeded = $true
        } catch {
            Write-Warning (
                "could not remove isolated vtebench build root " +
                "$buildRootWsl`: $($_.Exception.Message)"
            )
        } finally {
            if (-not $cleanupSucceeded) {
                try {
                    Stop-KettlePerfWslMarkedProcess `
                        -WslExe $WslExe `
                        -Distribution $WslDistribution `
                        -Marker $cleanupMarker
                } catch {
                    Write-Warning (
                        'vtebench build-root descendant cleanup failed: ' +
                        $_.Exception.Message
                    )
                }
            }
        }
    }
}
} finally {
    $wslLauncherEvidence.Stream.Dispose()
}
