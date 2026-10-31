// touchshim.c - Synthetic /dev/input/eventN multitouch device for Kindle emulation.
//
// LD_PRELOAD into a dynamically linked Kindle app. Replaces /dev/input/event*
// with a protocol-B multitouch device whose events come from the host viewer.
//
// Host protocol (qemu-kindle server.go, newline separated):
//   D <x> <y>    touch down at pixel (x, y)
//   M <x> <y>    move to (x, y)
//   U            touch up
//
// Env:
//   TOUCH_W / TOUCH_H  panel size in px (default 1236x1648 = Paperwhite 5)
//   TOUCH_UNIX         connect to this Unix socket instead of a virtio port
//   TOUCH_VPORT        virtio-serial port (default /dev/virtio-ports/org.kindle.touch)
//   TOUCH_DEBUG        1 = verbose
//
// Build:
//   arm-kindlehf-linux-gnueabihf-gcc -O2 -fPIC -shared -o libtouchshim.armhf.so touchshim.c -ldl -pthread

#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <linux/input.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>
#include <sys/un.h>
#include <time.h>
#include <unistd.h>

static int TOUCH_W = 1236;
static int TOUCH_H = 1648;
static int TOUCH_DEBUG = 0;
#define D(fmt, ...) do { if (TOUCH_DEBUG) fprintf(stderr, "[touchshim] " fmt "\n", ##__VA_ARGS__); } while (0)
#define E(fmt, ...) do { fprintf(stderr, "[touchshim:ERR] " fmt "\n", ##__VA_ARGS__); } while (0)

static const char *DEFAULT_VPORT = "/dev/virtio-ports/org.kindle.touch";
static const char *FALLBACK_VPORT = "/dev/vport0p1";

#define MAX_FAKE 16
static int fake_fds[MAX_FAKE];
static int fake_wr[MAX_FAKE];
static pthread_mutex_t tbl_mu = PTHREAD_MUTEX_INITIALIZER;

static int (*real_open)(const char *, int, ...) = NULL;
static int (*real_openat)(int, const char *, int, ...) = NULL;
static int (*real_close)(int) = NULL;
static int (*real_ioctl)(int, unsigned long, ...) = NULL;

static void init_real(void) {
    if (!real_open)   real_open   = dlsym(RTLD_NEXT, "open");
    if (!real_openat) real_openat = dlsym(RTLD_NEXT, "openat");
    if (!real_close)  real_close  = dlsym(RTLD_NEXT, "close");
    if (!real_ioctl)  real_ioctl  = dlsym(RTLD_NEXT, "ioctl");
}

static int is_touch_path(const char *p) {
    return p && strncmp(p, "/dev/input/event", 16) == 0 && p[16] != '\0';
}

static int lookup(int fd) {
    for (int i = 0; i < MAX_FAKE; i++) if (fake_fds[i] == fd) return i;
    return -1;
}

// ---- event injection -------------------------------------------------------
static int g_fd = -1;              // write end of the active pipe
static uint32_t g_tracking = 1;
static int g_down = 0;

static void emit(uint16_t type, uint16_t code, int32_t value) {
    if (g_fd < 0) return;
    struct input_event ev;
    memset(&ev, 0, sizeof ev);
    ev.type = type;
    ev.code = code;
    ev.value = value;
    ssize_t n = write(g_fd, &ev, sizeof ev);
    if (n != (ssize_t)sizeof ev) D("emit write %zd (errno=%d)", n, errno);
}

static void emit_down(int x, int y) {
    g_tracking++;
    emit(EV_ABS, ABS_MT_SLOT, 0);
    emit(EV_ABS, ABS_MT_TRACKING_ID, (int32_t)g_tracking);
    emit(EV_ABS, ABS_MT_POSITION_X, x);
    emit(EV_ABS, ABS_MT_POSITION_Y, y);
    emit(EV_KEY, BTN_TOUCH, 1);
    emit(EV_SYN, SYN_REPORT, 0);
    g_down = 1;
    D("down %d,%d id=%u", x, y, g_tracking);
}

