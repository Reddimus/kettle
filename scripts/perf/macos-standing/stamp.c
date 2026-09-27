// stamp OUT [CMD ARGS...]
//
// Runs as the command inside each terminal under test. It records the
// CLOCK_UPTIME_RAW time it started and its tty size in OUT, then execs CMD if
// given. The launch probe reads the same clock, so the difference is the time
// from spawning the terminal to its child's first instruction.
#include <stdio.h>
#include <sys/ioctl.h>
#include <time.h>
#include <unistd.h>

int main(int argc, char **argv) {
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
