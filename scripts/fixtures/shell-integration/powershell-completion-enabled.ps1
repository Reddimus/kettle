param([Parameter(Mandatory = $true)][string]$IntegrationPath)

$ErrorActionPreference = 'Stop'
Remove-Variable __kettle_prompt_installed -Scope Global -ErrorAction SilentlyContinue
Import-Module PSReadLine -ErrorAction Stop
$env:KETTLE_COMPLETION_OVERLAY = '1'
$env:TMUX = ''
$env:STY = ''
Set-PSReadLineKeyHandler -Key Tab -Function TabCompleteNext
Set-PSReadLineKeyHandler -Chord Shift+Tab -Function TabCompletePrevious
. $IntegrationPath

$tabHandler = Get-PSReadLineKeyHandler -Bound |
    Where-Object { $_.Key -eq 'Tab' } |
    Select-Object -First 1
if ($null -eq $tabHandler -or $tabHandler.Function -ne 'KettleCompleteNext') {
    throw 'completion-overlay = auto did not replace the stock Tab handler'
}
$backtabHandler = Get-PSReadLineKeyHandler -Bound |
    Where-Object { $_.Key -eq 'Shift+Tab' } |
    Select-Object -First 1
if ($null -eq $backtabHandler -or $backtabHandler.Function -ne 'KettleCompletePrevious') {
    throw 'completion-overlay = auto did not replace the stock Shift+Tab handler'
}
if ($null -eq (Get-Command __kettle_completion_cycle_next -ErrorAction SilentlyContinue)) {
    throw 'completion cycle helper was not installed'
}

$tooManyMatches = @(
    1..66 | ForEach-Object {
        [pscustomobject]@{
            CompletionText = "item-$($_.ToString('00'))"
            ToolTip = "description-$_"
            ResultType = 'Text'
        }
    }
)
$tooManyResult = [pscustomobject]@{
    CompletionMatches = $tooManyMatches
    ReplacementIndex = 0
    ReplacementLength = 0
}
if (-not (__kettle_completion_capture_result $tooManyResult)) {
    throw 'a multi-page result unexpectedly fell back to PSReadLine'
}
if ($global:__kettle_completion_matches.Count -ne 66 -or
    $global:__kettle_completion_rows.Count -ne 66) {
    throw 'a multi-page result was truncated before paging'
}
$global:__kettle_completion_session = [uint64]12
$global:__kettle_completion_request = [uint64]7
__kettle_completion_emit 65 'update'

$wireLimitMatches = @(
    1..64 | ForEach-Object {
        [pscustomobject]@{
            CompletionText = ('é' * 32)
            ToolTip = ('é' * 128)
            ResultType = 'Text'
        }
    }
)
$wireLimitResult = [pscustomobject]@{
    CompletionMatches = $wireLimitMatches
    ReplacementIndex = 0
    ReplacementLength = 0
}
if (-not (__kettle_completion_capture_result $wireLimitResult)) {
    throw 'a maximum-width page unexpectedly fell back to PSReadLine'
}
$stdout = [Console]::Out
$wireCapture = [System.IO.StringWriter]::new()
[Console]::SetOut($wireCapture)
try { __kettle_completion_emit 63 'update' } finally { [Console]::SetOut($stdout) }
$wirePayload = $wireCapture.ToString()
if ($wirePayload.Length -gt 65538 -or
    $wirePayload -notmatch ';completion;63;powershell;0;64;') {
    throw 'the PowerShell page exceeded the parser cap or lost its selected row'
}

$unboundedResult = [pscustomobject]@{
    CompletionMatches = @(1..2049 | ForEach-Object {
        [pscustomobject]@{ CompletionText = "wide-$_"; ToolTip = ''; ResultType = 'Text' }
    })
    ReplacementIndex = 0
    ReplacementLength = 0
}
if (-not (__kettle_completion_capture_result $unboundedResult)) {
    throw 'an over-count result should retain a bounded detached prefix'
}
if ($global:__kettle_completion_matches.Count -ne 2048 -or
    $global:__kettle_completion_matches[0].CompletionText -ne 'wide-1' -or
    $global:__kettle_completion_matches[2047].CompletionText -ne 'wide-2048') {
    throw 'the PowerShell count cap did not retain exactly its bounded prefix'
}

$oversizedResult = [pscustomobject]@{
    CompletionMatches = @([pscustomobject]@{
        CompletionText = 'x' * 4097
        ToolTip = ''
        ResultType = 'Text'
    })
    ReplacementIndex = 0
    ReplacementLength = 0
}
if (-not (__kettle_completion_capture_result $oversizedResult) -or
    $global:__kettle_completion_matches.Count -ne 0) {
    throw 'an oversized source field entered retained completion state'
}

$aggregateResult = [pscustomobject]@{
    CompletionMatches = @(1..100 | ForEach-Object {
        [pscustomobject]@{
            CompletionText = ('x' * 3996) + $_.ToString('0000')
            ToolTip = ''
            ResultType = 'Text'
        }
    })
    ReplacementIndex = 0
    ReplacementLength = 0
}
if (-not (__kettle_completion_capture_result $aggregateResult) -or
    $global:__kettle_completion_matches.Count -ne 65) {
    throw 'the PowerShell aggregate source-byte cap was not enforced'
}

$fakeMatches = @(1..64 | ForEach-Object {
    [pscustomobject]@{
        CompletionText = "item-$($_.ToString('00'))"
        ToolTip = "description-$_"
        ResultType = 'Text'
    }
})
$fakeResult = [pscustomobject]@{
    CompletionMatches = $fakeMatches
    ReplacementIndex = 0
    ReplacementLength = 0
}
if (-not (__kettle_completion_capture_result $fakeResult)) {
    throw 'a complete bounded result unexpectedly fell back to PSReadLine'
}
if ($global:__kettle_completion_matches.Count -ne 64 -or
    $global:__kettle_completion_rows.Count -ne 64 -or
    $global:__kettle_completion_matches[0].CompletionText -ne 'item-01' -or
    $global:__kettle_completion_matches[63].CompletionText -ne 'item-64') {
    throw 'the visible and inserted PowerShell completion sets diverged'
}