static void emit_move(int x, int y) {
    if (!g_down) { emit_down(x, y); return; }
    emit(EV_ABS, ABS_MT_POSITION_X, x);
    emit(EV_ABS, ABS_MT_POSITION_Y, y);
    emit(EV_SYN, SYN_REPORT, 0);
    D("move %d,%d", x, y);
}

static void emit_up(void) {
    if (!g_down) return;
    emit(EV_ABS, ABS_MT_SLOT, 0);
    emit(EV_ABS, ABS_MT_TRACKING_ID, -1);
    emit(EV_KEY, BTN_TOUCH, 0);
    emit(EV_SYN, SYN_REPORT, 0);
    g_down = 0;
    D("up");
}

// ---- host connection -------------------------------------------------------
static int connect_host(void) {
    const char *unix_path = getenv("TOUCH_UNIX");
    if (unix_path && *unix_path) {
        int s = socket(AF_UNIX, SOCK_STREAM, 0);
        if (s < 0) return -1;
        struct sockaddr_un sa;
        memset(&sa, 0, sizeof sa);
        sa.sun_family = AF_UNIX;
        strncpy(sa.sun_path, unix_path, sizeof(sa.sun_path) - 1);
        if (connect(s, (struct sockaddr *)&sa, sizeof sa) == 0) { D("unix connected %s", unix_path); return s; }
        close(s);
        return -1;
    }
    const char *vp = getenv("TOUCH_VPORT");
    if (!vp || !*vp) vp = DEFAULT_VPORT;
    int fd = -1;
    if (real_open) fd = real_open(vp, O_RDONLY | O_NONBLOCK);
    if (fd < 0 && real_open) fd = real_open(FALLBACK_VPORT, O_RDONLY | O_NONBLOCK);
    if (fd >= 0) D("vport connected %s", vp);
    return fd;
}

static void *pump(void *arg) {
    (void)arg;
    char line[256];
    size_t used = 0;
    int s = -1;
    for (;;) {
        if (s < 0) {
            s = connect_host();
            if (s < 0) { sleep(1); continue; }
            used = 0;
        }
        char buf[128];
        ssize_t n = read(s, buf, sizeof buf);
        if (n <= 0) {
            // virtio-serial returns 0 when there is simply nothing to read, not
            // only at real EOF. Treating 0 as "disconnected" made the pump reopen
            // the port on every idle tick — a reconnect storm that also risks
            // dropping input. Only a genuine error (beyond the usual would-block
            // codes) actually drops the connection.
            if (n < 0 && errno != EAGAIN && errno != EWOULDBLOCK && errno != EINTR) {
                close(s);
                s = -1;
            }
            struct timespec ts = {0, 20 * 1000 * 1000};
            nanosleep(&ts, NULL);
            continue;
        }
        for (ssize_t i = 0; i < n; i++) {
            char c = buf[i];
            if (c == '\n' || c == '\r') {
                if (used) {
                    line[used] = 0;
                    if (line[0] == 'D' || line[0] == 'M') {
                        int x = 0, y = 0;
                        if (sscanf(line + 1, "%d %d", &x, &y) == 2) {
                            if (x < 0) x = 0;
                            if (x > TOUCH_W - 1) x = TOUCH_W - 1;
                            if (y < 0) y = 0;
                            if (y > TOUCH_H - 1) y = TOUCH_H - 1;
                            if (line[0] == 'D') emit_down(x, y); else emit_move(x, y);
                        }
                    } else if (line[0] == 'U') {
                        emit_up();
                    }
                    used = 0;
                }
            } else if (used < sizeof(line) - 1) {
                line[used++] = c;
            }
        }
    }
    return NULL;
}

