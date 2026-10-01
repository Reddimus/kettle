// observer PID WINDOW OUT ORIGIN_NS INTERVAL_MS COUNT
// Window 0 is a scratch diagnostic only. It always reports invalid focus.
// Never signals the target. The Python owner reaps this before its launch helper.
#import <AppKit/AppKit.h>
#import <CoreGraphics/CoreGraphics.h>
#include <libproc.h>
#include <mach/mach_time.h>
#include <sys/resource.h>
#include <sys/proc.h>
#include <signal.h>
#include <time.h>

static volatile sig_atomic_t stopped = 0;
static void stop(int sig) { stopped = 1; }
static uint64_t now(void) { return clock_gettime_nsec_np(CLOCK_UPTIME_RAW); }
static NSString *identity(pid_t pid) {
    struct proc_bsdinfo info;
    if (proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, sizeof info) != sizeof info) return nil;
    if (info.pbi_status == SZOMB) return nil;
    return [NSString stringWithFormat:@"%llu:%llu", (uint64_t)info.pbi_start_tvsec,
            (uint64_t)info.pbi_start_tvusec];
}
// This decision takes values only; the self-test never reads the window server.
static NSDictionary *focusDecision(pid_t pid, CGWindowID target, NSNumber *front,
                                   NSArray *windows, uint64_t t) {
    NSDictionary *wanted = nil, *top = nil;
    for (NSDictionary *w in windows) {
        if ([w[(id)kCGWindowNumber] unsignedIntValue] == target) wanted = w;
        if (!top && [w[(id)kCGWindowLayer] intValue] == 0) top = w;
    }
    BOOL known = front && windows && wanted && top && target != 0;
    BOOL visible = known && front.intValue == pid
        && [top[(id)kCGWindowNumber] unsignedIntValue] == target
        && [wanted[(id)kCGWindowOwnerPID] intValue] == pid;
    CGRect rect = CGRectZero;
    if (known && !CGRectMakeWithDictionaryRepresentation((__bridge CFDictionaryRef)wanted[(id)kCGWindowBounds], &rect)) {
        known = NO; visible = NO;
    }
    // The window list is front-to-back. Any intersecting window above the
    // target, including a same-process dialog or nonzero-layer alert, fails.
    if (visible) {
        for (NSDictionary *w in windows) {
            if ([w[(id)kCGWindowNumber] unsignedIntValue] == target) break;
            CGRect cover = CGRectZero;
            if (!w[(id)kCGWindowAlpha] || !w[(id)kCGWindowBounds]
                || !CGRectMakeWithDictionaryRepresentation((__bridge CFDictionaryRef)w[(id)kCGWindowBounds], &cover)) {
                known = NO; visible = NO; break;
            }
            if ([w[(id)kCGWindowAlpha] doubleValue] > 0 && CGRectIntersectsRect(rect, cover)) {
                visible = NO; break;
            }
        }
    }
    return @{@"t_ns":@(t), @"known":@(known), @"valid":@(visible),
             @"frontmost_pid":front ?: NSNull.null, @"target_window":@(target),
             @"top_window":top[(id)kCGWindowNumber] ?: NSNull.null};
}
static NSDictionary *focus(pid_t pid, CGWindowID target) {
    uint64_t t = now();
    NSNumber *front = NSWorkspace.sharedWorkspace.frontmostApplication
        ? @(NSWorkspace.sharedWorkspace.frontmostApplication.processIdentifier) : nil;
    NSArray *windows = CFBridgingRelease(CGWindowListCopyWindowInfo(kCGWindowListOptionOnScreenOnly,
                                                                  kCGNullWindowID));
    return focusDecision(pid, target, front, windows, t);
}
static int selfTest(void) {
    NSData *input = [NSFileHandle.fileHandleWithStandardInput readDataToEndOfFile];
    NSDictionary *fixture = [NSJSONSerialization JSONObjectWithData:input options:0 error:nil];
    if (!fixture) return 2;
    NSDictionary *decision = focusDecision([fixture[@"pid"] intValue], [fixture[@"target"] unsignedIntValue],
                                           fixture[@"front"], fixture[@"windows"], 100);
    NSData *data = [NSJSONSerialization dataWithJSONObject:decision options:NSJSONWritingSortedKeys error:nil];
    fwrite(data.bytes, 1, data.length, stdout);
    return 0;
}
int main(int argc, char **argv) {
    @autoreleasepool {
        if (argc == 2 && strcmp(argv[1], "--self-test") == 0) return selfTest();
        if (argc != 7) return 2;
        pid_t pid = atoi(argv[1]); CGWindowID window = (CGWindowID)strtoul(argv[2], NULL, 10);
        uint64_t origin = strtoull(argv[4], NULL, 10);
        int interval = atoi(argv[5]), count = atoi(argv[6]);
        if (pid <= 0 || interval < 50 || interval > 1000 || count <= 0 || count > 12000) return 2;
        FILE *out = fopen(argv[3], "wx"); if (!out) return 2;
        signal(SIGTERM, stop); signal(SIGINT, stop);
        NSString *initial = identity(pid);
        pid_t parent = getppid();
        __block BOOL changesOverflow = NO;
        __block NSMutableArray *changes = [NSMutableArray array];
        id token = [NSWorkspace.sharedWorkspace.notificationCenter
            addObserverForName:NSWorkspaceDidActivateApplicationNotification object:nil queue:nil
            usingBlock:^(NSNotification *note) {
                if (changes.count < 64) [changes addObject:@{@"t_ns":@(now()), @"valid":@NO}];
                else changesOverflow = YES;
            }];
        mach_timebase_info_data_t base; mach_timebase_info(&base);
        for (int i = 0; i < count && !stopped && getppid() == parent; ++i) {
            uint64_t deadline = origin + (uint64_t)i * interval * 1000000;
            while (now() < deadline && !stopped && getppid() == parent) {
                uint64_t at = now();
                double seconds = at < deadline ? MIN((double)(deadline - at) / 1e9, .01) : 0;
                if (seconds > 0) [NSRunLoop.currentRunLoop runUntilDate:[NSDate dateWithTimeIntervalSinceNow:seconds]];
            }
            if (stopped || getppid() != parent) break;
            [NSRunLoop.currentRunLoop runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0]];
            NSDictionary *before = focus(pid, window);
            NSString *beforeID = identity(pid);
            uint64_t start = now();
            struct rusage_info_v4 usage = {0}; struct proc_taskinfo task = {0};
            int r = proc_pid_rusage(pid, RUSAGE_INFO_V4, (rusage_info_t *)&usage);
            int n = proc_pidinfo(pid, PROC_PIDTASKINFO, 0, &task, sizeof task);
            uint64_t end = now(); NSString *afterID = identity(pid);
            [NSRunLoop.currentRunLoop runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0]];
            NSDictionary *after = focus(pid, window);
            BOOL ok = initial && [initial isEqual:beforeID] && [initial isEqual:afterID]
                && r == 0 && n == sizeof task;
            NSDictionary *record = @{@"query_start_ns":@(start), @"query_end_ns":@(end), @"t_ns":@(end),
                @"scheduled_ns":@(deadline), @"pid":@(pid), @"process_start_identity":initial ?: NSNull.null,
                @"rss":@(task.pti_resident_size), @"footprint":@(usage.ri_phys_footprint),
                @"max_footprint":@(usage.ri_lifetime_max_phys_footprint),
                @"cpu_ns":@((uint64_t)((__uint128_t)(usage.ri_user_time + usage.ri_system_time) * base.numer / base.denom)),
                @"wakeups":@(usage.ri_pkg_idle_wkups + usage.ri_interrupt_wkups),
                @"status":ok ? @"ok" : @"target-exited-or-query-failed",
                @"focus_before":before, @"focus_after":after, @"focus_changes":[changes copy],
                @"focus_notifications_overflow":@(changesOverflow)};
            NSData *data = [NSJSONSerialization dataWithJSONObject:record options:NSJSONWritingSortedKeys error:nil];
            if (!data || fwrite(data.bytes, 1, data.length, out) != data.length || fputc('\n', out) == EOF || fflush(out)) break;
            [changes removeAllObjects]; changesOverflow = NO;
            if (!ok) break;
        }
        [NSWorkspace.sharedWorkspace.notificationCenter removeObserver:token];
        fclose(out);
    }
    return 0;
}
