# Continuous display-change monitoring for Windows performance acquisition.
# Endpoint snapshots alone cannot prove stability because a user can switch
# away from a topology and later return to the same topology.

Set-StrictMode -Version Latest

function Initialize-KettlePerfDisplayStabilityNativeType {
    param([switch]$IncludeSelfTestProbe)

    if ('KettlePerf.DisplayStabilitySubscription' -as [type]) {
        if (
            $IncludeSelfTestProbe -and
            -not ('KettlePerfDisplayStabilityTest.Race' -as [type])
        ) {
            throw (
                'Display stability native type was initialized without its ' +
                'self-test probe'
            )
        }
        return
    }
    $source = @'
using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Globalization;
using System.Reflection;
using System.Threading;

namespace KettlePerf {
    public sealed class DisplayStabilitySubscription : IDisposable {
        private readonly object gate = new object();
        private readonly ConcurrentQueue<object> queue;
        private readonly EventInfo displaySettingsChanged;
        private readonly MethodInfo invokeOnEventsThread;
        private readonly EventHandler handler;
        private readonly ManualResetEventSlim callbackEnteredForTesting;
        private readonly ManualResetEventSlim callbackReleaseForTesting;
        private readonly ManualResetEventSlim disposeWaitingForTesting;
        private bool acceptingCallbacks = true;
        private bool disposeStarted;
        private bool disposed;
        private int callbacksInFlight;

        public DisplayStabilitySubscription(
            Type systemEventsType,
            ConcurrentQueue<object> queue)
            : this(systemEventsType, queue, null, null, null) {
        }

        public DisplayStabilitySubscription(
            Type systemEventsType,
            ConcurrentQueue<object> queue,
            ManualResetEventSlim callbackEnteredForTesting,
            ManualResetEventSlim callbackReleaseForTesting,
            ManualResetEventSlim disposeWaitingForTesting) {
            if (systemEventsType == null) {
                throw new ArgumentNullException("systemEventsType");
            }
            if (queue == null) {
                throw new ArgumentNullException("queue");
            }
            this.queue = queue;
            displaySettingsChanged = systemEventsType.GetEvent(
                "DisplaySettingsChanged",
                BindingFlags.Public | BindingFlags.Static);
            if (displaySettingsChanged == null) {
                throw new InvalidOperationException(
                    "DisplaySettingsChanged event is unavailable");
            }
            invokeOnEventsThread = systemEventsType.GetMethod(
                "InvokeOnEventsThread",
                BindingFlags.Public | BindingFlags.Static,
                null,
                new Type[] { typeof(Delegate) },
                null);
            if (invokeOnEventsThread == null) {
                throw new InvalidOperationException(
                    "SystemEvents dispatcher barrier is unavailable");
            }
            this.callbackEnteredForTesting = callbackEnteredForTesting;
            this.callbackReleaseForTesting = callbackReleaseForTesting;
            this.disposeWaitingForTesting = disposeWaitingForTesting;
            handler = new EventHandler(OnDisplaySettingsChanged);
            displaySettingsChanged.AddEventHandler(null, handler);
        }

        private void OnDisplaySettingsChanged(object sender, EventArgs args) {
            lock (gate) {
                if (!acceptingCallbacks) {
                    return;
                }
                callbacksInFlight++;
            }
            try {
                if (callbackEnteredForTesting != null) {
                    callbackEnteredForTesting.Set();
                }
                if (callbackReleaseForTesting != null) {
                    callbackReleaseForTesting.Wait();
                }
                queue.Enqueue(new Dictionary<string, object> {
                    { "kind", "display-settings-changed" },
                    {
                        "captured_at_utc",
                        DateTimeOffset.UtcNow.ToString(
                            "o",
                        CultureInfo.InvariantCulture)
                    }
                });
            } finally {
                lock (gate) {
                    callbacksInFlight--;
                    if (callbacksInFlight == 0) {
                        Monitor.PulseAll(gate);
                    }
                }
            }
        }

        public void Dispose() {
            lock (gate) {
                while (disposeStarted && !disposed) {
                    Monitor.Wait(gate);
                }
                if (disposed) {
                    return;
                }
                disposeStarted = true;
            }
            try {
                displaySettingsChanged.RemoveEventHandler(null, handler);
                // SystemEvents raises callbacks on its dispatcher thread.
                // A synchronous no-op queued after unsubscription is a
                // boundary barrier: callbacks already queued before removal
                // complete before callback admission is closed below.
                invokeOnEventsThread.Invoke(
                    null,
                    new object[] { new Action(delegate() {}) });
            } catch {
                lock (gate) {
                    disposeStarted = false;
                    Monitor.PulseAll(gate);
                }
                throw;
            }
            lock (gate) {
                acceptingCallbacks = false;
                while (callbacksInFlight != 0) {
                    if (disposeWaitingForTesting != null) {
                        disposeWaitingForTesting.Set();
                    }
                    Monitor.Wait(gate);
                }
                disposed = true;
                Monitor.PulseAll(gate);
            }
        }
    }
}
'@
    if ($IncludeSelfTestProbe) {
        $source += @'

namespace KettlePerfDisplayStabilityTest {
    public sealed class RaceResult {
        public bool DisposeWaitedForCallback { get; set; }
        public int CallbackCountBeforePostDisposeRaise { get; set; }
        public int CallbackCountAfterPostDisposeRaise { get; set; }
        public int DispatcherBarrierCount { get; set; }
    }

    public static class Race {
        public static event EventHandler DisplaySettingsChanged;
        private static int dispatcherBarrierCount;

        public static void InvokeOnEventsThread(Delegate method) {
            Interlocked.Increment(ref dispatcherBarrierCount);
            method.DynamicInvoke();
        }

        private static void Raise() {
            EventHandler snapshot = DisplaySettingsChanged;
            if (snapshot != null) {
                snapshot(null, EventArgs.Empty);
            }
        }

        public static RaceResult Run() {
            dispatcherBarrierCount = 0;
            var queue = new ConcurrentQueue<object>();
            var callbackEntered = new ManualResetEventSlim(false);
            var callbackRelease = new ManualResetEventSlim(false);
            var disposeWaiting = new ManualResetEventSlim(false);
            var subscription = new KettlePerf.DisplayStabilitySubscription(
                typeof(Race),
                queue,
                callbackEntered,
                callbackRelease,
                disposeWaiting);
            Exception raiseError = null;
            Exception disposeError = null;
            var raiseThread = new Thread(delegate() {
                try {
                    Raise();
                } catch (Exception error) {
                    raiseError = error;
                }
            });
            var disposeThread = new Thread(delegate() {
                try {
                    subscription.Dispose();
                } catch (Exception error) {
                    disposeError = error;
                }
            });
            try {
                raiseThread.Start();
                if (!callbackEntered.Wait(TimeSpan.FromSeconds(5))) {
                    throw new InvalidOperationException(
                        "display callback did not enter");
                }
                disposeThread.Start();
                if (!disposeWaiting.Wait(TimeSpan.FromSeconds(5))) {
                    throw new InvalidOperationException(
                        "disposal did not wait for the in-flight callback");
                }
                bool disposeWaitedForCallback = disposeThread.IsAlive;
                callbackRelease.Set();
                if (!raiseThread.Join(TimeSpan.FromSeconds(5))) {
                    throw new InvalidOperationException(
                        "display callback did not complete");
                }
                if (!disposeThread.Join(TimeSpan.FromSeconds(5))) {
                    throw new InvalidOperationException(
                        "display subscription disposal did not complete");
                }
                if (raiseError != null) {
                    throw new InvalidOperationException(
                        "display callback failed",
                        raiseError);
                }
                if (disposeError != null) {
                    throw new InvalidOperationException(
                        "display subscription disposal failed",
                        disposeError);
                }
                int callbackCountBeforePostDisposeRaise = queue.Count;
                Raise();
                return new RaceResult {
                    DisposeWaitedForCallback = disposeWaitedForCallback,
                    CallbackCountBeforePostDisposeRaise =
                        callbackCountBeforePostDisposeRaise,
                    CallbackCountAfterPostDisposeRaise = queue.Count,
                    DispatcherBarrierCount = dispatcherBarrierCount
                };
            } finally {
                callbackRelease.Set();
                if (raiseThread.IsAlive) {
                    raiseThread.Join(TimeSpan.FromSeconds(5));
                }
                if (disposeThread.IsAlive) {
                    disposeThread.Join(TimeSpan.FromSeconds(5));
                }
                subscription.Dispose();
                callbackEntered.Dispose();
                callbackRelease.Dispose();
                disposeWaiting.Dispose();
            }
        }
    }
}
'@
    }
    Add-Type -TypeDefinition $source
}

