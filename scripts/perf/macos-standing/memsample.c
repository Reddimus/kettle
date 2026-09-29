// memsample PID [INTERVAL_MS COUNT]
//
// Prints one JSON object for PID: resident size, phys_footprint (the figure
// Activity Monitor shows as Memory, which includes GPU driver memory that
// resident size does not), its lifetime maximum, CPU time in nanoseconds, and
// package idle plus interrupt wakeups. Two samples a known interval apart give
// CPU share and wakeups per second.
//
// With INTERVAL_MS and COUNT it prints up to COUNT such lines, one every
// INTERVAL_MS, each with t_ns on the CLOCK_UPTIME_RAW clock that `stamp` and
// `launch` use, and stops early once PID is gone.
#include <libproc.h>
#include <mach/mach_time.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/resource.h>
#include <time.h>
#include <unistd.h>

static int sample(int pid, int timed) {
	struct rusage_info_v4 usage;
	struct proc_taskinfo task;
	if (proc_pid_rusage(pid, RUSAGE_INFO_V4, (rusage_info_t *)&usage) != 0) {
		return 1;
	}
	if (proc_pidinfo(pid, PROC_PIDTASKINFO, 0, &task, sizeof task) != sizeof task) {
		return 1;
	}
	// ri_user_time and ri_system_time are in Mach absolute-time units.
	mach_timebase_info_data_t base;
	mach_timebase_info(&base);
	unsigned long long ticks = usage.ri_user_time + usage.ri_system_time;
	unsigned long long cpu_ns = ticks * base.numer / base.denom;
	if (timed) {
		printf("{\"t_ns\": %llu, ", clock_gettime_nsec_np(CLOCK_UPTIME_RAW));
	} else {
		printf("{");
	}
	printf("\"pid\": %d, \"rss\": %llu, \"footprint\": %llu, \"max_footprint\": %llu, "
	       "\"cpu_ns\": %llu, \"wakeups\": %llu}\n",
	       pid, (unsigned long long)task.pti_resident_size, usage.ri_phys_footprint,
	       usage.ri_lifetime_max_phys_footprint, cpu_ns,
	       usage.ri_pkg_idle_wkups + usage.ri_interrupt_wkups);
	fflush(stdout);
	return 0;
}

int main(int argc, char **argv) {
	if (argc != 2 && argc != 4) {
		fprintf(stderr, "usage: memsample PID [INTERVAL_MS COUNT]\n");
		return 2;
	}
	int pid = atoi(argv[1]);
	if (argc == 2) {
		return sample(pid, 0);
	}
	int interval_ms = atoi(argv[2]);
	int count = atoi(argv[3]);
	if (interval_ms <= 0 || count <= 0) {
		fprintf(stderr, "memsample: INTERVAL_MS and COUNT must be positive\n");
		return 2;
	}
	for (int i = 0; i < count; i++) {
		if (sample(pid, 1) != 0) {
			// The process exited; the lines so far are the timeline.
			return i == 0 ? 1 : 0;
		}
		if (i + 1 < count) {
			usleep((useconds_t)interval_ms * 1000);
		}
	}
	return 0;
}
