// keyblock LOG [cursor CONTROL ACK DEADLINE_MS]
//
// The keystroke-to-screen payload. It runs inside every measured terminal and
// sends every terminal the same bytes. On /dev/tty in raw mode, with the cursor
// hidden and set to a steady block (so no cursor blink timer runs), it draws a
// 16x4-cell block in the middle of the 120x36 grid. Every read() toggles the
// block between reverse video and normal video with a single write(), then
// appends a 32-byte record to LOG: sequence number, bytes read, and the
// CLOCK_UPTIME_RAW times just after the read and just after the write. No
// alternate screen, synchronized output, mouse mode or OSC is used.
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <termios.h>
#include <time.h>
#include <unistd.h>

// Frame geometry: rows 17-20 and columns 53-68 (1-based) of a 120x36 grid.
#define TOP 17
#define LEFT 53
#define WIDTH 16
#define HEIGHT 4

struct record {
	uint64_t seq;
	uint64_t nbytes;
	uint64_t t_read;
	uint64_t t_written;
};

static size_t frame(char *out, size_t cap, int on) {
	size_t len = 0;
	for (int r = 0; r < HEIGHT; r++) {
		len += (size_t)snprintf(out + len, cap - len, "\x1b[%d;%dH\x1b[%dm", TOP + r, LEFT, on ? 7 : 27);
		for (int c = 0; c < WIDTH; c++) {
			out[len++] = ' ';
		}
	}
	len += (size_t)snprintf(out + len, cap - len, "\x1b[0m");
	return len;
}

static int write_all(int fd, const char *data, size_t len) {
	while (len > 0) {
		ssize_t n = write(fd, data, len);
		if (n < 0) {
			return -1;
		}
		data += n;
		len -= (size_t)n;
	}
	return 0;
}

// This transition consumes no tty byte or payload sequence. Calibration must
// be accepted by the probe, which is the only writer of ENABLE.
static int may_enable(uint64_t seq, int enabled) { return seq == 6 && !enabled; }

static size_t cursor_frame(char *out, const char *data, size_t len, int enabled) {
    memcpy(out, data, len);
    if (enabled) {
        memcpy(out + len, "\x1b[2;3H", 6);
        len += 6;
    }
    return len;
}

static int enable_cursor(int tty, const char *ack) {
    static const char bytes[] = "\x1b[?25h\x1b[1 q\x1b[2;3H";
    if (write_all(tty, bytes, sizeof bytes - 1) != 0) return -1;
    uint64_t enabled_ns = clock_gettime_nsec_np(CLOCK_UPTIME_RAW);
    char temp[4096];
    if (snprintf(temp, sizeof temp, "%s.tmp", ack) >= (int)sizeof temp) return -1;
    int fd = open(temp, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, 0600);
    if (fd < 0) return -1;
    char line[128];
    int size = snprintf(line, sizeof line, "ENABLED %llu\n", (unsigned long long)enabled_ns);
    int ok = write_all(fd, line, (size_t)size);
    if (close(fd) != 0 || ok != 0 || rename(temp, ack) != 0) return -1;
    return 0;
}

int main(int argc, char **argv) {
	int cursor = argc == 6 && strcmp(argv[2], "cursor") == 0;
	if (argc != 2 && !cursor) {
		fprintf(stderr, "usage: keyblock LOG [cursor CONTROL ACK DEADLINE_MS]\n");
		return 2;
	}
    int control = -1;
    uint64_t deadline = 0;
    if (cursor) {
        char *end;
        unsigned long long ms = strtoull(argv[5], &end, 10);
        if (*end || ms == 0 || ms > 100000000 || argv[5][0] == '-') return 2;
        deadline = clock_gettime_nsec_np(CLOCK_UPTIME_RAW) + ms * 1000000;
        control = open(argv[3], O_RDWR | O_NONBLOCK | O_NOFOLLOW);
        struct stat st;
        if (control < 0 || fstat(control, &st) != 0 || !S_ISFIFO(st.st_mode)
            || (st.st_mode & 0777) != 0600 || access(argv[4], F_OK) == 0) return 1;
    }
	int tty = open("/dev/tty", O_RDWR);
	int log = open(argv[1], O_WRONLY | O_CREAT | O_TRUNC, 0600);
	if (tty < 0 || log < 0) {
		return 1;
	}
    // macOS poll() reports POLLNVAL for a /dev/tty descriptor, so the cursor
    // loop waits on standard input instead. It must be this session's
    // controlling terminal, the device /dev/tty reads and writes. Block mode
    // never polls and is unchanged.
    if (cursor && (!isatty(STDIN_FILENO) || tcgetsid(STDIN_FILENO) != getsid(0)
                   || tcgetsid(tty) != getsid(0))) {
        return 1;
    }
	struct termios raw;
	if (tcgetattr(tty, &raw) != 0) {
		return 1;
	}
	cfmakeraw(&raw);
	if (tcsetattr(tty, TCSANOW, &raw) != 0) {
		return 1;
	}
	signal(SIGHUP, SIG_DFL);

	char on_frame[512], off_frame[512], init[768];
	size_t on_len = frame(on_frame, sizeof on_frame, 1);
	size_t off_len = frame(off_frame, sizeof off_frame, 0);
	// Hide the cursor, make it a steady block, reset attributes, clear, home.
	size_t init_len = (size_t)snprintf(init, sizeof init, "\x1b[?25l\x1b[2 q\x1b[0m\x1b[2J\x1b[H");
	memcpy(init + init_len, off_frame, off_len);
	init_len += off_len;
	if (write_all(tty, init, init_len) != 0) {
		return 1;
	}

	int on = 0;
	uint64_t seq = 0;
    int enabled = 0;
	char buf[64];
	for (;;) {
        if (cursor) {
            if (clock_gettime_nsec_np(CLOCK_UPTIME_RAW) >= deadline) return 1;
            struct pollfd fds[] = {{control, POLLIN, 0}, {STDIN_FILENO, POLLIN, 0}};
            int rc = poll(fds, 2, 1000);
            if (rc < 0 && errno == EINTR) continue;
            if (rc < 0 || fds[0].revents & (POLLERR | POLLNVAL)
                || fds[1].revents & (POLLERR | POLLHUP | POLLNVAL)) return 1;
            if (fds[0].revents & POLLIN) {
                char request[8];
                ssize_t n = read(control, request, sizeof request);
                if (n != 7 || memcmp(request, "ENABLE\n", 7) != 0 || !may_enable(seq, enabled)) return 1;
                if (enable_cursor(tty, argv[4]) != 0) return 1;
                enabled = 1;
            }
            if (!(fds[1].revents & POLLIN)) continue;
            if (seq >= 6 && !enabled) return 1;
        }
		ssize_t n = read(tty, buf, sizeof buf);
		uint64_t t_read = clock_gettime_nsec_np(CLOCK_UPTIME_RAW);
		if (n <= 0) {
			return 0;
		}
		on = !on;
        const char *data = on ? on_frame : off_frame;
        size_t len = on ? on_len : off_len;
        char parked[768];
        if (cursor) {
            len = cursor_frame(parked, data, len, enabled);
            data = parked;
        }
		if (write_all(tty, data, len) != 0) {
			return 1;
		}
		struct record rec = {++seq, (uint64_t)n, t_read, clock_gettime_nsec_np(CLOCK_UPTIME_RAW)};
		if (write(log, &rec, sizeof rec) != (ssize_t)sizeof rec) {
			return 1;
		}
	}
}