function Start-KettlePerfDisplayStabilityMonitor {
    param(
        [Parameter(Mandatory)]
        [ValidatePattern(
            '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-' +
            '[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'
        )]
        [string]$RunId
    )

    $queue = [Collections.Concurrent.ConcurrentQueue[object]]::new()
    $subscription = $null
    $registrationError = $null
    try {
        Initialize-KettlePerfDisplayStabilityNativeType
        $subscription = (
            [KettlePerf.DisplayStabilitySubscription]::new(
                [Microsoft.Win32.SystemEvents],
                $queue
            )
        )
    } catch {
        $registrationError = $_.Exception.GetType().FullName
    }

    return [pscustomobject]@{
        schema = 'kettle-display-stability-monitor-v1'
        provider = 'Microsoft.Win32.SystemEvents.DisplaySettingsChanged'
        run_id = $RunId
        queue = $queue
        subscription = $subscription
        registration_succeeded = $null -ne $subscription
        registration_error_type = $registrationError
        stopped = $false
    }
}

function Get-KettlePerfDisplayStabilityEvents {
    param(
        [Parameter(Mandatory)]
        $Monitor
    )

    if (
        $null -eq $Monitor -or
        $Monitor.schema -cne 'kettle-display-stability-monitor-v1' -or
        $null -eq $Monitor.queue
    ) {
        throw 'Display stability monitor is invalid'
    }
    return [object[]]@($Monitor.queue.ToArray())
}

