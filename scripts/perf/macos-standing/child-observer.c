// Private startup smoke observer; never used for timing measurements.
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <signal.h>
#include <stdint.h>
#include <string.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/ioctl.h>
#include <time.h>
#include <unistd.h>

static volatile sig_atomic_t winch;
static void on_winch(int signo) {
    (void)signo;
    if (winch < INT_MAX) ++winch;
}
static uint64_t raw_ns(void) {
    struct timespec t;
    if (clock_gettime(CLOCK_UPTIME_RAW, &t)) _exit(78);
    return (uint64_t)t.tv_sec * 1000000000ULL + (uint64_t)t.tv_nsec;
}
int main(int argc, char **argv) {
    const char *path = getenv("KETTLE_S3_CHILD_OBSERVATION");
    if (!path) return 78;
    uint64_t start = raw_ns();
    struct sigaction action = {0};
    action.sa_handler = on_winch;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGWINCH, &action, NULL)) return 78;
    struct winsize size;
    if (ioctl(STDIN_FILENO, TIOCGWINSZ, &size)) return 78;
    uint64_t observed = raw_ns();
    while (raw_ns() - observed < 2000000000ULL) {
        struct timespec pause = {0, 10000000};
        while (nanosleep(&pause, &pause) && errno == EINTR) {}
    }
    uint64_t end = raw_ns();
    int fd = open(path, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0600);
    if (fd < 0) return 78;
    char line[512];
    int n = snprintf(line, sizeof(line),
        "{\"clock\":\"CLOCK_UPTIME_RAW\",\"pid\":%ld,\"session_id\":%ld,"
        "\"start_ns\":%llu,\"t_ns\":%llu,\"end_ns\":%llu,"
        "\"cols\":%u,\"rows\":%u,\"pixel_width\":%u,"
        "\"pixel_height\":%u,\"sigwinch\":%d}\n",
        (long)getpid(), (long)getsid(0), (unsigned long long)start,
        (unsigned long long)observed, (unsigned long long)end,
        size.ws_col, size.ws_row, size.ws_xpixel, size.ws_ypixel, (int)winch);
    if (n <= 0 || (size_t)n >= sizeof(line)) return 78;
    size_t offset = 0;
    while (offset < (size_t)n) {
        ssize_t written = write(fd, line + offset, (size_t)n - offset);
        if (written < 0 && errno == EINTR) continue;
        if (written <= 0) return 78;
        offset += (size_t)written;
    }
    if (close(fd)) return 78;
    unsetenv("KETTLE_S3_CHILD_OBSERVATION");
    if (argc == 2 && strcmp(argv[1], "--observe-only") == 0) return 0;
    execl("/bin/zsh", "zsh", "-f", "-i", (char *)NULL);
    return 78;
}