$container = [pscustomobject]@{
    CompletionText = "'folder'"
    ResultType = 'ProviderContainer'
}
$replacement = __kettle_completion_replacement $container
$separator = [string][IO.Path]::DirectorySeparatorChar
if ([string]$replacement[0] -ne "'folder$separator'" -or [int]$replacement[1] -ne -1) {
    throw 'provider-container completion did not preserve the closing quote'
}

# Exercise the real cycle through an in-memory editor boundary. This proves the
# installed handler increments its request before publishing and that the row
# used for the edit is the row sent to the detached card.
$global:__fixture_line = 'Get-I'
$global:__fixture_cursor = $global:__fixture_line.Length
function global:__kettle_completion_editor_state {
    return [pscustomobject]@{
        Line = $global:__fixture_line
        Cursor = $global:__fixture_cursor
    }
}
function global:__kettle_completion_expand([string]$Line, [int]$Cursor) {
    if ($Line -ne 'Get-I' -or $Cursor -ne 5) {
        throw "unexpected mock expansion input: $Line at $Cursor"
    }
    return [pscustomobject]@{
        CompletionMatches = @(
            [pscustomobject]@{
                CompletionText = 'Get-Item'
                ToolTip = 'Gets an item'
                ResultType = 'Command'
            },
            [pscustomobject]@{
                CompletionText = 'Get-ItemProperty'
                ToolTip = 'Gets a property'
                ResultType = 'Command'
            }
        )
        ReplacementIndex = 0
        ReplacementLength = 5
    }
}
function global:__kettle_completion_apply_replacement(
    [int]$Index,
    [int]$Length,
    [string]$Text
) {
    $global:__fixture_line =
        $global:__fixture_line.Substring(0, $Index) + $Text +
        $global:__fixture_line.Substring($Index + $Length)
    $global:__fixture_cursor = $Index + $Text.Length
}
function global:__kettle_completion_set_cursor([int]$Position) {
    $global:__fixture_cursor = $Position
}

$global:__kettle_completion_session = [uint64]21
$global:__kettle_completion_request = [uint64]0
__kettle_completion_reset_cycle
$stdout = [Console]::Out
$capture = [System.IO.StringWriter]::new()
[Console]::SetOut($capture)
try { __kettle_completion_handle_next } finally { [Console]::SetOut($stdout) }
$firstCycleWire = $capture.ToString()
if ($global:__fixture_line -ne 'Get-Item' -or
    [uint64]$global:__kettle_completion_request -ne 1 -or
    $firstCycleWire -notmatch
        '\]777;kettle-completion;3;show;21;[0-9]+;1;completion;0;powershell;0;2;Get-Item;Gets%20an%20item') {
    throw 'the first real completion cycle did not edit and publish request 1'
}

$capture = [System.IO.StringWriter]::new()
[Console]::SetOut($capture)
try { __kettle_completion_handle_next } finally { [Console]::SetOut($stdout) }
$secondCycleWire = $capture.ToString()
if ($global:__fixture_line -ne 'Get-ItemProperty' -or
    [uint64]$global:__kettle_completion_request -ne 2 -or
    $secondCycleWire -notmatch
        '\]777;kettle-completion;3;update;21;[0-9]+;2;completion;1;powershell;0;2;') {
    throw 'the second real completion cycle did not edit and publish request 2'
}

$maxExactRequest = [uint64]4503599627370495
$global:__kettle_completion_request = $maxExactRequest - [uint64]1
if (-not (__kettle_completion_begin_request) -or
    [uint64]$global:__kettle_completion_request -ne $maxExactRequest) {
    throw 'the final exact request id was not admitted'
}
if (__kettle_completion_begin_request) {
    throw 'request rollover reused an exhausted completion id'
}
if ([uint64]$global:__kettle_completion_request -ne $maxExactRequest -or
    $global:__kettle_completion_matches.Count -ne 0) {
    throw 'request exhaustion did not fail closed'
}
[Console]::Write($firstCycleWire)
[Console]::Write($secondCycleWire)

# Exercise the installed handlers' failure boundary without requiring a live
# console editor. A provider/editor exception must clear detached state and
# return normally; calling a stock PSReadLine action here would make inline UI
# possible again and is separately rejected by the portable source guard.
function global:__kettle_completion_cycle_next { throw 'synthetic provider failure' }
$generation = [uint64]$global:__kettle_completion_generation
__kettle_completion_handle_next
if ([uint64]$global:__kettle_completion_generation -ne $generation + 1 -or
    $global:__kettle_completion_matches.Count -ne 0) {
    throw 'the forward completion handler did not clear state after failure'
}

function global:__kettle_completion_cycle_previous { throw 'synthetic provider failure' }
$generation = [uint64]$global:__kettle_completion_generation
__kettle_completion_handle_previous
if ([uint64]$global:__kettle_completion_generation -ne $generation + 1 -or
    $global:__kettle_completion_matches.Count -ne 0) {
    throw 'the reverse completion handler did not clear state after failure'
}

$global:__kettle_completion_enabled = $true
$global:__kettle_completion_generation = $global:__kettle_completion_counter_max
if (__kettle_completion_begin_generation -or $global:__kettle_completion_enabled) {
    throw 'generation rollover did not fail the completion side channel closed'
}

Write-Output 'KETTLE_POWERSHELL_COMPLETION_ENABLED_OK'
