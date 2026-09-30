// stamp OUT [CMD ARGS...]
// stamp --settle COLS ROWS SECONDS OUT
//
// Runs as the command inside each terminal under test. It records the
// CLOCK_UPTIME_RAW time it started and its tty size in OUT, then execs CMD if
// given. The launch probe reads the same clock, so the difference is the time
// from spawning the terminal to its child's first instruction.
//
// With --settle it waits, up to SECONDS, for the tty to be COLS x ROWS, then
// writes "cols rows waited_ms" to OUT whatever size it ended at. A terminal
// can start its child before its first resize (Ghostty sometimes does), so
// the size at the first instruction is not always the size the workload
// runs at. OUT appears whole: it is written to OUT.tmp, then renamed.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <time.h>
#include <unistd.h>

static int settle(int argc, char **argv) {
	if (argc != 6) {
		fprintf(stderr, "usage: stamp --settle COLS ROWS SECONDS OUT\n");
		return 2;
	}
	unsigned cols = (unsigned)strtoul(argv[2], NULL, 10);
	unsigned rows = (unsigned)strtoul(argv[3], NULL, 10);
	unsigned long long started = clock_gettime_nsec_np(CLOCK_UPTIME_RAW);
	unsigned long long limit = started + (unsigned long long)(strtod(argv[4], NULL) * 1e9);
	struct winsize size = {0};
	for (;;) {
		ioctl(STDIN_FILENO, TIOCGWINSZ, &size);
		if ((size.ws_col == cols && size.ws_row == rows) || clock_gettime_nsec_np(CLOCK_UPTIME_RAW) >= limit) {
			break;
		}
		usleep(5000);
	}
	unsigned long long waited = (clock_gettime_nsec_np(CLOCK_UPTIME_RAW) - started) / 1000000;
	char tmp[4096];
	if (snprintf(tmp, sizeof tmp, "%s.tmp", argv[5]) >= (int)sizeof tmp) {
		return 1;
	}
	FILE *out = fopen(tmp, "w");
	if (!out) {
		return 1;
	}
	fprintf(out, "%u %u %llu\n", size.ws_col, size.ws_row, waited);
	if (fclose(out) != 0 || rename(tmp, argv[5]) != 0) {
		return 1;
	}
	return 0;
}

int main(int argc, char **argv) {
	if (argc > 1 && strcmp(argv[1], "--settle") == 0) {
		return settle(argc, argv);
	}
	if (argc < 2) {
		fprintf(stderr, "usage: stamp OUT [CMD ARGS...]\n");
		return 2;
	}
	unsigned long long now = clock_gettime_nsec_np(CLOCK_UPTIME_RAW);
	struct winsize size = {0};
	ioctl(STDIN_FILENO, TIOCGWINSZ, &size);
	FILE *out = fopen(argv[1], "w");
	if (!out) {
		return 1;
	}
	fprintf(out, "%llu %u %u\n", now, size.ws_col, size.ws_row);
	fclose(out);
	if (argc > 2) {
		execvp(argv[2], argv + 2);
		return 127;
	}
	return 0;
}