// ---- interposed libc -------------------------------------------------------
static int make_fake(const char *why) {
    int p[2];
    if (pipe(p) != 0) { E("pipe failed errno=%d", errno); return -1; }
    fcntl(p[0], F_SETFL, fcntl(p[0], F_GETFL, 0) | O_NONBLOCK);
    fcntl(p[1], F_SETFL, fcntl(p[1], F_GETFL, 0) | O_NONBLOCK);
    pthread_mutex_lock(&tbl_mu);
    int slot = -1;
    for (int i = 0; i < MAX_FAKE; i++) if (fake_fds[i] == 0) { slot = i; break; }
    if (slot < 0) { pthread_mutex_unlock(&tbl_mu); close(p[0]); close(p[1]); E("fake fd table full"); return -1; }
    fake_fds[slot] = p[0];
    fake_wr[slot] = p[1];
    pthread_mutex_unlock(&tbl_mu);
    if (g_fd < 0) {
        g_fd = p[1];
        static pthread_t th;
        static int started = 0;
        if (!started) { started = 1; pthread_create(&th, NULL, pump, NULL); }
    }
    D("open(%s) -> fake fd %d", why, p[0]);
    return p[0];
}

int open(const char *path, int flags, ...) {
    init_real();
    if (is_touch_path(path)) return make_fake(path);
    va_list ap; va_start(ap, flags);
    mode_t mode = va_arg(ap, mode_t);
    va_end(ap);
    return real_open ? real_open(path, flags, mode) : -1;
}

int openat(int dirfd, const char *path, int flags, ...) {
    init_real();
    if (is_touch_path(path)) return make_fake(path);
    va_list ap; va_start(ap, flags);
    mode_t mode = va_arg(ap, mode_t);
    va_end(ap);
    return real_openat ? real_openat(dirfd, path, flags, mode) : -1;
}

// NOTE: the inject-side write end must be released with real_close(), not with
// close(). Our close() is interposed, so calling close() here re-enters this
// same function and tries to re-lock tbl_mu — a non-recursive mutex this thread
// already holds. That self-deadlocks while holding the table lock, wedging every
// later open()/ioctl() in the app (it froze right after the touch device probe).
// So: pull the write fd out under the lock, release, then real_close it.
int close(int fd) {
    init_real();
    int wr = -1;
    pthread_mutex_lock(&tbl_mu);
    int i = lookup(fd);
    if (i >= 0) {
        wr = fake_wr[i];
        fake_fds[i] = 0;
        fake_wr[i] = 0;
        if (g_fd == wr) g_fd = -1;
    }
    pthread_mutex_unlock(&tbl_mu);
    if (wr >= 0) {
        D("close(fd=%d) -> drop inject end %d", fd, wr);
        if (real_close) real_close(wr);
    }
    return real_close ? real_close(fd) : -1;
}

static void set_bit(unsigned char *bits, size_t nbytes, unsigned bit) {
    size_t b = bit / 8;
    if (b < nbytes) bits[b] |= (unsigned char)(1u << (bit % 8));
}