function Stop-KettlePerfDisplayStabilityMonitor {
    param(
        [Parameter(Mandatory)]
        $Monitor
    )

    if (
        $null -eq $Monitor -or
        $Monitor.schema -cne 'kettle-display-stability-monitor-v1'
    ) {
        throw 'Display stability monitor is invalid'
    }
    if (-not [bool]$Monitor.stopped) {
        if ([bool]$Monitor.registration_succeeded) {
            if ($null -eq $Monitor.subscription) {
                throw 'Display stability subscription is missing'
            }
            $Monitor.subscription.Dispose()
        }
        $Monitor.stopped = $true
    }
    return Get-KettlePerfDisplayStabilityEvents -Monitor $Monitor
}

function Get-KettlePerfDisplayStabilityEvidence {
    param(
        [Parameter(Mandatory)]
        $Monitor,
        [Parameter(Mandatory)]
        [ValidatePattern('^[0-9a-f]{64}$')]
        [string]$InitialSignature,
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]]$Checkpoints
    )

    if (-not [bool]$Monitor.stopped) {
        throw (
            'Display stability evidence cannot be finalized before the ' +
            'monitor is stopped and in-flight callbacks are drained'
        )
    }
    $events = @(Get-KettlePerfDisplayStabilityEvents -Monitor $Monitor)
    $invalidCheckpoints = [Collections.Generic.List[string]]::new()
    foreach ($checkpoint in $Checkpoints) {
        $phase = [string]$checkpoint.phase
        $snapshot = $checkpoint.snapshot
        $signature = [string]$snapshot.signature_sha256
        if (
            $phase -cnotmatch '^[a-z0-9][a-z0-9-]{0,63}$' -or
            $snapshot.schema -cne 'kettle-display-topology-snapshot-v2' -or
            $signature -cnotmatch '^[0-9a-f]{64}$' -or
            -not [StringComparer]::Ordinal.Equals(
                $signature,
                $InitialSignature
            )
        ) {
            $invalidCheckpoints.Add($phase)
        }
    }
    $stable = (
        [bool]$Monitor.registration_succeeded -and
        $events.Count -eq 0 -and
        $Checkpoints.Count -ge 2 -and
        $invalidCheckpoints.Count -eq 0
    )
    return [pscustomobject][ordered]@{
        schema = 'kettle-display-stability-evidence-v1'
        provider = [string]$Monitor.provider
        monitoring_active_for_run = [bool]$Monitor.registration_succeeded
        registration_error_type = $Monitor.registration_error_type
        display_change_events = [object[]]$events
        checkpoints = [object[]]$Checkpoints
        invalid_checkpoint_phases = [string[]]$invalidCheckpoints.ToArray()
        stable = $stable
    }
}
