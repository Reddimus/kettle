# Windows-side terminal workload wrapper for vtebench. WSL stdout stays live
# on the terminal PTY; a bounded binary stderr frame carries completion and DAT
# bytes privately to this process, which authenticates them to the parent pipe.
param(
    [Parameter(Mandatory)]
    [ValidatePattern('^kettle-perf-vtebench-[0-9a-f]{48}$')]
    [string]$PipeName,
    [Parameter(Mandatory)]
    [ValidatePattern('^[0-9a-f]{64}$')]
    [string]$PipeNonce,
    [Parameter(Mandatory)]
    [string]$CacheWsl,
    [Parameter(Mandatory)]
    [string]$BinaryWsl,
    [Parameter(Mandatory)]
    [string]$BuildRootWsl,
    [Parameter(Mandatory)]
    [string]$SetsidWsl,
    [Parameter(Mandatory)]
    [ValidatePattern('^[0-9a-f]{64}$')]
    [string]$SetsidSha256,
    [Parameter(Mandatory)]
    [string]$WslExe,
    [Parameter(Mandatory)]
    [string]$WslDistribution,
    [ValidateRange(1, 86400)]
    [int]$TimeoutSec = 900,
    [ValidateRange(1024, 1048576)]
    [int]$MaximumDatBytes = 1MB
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. "$PSScriptRoot\vtebench-channel.ps1"
. "$PSScriptRoot\wsl-launcher.ps1"

function Assert-KettlePerfVtebenchWslPath {
    param(
        [Parameter(Mandatory)]
        [string]$Path,
        [Parameter(Mandatory)]
        [string]$Name
    )

    if (
        -not $Path.StartsWith('/', [StringComparison]::Ordinal) -or
        $Path.Length -gt 4096 -or
        $Path.Contains([char]0) -or
        $Path.Contains("`r") -or
        $Path.Contains("`n")
    ) {
        throw "vtebench wrapper received an invalid $Name path"
    }
    return $Path
}

function ConvertTo-KettlePerfVtebenchBashLiteral {
    param(
        [Parameter(Mandatory)]
        [string]$Value
    )

    $singleQuoteEscape = "'" + '"' + "'" + '"' + "'"
    return "'" + $Value.Replace("'", $singleQuoteEscape) + "'"
}

function ConvertTo-KettlePerfVtebenchWslCommand {
    param(
        [Parameter(Mandatory)]
        [string]$Script
    )

    $encoded = [Convert]::ToBase64String(
        [Text.Encoding]::UTF8.GetBytes($Script)
    )
    return "printf '%s' '$encoded' | base64 --decode | bash"
}

$CacheWsl = Assert-KettlePerfVtebenchWslPath `
    -Path $CacheWsl -Name cache
$BinaryWsl = Assert-KettlePerfVtebenchWslPath `
    -Path $BinaryWsl -Name binary
$BuildRootWsl = Assert-KettlePerfVtebenchWslPath `
    -Path $BuildRootWsl -Name build-root
$SetsidWsl = Assert-KettlePerfVtebenchWslPath `
    -Path $SetsidWsl -Name setsid
$PipeName = Assert-KettlePerfThroughputChannelName $PipeName
$PipeNonce = Assert-KettlePerfThroughputChannelNonce $PipeNonce
$WslExe = Resolve-KettlePerfWslLauncherPath -Path $WslExe
$wslEvidence = Open-KettlePerfWslLauncherEvidence `
    -Path $WslExe -ResolutionPolicy explicit-override-v1
$WslDistribution = Resolve-KettlePerfWslDistribution `
    -WslExe $WslExe -Name $WslDistribution
$marker = 'kettle-vtebench-' + $PipeNonce

$bashTemplate = @'
set -euo pipefail
exec 3>&2
exec 2>&1
cache=__CACHE__
binary=__BINARY__
build_root=__BUILD_ROOT__
setsid_path=__SETSID__
setsid_sha256=__SETSID_SHA256__
maximum_bytes=__MAXIMUM_BYTES__
marker=__MARKER__
dat="$(mktemp "$build_root/vtebench-result.XXXXXX.dat")"
cleanup() {
    rm -f -- "$dat"
}
trap cleanup EXIT
cd -- "$cache"
setsid_real="$(realpath -e -- "$setsid_path")"
[[ "$setsid_real" == "$setsid_path" ]]
[[ -f "$setsid_real" && -x "$setsid_real" && ! -L "$setsid_path" ]]
[[ "$(sha256sum "$setsid_real" | awk '{print $1}')" == "$setsid_sha256" ]]
set +e
"$setsid_real" --fork --wait \
    bash -c 'exec -a "$1" "$2" --silent --dat "$3"' \
    _ "$marker" "$binary" "$dat"
status=$?
set -e
if [[ ! -f "$dat" || -L "$dat" ]]; then
    status=125
    rm -f -- "$dat"
    printf '\n' > "$dat"
fi
bytes="$(stat --printf='%s' -- "$dat" 2>/dev/null || printf '0')"
if [[ ! "$bytes" =~ ^[0-9]+$ ]] ||
   (( bytes <= 0 || bytes > maximum_bytes )); then
    status=125
    printf '\n' > "$dat"
    bytes=1
fi
emit_byte() {
    local value="$1"
    printf "\\$(printf '%03o' "$((value & 255))")" >&3
}
emit_u32() {
    local value="$1"
    local index
    for ((index = 0; index < 4; index++)); do
        emit_byte "$value"
        value=$((value >> 8))
    done
}
emit_u64() {
    local value="$1"
    local index
    for ((index = 0; index < 8; index++)); do
        emit_byte "$value"
        value=$((value >> 8))
    done
}
printf 'KVD1' >&3
emit_u32 "$status"
emit_u64 "$bytes"
cat -- "$dat" >&3
exec 3>&-
exit "$status"
'@
$bash = $bashTemplate.Replace(
    '__CACHE__',
    (ConvertTo-KettlePerfVtebenchBashLiteral $CacheWsl)
).Replace(
    '__BINARY__',
    (ConvertTo-KettlePerfVtebenchBashLiteral $BinaryWsl)
).Replace(
    '__BUILD_ROOT__',
    (ConvertTo-KettlePerfVtebenchBashLiteral $BuildRootWsl)
).Replace(
    '__SETSID__',
    (ConvertTo-KettlePerfVtebenchBashLiteral $SetsidWsl)
).Replace(
    '__SETSID_SHA256__',
    (ConvertTo-KettlePerfVtebenchBashLiteral $SetsidSha256)
).Replace(
    '__MAXIMUM_BYTES__',
    $MaximumDatBytes.ToString(
        [Globalization.CultureInfo]::InvariantCulture
    )
).Replace(
    '__MARKER__',
    (ConvertTo-KettlePerfVtebenchBashLiteral $marker)
)
$encodedCommand = ConvertTo-KettlePerfVtebenchWslCommand $bash
$startInfo = [Diagnostics.ProcessStartInfo]::new()
$startInfo.FileName = $WslExe
# The only dynamic shell argument is base64. It cannot terminate the quoted
# Windows argv element or inject Bash syntax.
$startInfo.Arguments = Get-KettlePerfWslBashArguments `
    -Distribution $WslDistribution `
    -EncodedCommand $encodedCommand
$startInfo.UseShellExecute = $false
$startInfo.CreateNoWindow = $false
$startInfo.RedirectStandardOutput = $false
$startInfo.RedirectStandardError = $true
$process = [Diagnostics.Process]::new()
$process.StartInfo = $startInfo
$frame = $null
$started = $false
$completed = $false
try {
    if (-not $process.Start()) {
        throw 'Could not start the pinned WSL vtebench workload'
    }
    $started = $true
    try {
        $frame = Read-KettlePerfVtebenchPrivateFrame `
            -Stream $process.StandardError.BaseStream `
            -MaximumDatBytes $MaximumDatBytes `
            -TimeoutMs ([int]($TimeoutSec * 1000))
        if (-not $process.WaitForExit(5000)) {
            throw 'WSL vtebench did not exit after its private result frame'
        }
        if ($process.ExitCode -ne $frame.Status) {
            throw (
                'WSL vtebench process exit differs from its private frame ' +
                "($($process.ExitCode) != $($frame.Status))"
            )
        }
        Send-KettlePerfVtebenchChannelResult `
            -PipeName $PipeName -Nonce $PipeNonce `
            -Status $frame.Status -DatBytes $frame.DatBytes
        $completed = $true
    } finally {
        if (-not $completed) {
            try {
                Stop-KettlePerfWslMarkedProcess `
                    -WslExe $WslExe `
                    -Distribution $WslDistribution `
                    -Marker $marker
            } catch {
                Write-Warning (
                    'vtebench WSL descendant cleanup failed: ' +
                    $_.Exception.Message
                )
            }
        }
        if ($started -and -not $process.HasExited) {
            try {
                $process.Kill()
            } catch {
                Write-Verbose (
                    'vtebench wrapper cleanup raced process exit: ' +
                    $_.Exception.Message
                )
            }
            [void]$process.WaitForExit(5000)
        }
    }
} finally {
    if ($null -ne $frame -and $null -ne $frame.DatBytes) {
        [Array]::Clear(
            $frame.DatBytes,
            0,
            $frame.DatBytes.Length
        )
    }
    $process.Dispose()
    $wslEvidence.Stream.Dispose()
}
