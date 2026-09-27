// memsample PID
//
// Prints one JSON object for PID: resident size, phys_footprint (the figure
// Activity Monitor shows as Memory, which includes GPU driver memory that
// resident size does not), its lifetime maximum, CPU time in nanoseconds, and
// package idle plus interrupt wakeups. Two samples a known interval apart give
// CPU share and wakeups per second.
#include <libproc.h>
#include <mach/mach_time.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/resource.h>

int main(int argc, char **argv) {
	if (argc != 2) {
		fprintf(stderr, "usage: memsample PID\n");
		return 2;
	}
	int pid = atoi(argv[1]);
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
	printf("{\"pid\": %d, \"rss\": %llu, \"footprint\": %llu, \"max_footprint\": %llu, "
	       "\"cpu_ns\": %llu, \"wakeups\": %llu}\n",
	       pid, (unsigned long long)task.pti_resident_size, usage.ri_phys_footprint,
	       usage.ri_lifetime_max_phys_footprint, cpu_ns,
	       usage.ri_pkg_idle_wkups + usage.ri_interrupt_wkups);
	return 0;
}
