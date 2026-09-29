// keyblock LOG
//
// The keystroke-to-screen payload. It runs inside every measured terminal and
// sends every terminal the same bytes. On /dev/tty in raw mode, with the cursor
// hidden and set to a steady block (so no cursor blink timer runs), it draws a
// 16x4-cell block in the middle of the 120x36 grid. Every read() toggles the
// block between reverse video and normal video with a single write(), then
// appends a 32-byte record to LOG: sequence number, bytes read, and the
// CLOCK_UPTIME_RAW times just after the read and just after the write. No
// alternate screen, synchronized output, mouse mode or OSC is used.
#include <fcntl.h>
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

int main(int argc, char **argv) {
	if (argc != 2) {
		fprintf(stderr, "usage: keyblock LOG\n");
		return 2;
	}
	int tty = open("/dev/tty", O_RDWR);
	int log = open(argv[1], O_WRONLY | O_CREAT | O_TRUNC, 0600);
	if (tty < 0 || log < 0) {
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
	char buf[64];
	for (;;) {
		ssize_t n = read(tty, buf, sizeof buf);
		uint64_t t_read = clock_gettime_nsec_np(CLOCK_UPTIME_RAW);
		if (n <= 0) {
			return 0;
		}
		on = !on;
		if (write_all(tty, on ? on_frame : off_frame, on ? on_len : off_len) != 0) {
			return 1;
		}
		struct record rec = {++seq, (uint64_t)n, t_read, clock_gettime_nsec_np(CLOCK_UPTIME_RAW)};
		if (write(log, &rec, sizeof rec) != (ssize_t)sizeof rec) {
			return 1;
		}
	}
}
