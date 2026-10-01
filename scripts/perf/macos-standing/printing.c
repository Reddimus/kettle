// printing BARRIER LOG: fixed 80-line absolute schedule, then quiet hold.
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <unistd.h>
#include <string.h>

static int synthetic = 0;
static unsigned long long clock_ns = 10000000000ULL;
static unsigned long long now(void) { return synthetic ? clock_ns : clock_gettime_nsec_np(CLOCK_UPTIME_RAW); }
static int paced_sleep(const struct timespec *delay, struct timespec *unused) {
    if (!synthetic) return nanosleep(delay, unused);
    unsigned long long ns = (unsigned long long)delay->tv_sec * 1000000000ULL + delay->tv_nsec;
    // Short returns exercise the absolute wait's retry as well as write cost.
    clock_ns += ns > 17000000ULL ? 17000000ULL : ns;
    return 0;
}
static int paced_usleep(unsigned int us) {
    if (synthetic) { clock_ns += (unsigned long long)us * 1000; return 0; }
    return usleep(us);
}
#define nanosleep paced_sleep
#define usleep paced_usleep
static void until(unsigned long long deadline) {
    for (;;) {
        unsigned long long t = now();
        if (t >= deadline) return;
        struct timespec delay = {(time_t)((deadline - t) / 1000000000),
                                 (long)((deadline - t) % 1000000000)};
        nanosleep(&delay, NULL);
    }
}
static int printLines(FILE *log) {
    unsigned long long began = now();
    fprintf(log, "{\"began_ns\":%llu}\n", began); fflush(log);
    for (int i = 0; i < 80; ++i) {
        unsigned long long deadline = began + (unsigned long long)i * 100000000;
        until(deadline);
        unsigned long long wrote = now();
        if (printf("%02d: The quick brown fox jumps over the lazy dog 0123456789\n", i + 1) < 0
            || fflush(stdout) != 0) { fclose(log); return 4; }
        if (synthetic) clock_ns += 30000000ULL;
        fprintf(log, "{\"seq\":%d,\"deadline_ns\":%llu,\"write_ns\":%llu,\"write_end_ns\":%llu}\n",
                i + 1, deadline, wrote, now()); fflush(log);
    }
    until(began + 8000000000ULL);
    fprintf(log, "{\"done_ns\":%llu}\n", now()); fclose(log);
    return 0;
}
int main(int argc, char **argv) {
    if (argc == 2 && strcmp(argv[1], "--self-test") == 0) {
        synthetic = 1;
        return printLines(stderr);
    }
    if (argc != 3) return 2;
    FILE *log = fopen(argv[2], "wx");
    if (!log) return 2;
    unsigned long long wait_end = now() + 30000000000ULL;
    while (access(argv[1], F_OK) != 0) {
        if (now() >= wait_end) { fclose(log); return 3; }
        usleep(1000);
    }
    int result = printLines(log);
    if (result) return result;
    sleep(30);
    return 0;
}
