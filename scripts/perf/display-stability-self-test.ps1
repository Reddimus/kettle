param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

. "$PSScriptRoot\display-stability.ps1"

function New-KettlePerfDisplayStabilityTestSnapshot {
    param(
        [Parameter(Mandatory)]
        [string]$Signature
    )

    return [pscustomobject][ordered]@{
        schema = 'kettle-display-topology-snapshot-v2'
        signature_sha256 = $Signature
    }
}

Initialize-KettlePerfDisplayStabilityNativeType -IncludeSelfTestProbe

$race = [KettlePerfDisplayStabilityTest.Race]::Run()
if (
    -not $race.DisposeWaitedForCallback -or
    $race.CallbackCountBeforePostDisposeRaise -ne 1 -or
    $race.CallbackCountAfterPostDisposeRaise -ne 1 -or
    $race.DispatcherBarrierCount -ne 1
) {
    throw (
        'Display stability disposal did not cross the dispatcher barrier and ' +
        'drain one already-started callback before finalization'
    )
}

$signature = 'a' * 64
$monitor = Start-KettlePerfDisplayStabilityMonitor `
    -RunId ([Guid]::NewGuid().ToString('D'))
try {
    if (-not $monitor.registration_succeeded) {
        throw (
            'The Windows display stability event provider was unavailable: ' +
            [string]$monitor.registration_error_type
        )
    }
    $checkpoints = @(
        [pscustomobject][ordered]@{
            phase = 'start'
            snapshot = New-KettlePerfDisplayStabilityTestSnapshot $signature
        },
        [pscustomobject][ordered]@{
            phase = 'end'
            snapshot = New-KettlePerfDisplayStabilityTestSnapshot $signature
        }
    )
    $liveEvidenceRejected = $false
    try {
        [void](Get-KettlePerfDisplayStabilityEvidence `
            -Monitor $monitor -InitialSignature $signature `
            -Checkpoints $checkpoints)
    } catch {
        $liveEvidenceRejected = (
            $_.Exception.Message -like (
                '*cannot be finalized before the monitor is stopped*'
            )
        )
    }
    if (-not $liveEvidenceRejected) {
        throw 'Display stability evidence was finalized before callback drain'
    }
    [void](Stop-KettlePerfDisplayStabilityMonitor -Monitor $monitor)
    $evidence = Get-KettlePerfDisplayStabilityEvidence `
        -Monitor $monitor -InitialSignature $signature `
        -Checkpoints $checkpoints
    if (
        $evidence.schema -cne 'kettle-display-stability-evidence-v1' -or
        $evidence.monitoring_active_for_run -ne $true -or
        $evidence.stable -ne $true -or
        @($evidence.display_change_events).Count -ne 0
    ) {
        throw 'Stable display checkpoints were rejected'
    }

    $monitor.queue.Enqueue([pscustomobject][ordered]@{
        kind = 'display-settings-changed'
        captured_at_utc = [DateTimeOffset]::UtcNow.ToString('o')
    })
    $eventEvidence = Get-KettlePerfDisplayStabilityEvidence `
        -Monitor $monitor -InitialSignature $signature `
        -Checkpoints $checkpoints
    if (
        $eventEvidence.stable -ne $false -or
        @($eventEvidence.display_change_events).Count -ne 1
    ) {
        throw 'An intervening display-change event did not invalidate evidence'
    }

    $monitor.queue.TryDequeue([ref]$null) | Out-Null
    $changed = @(
        $checkpoints[0],
        [pscustomobject][ordered]@{
            phase = 'end'
            snapshot = New-KettlePerfDisplayStabilityTestSnapshot ('b' * 64)
        }
    )
    $changedEvidence = Get-KettlePerfDisplayStabilityEvidence `
        -Monitor $monitor -InitialSignature $signature `
        -Checkpoints $changed
    if (
        $changedEvidence.stable -ne $false -or
        @($changedEvidence.invalid_checkpoint_phases) -cnotcontains 'end'
    ) {
        throw 'A changed checkpoint signature did not invalidate evidence'
    }
} finally {
    [void](Stop-KettlePerfDisplayStabilityMonitor -Monitor $monitor)
}

if (-not $monitor.stopped) {
    throw 'Display stability monitor was not stopped'
}

Write-Output (
    'display-stability self-test: PASS ' +
    "($($PSVersionTable.PSVersion))"
)