int ioctl(int fd, unsigned long req, ...) {
    init_real();
    void *argp = NULL;
    va_list ap; va_start(ap, req); argp = va_arg(ap, void *); va_end(ap);

    pthread_mutex_lock(&tbl_mu);
    int ours = (lookup(fd) >= 0);
    pthread_mutex_unlock(&tbl_mu);
    if (!ours) {
        if (!real_ioctl) { errno = ENOTTY; return -1; }
        return real_ioctl(fd, req, argp);
    }

    // Kernel macro expansion: _IOC(dir,type,nr,size) = (dir<<30)|(size<<16)|(type<<8)|nr
    // so the low byte is "nr". Ranges that matter (type = 'E'):
    //   0x01 EVIOCGVERSION  0x02 EVIOCGID  0x06 EVIOCGNAME
    //   0x20..0x3f EVIOCGBIT(evtype,len)  -> evtype = nr - 0x20
    //   0x40..0x7f EVIOCGABS(axis)        -> axis   = nr - 0x40
    //   0x90 EVIOCGRAB
    unsigned nr = req & 0xffu;
    unsigned size = (req >> 16) & 0x3fffu;

    if (req == EVIOCGRAB) { D("EVIOCGRAB -> ok"); return 0; }
    if (nr == 0x06) { if (argp && size) strncpy((char *)argp, "kindle-touch-emu", size - 1); return 0; }  // EVIOCGNAME
    if (nr == 0x01) { if (argp) *(int *)argp = 0x010001; return 0; }                                     // EVIOCGVERSION
    if (nr == 0x02) {                                                                                     // EVIOCGID
        if (argp && size >= 8) { memset(argp, 0, size); ((unsigned short *)argp)[0] = 0x03; ((unsigned short *)argp)[1] = 0x1234; ((unsigned short *)argp)[2] = 0x5678; ((unsigned short *)argp)[3] = 0x0001; }
        return 0;
    }
    if (nr >= 0x20 && nr <= 0x3f) {                     // EVIOCGBIT(evtype, len)
        unsigned evtype = nr - 0x20;
        if (argp && size) {
            memset(argp, 0, size);
            if (evtype == EV_ABS) {
                set_bit((unsigned char *)argp, size, ABS_MT_SLOT);
                set_bit((unsigned char *)argp, size, ABS_MT_TRACKING_ID);
                set_bit((unsigned char *)argp, size, ABS_MT_POSITION_X);
                set_bit((unsigned char *)argp, size, ABS_MT_POSITION_Y);
                set_bit((unsigned char *)argp, size, ABS_PRESSURE);
            } else if (evtype == EV_KEY) {
                set_bit((unsigned char *)argp, size, BTN_TOUCH);
            } else if (evtype == EV_SYN) {
                set_bit((unsigned char *)argp, size, SYN_REPORT);
            }
            D("EVIOCGBIT evtype=%u len=%u", evtype, size);
        }
        return 0;
    }
    // EVIOCGABS runs up to 0x40 + ABS_MAX, and ABS_MAX is 0x3f, so the range is
    // 0x40..0x7f — NOT 0x40..0x5f. ABS_MT_POSITION_X is 0x35, giving nr 0x75.
    // Clipping the range at 0x60 silently rejects the X/Y axis queries, and the
    // backend then falls back to a 4095 max and scales every touch off-screen.
    if (nr >= 0x40 && nr <= 0x7f) {                     // EVIOCGABS(axis)
        unsigned axis = nr - 0x40;
        if (argp && size >= (unsigned)sizeof(struct input_absinfo)) {
            struct input_absinfo *ai = argp;
            memset(ai, 0, sizeof *ai);
            if (axis == ABS_MT_POSITION_X) { ai->minimum = 0; ai->maximum = TOUCH_W; }
            else if (axis == ABS_MT_POSITION_Y) { ai->minimum = 0; ai->maximum = TOUCH_H; }
            else if (axis == ABS_MT_SLOT) { ai->minimum = 0; ai->maximum = 9; }
            else if (axis == ABS_MT_TRACKING_ID) { ai->minimum = 0; ai->maximum = 65535; }
            else if (axis == ABS_PRESSURE) { ai->minimum = 0; ai->maximum = 255; }
            D("EVIOCGABS axis=0x%x max=%d", axis, ai->maximum);
        }
        return 0;
    }
    D("ioctl req=0x%lx -> ok (default)", req);
    return 0;
}

__attribute__((constructor)) static void shim_init(void) {
    const char *w = getenv("TOUCH_W"); if (w) TOUCH_W = atoi(w);
    const char *h = getenv("TOUCH_H"); if (h) TOUCH_H = atoi(h);
    const char *d = getenv("TOUCH_DEBUG"); if (d) TOUCH_DEBUG = atoi(d);
    if (TOUCH_W <= 0) TOUCH_W = 1236;
    if (TOUCH_H <= 0) TOUCH_H = 1648;
    init_real();
    E("active: panel %dx%d, vport=%s", TOUCH_W, TOUCH_H,
      getenv("TOUCH_UNIX") ? getenv("TOUCH_UNIX") : DEFAULT_VPORT);
}