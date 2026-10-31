// lfs-compat.c —— 把 32 位 glibc 的「大文件」符号变体转发到普通变体。
//
// 为什么需要它：在 32 位 glibc 上 open64 / stat64 / mmap64 和 open / stat / mmap
// 是**不同的动态符号**。Rust 的 std 在 32 位 Linux 上走的是 64 位变体，而
// fbshim/touchshim 只拦截了普通变体 —— 结果就是拦截被绕过：应用 opens
// "/dev/fb0" 实际打开的是真实设备（或直接失败），画面一帧都出不来。
//
// 做法：把这个 shim 放在 LD_PRELOAD 的**最前面**，它只做转发，通过
// dlsym(RTLD_NEXT, "open") 找到链上下一个 shim（也就是 fbshim / touchshim）
// 的同名普通变体。这样 64 位变体就自动享受到后面所有 shim 的处理。
//
// LD_PRELOAD 顺序必须是：
//   liblfscompat.so:libfbshim.armhf.so:libtouchshim.armhf.so

#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <stdarg.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/types.h>

typedef int (*open_fn)(const char *, int, ...);
typedef int (*openat_fn)(int, const char *, int, ...);
typedef int (*stat_fn)(const char *, struct stat *);
typedef int (*fstat_fn)(int, struct stat *);
typedef void *(*mmap_fn)(void *, size_t, int, int, int, off_t);

static open_fn   nx_open;
static openat_fn nx_openat;
static stat_fn   nx_stat;
static stat_fn   nx_lstat;
static fstat_fn  nx_fstat;
static mmap_fn   nx_mmap;

static void resolve(void) {
    if (!nx_open)   nx_open   = (open_fn)dlsym(RTLD_NEXT, "open");
    if (!nx_openat) nx_openat = (openat_fn)dlsym(RTLD_NEXT, "openat");
    if (!nx_stat)   nx_stat   = (stat_fn)dlsym(RTLD_NEXT, "stat");
    if (!nx_lstat)  nx_lstat  = (stat_fn)dlsym(RTLD_NEXT, "lstat");
    if (!nx_fstat)  nx_fstat  = (fstat_fn)dlsym(RTLD_NEXT, "fstat");
    if (!nx_mmap)   nx_mmap   = (mmap_fn)dlsym(RTLD_NEXT, "mmap");
}

int open64(const char *path, int flags, ...) {
    resolve();
    va_list ap;
    va_start(ap, flags);
    mode_t mode = va_arg(ap, mode_t);
    va_end(ap);
    return nx_open ? nx_open(path, flags, mode) : -1;
}

int openat64(int dirfd, const char *path, int flags, ...) {
    resolve();
    va_list ap;
    va_start(ap, flags);
    mode_t mode = va_arg(ap, mode_t);
    va_end(ap);
    return nx_openat ? nx_openat(dirfd, path, flags, mode) : -1;
}

// __open64_2 / __openat64_2 是 glibc 为 fortify 提供的别名，也一并转发
int __open64_2(const char *path, int flags) {
    resolve();
    return nx_open ? nx_open(path, flags, 0) : -1;
}

int __openat64_2(int dirfd, const char *path, int flags) {
    resolve();
    return nx_openat ? nx_openat(dirfd, path, flags, 0) : -1;
}

int stat64(const char *path, struct stat64 *buf) {
    resolve();
    return nx_stat ? nx_stat(path, (struct stat *)buf) : -1;
}

int lstat64(const char *path, struct stat64 *buf) {
    resolve();
    return nx_lstat ? nx_lstat(path, (struct stat *)buf) : -1;
}

int fstat64(int fd, struct stat64 *buf) {
    resolve();
    return nx_fstat ? nx_fstat(fd, (struct stat *)buf) : -1;
}

void *mmap64(void *addr, size_t len, int prot, int flags, int fd, off64_t off) {
    resolve();
    return nx_mmap ? nx_mmap(addr, len, prot, flags, fd, (off_t)off) : MAP_FAILED;
}
