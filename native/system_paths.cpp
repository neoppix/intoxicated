// The engine's file-system calls: the libc functions it imports that take a
// path or a bionic-layout struct, as `intoxicated_system_symbols` hands them out.
// Three places read that table: the import resolution
// (`crates/intoxicated-runtime/src/bionic/mod.rs`), the linker's dlsym override
// (linker_surface.cpp), so a run-time lookup of `openat` gets the same function
// the import table did, and the raw-syscall path in freebsd_libc_compat.c.
//
// On the way to FreeBSD's own call, depending on the function:
// - root tools a real device does not ship (su, magisk, ...) read as absent;
// - a path under `/system/` is redirected into the directory the runtime
//   builds for it (`intoxicated_set_system_root`);
// - `/proc` and `/sys` are synthesised from FreeBSD's sysctls when
//   INTOXICATED_FAKE_PROC is set, and never read from the host's procfs;
// - Linux open flags are translated, and bionic's `stat`, `dirent`, `statfs`
//   and `statvfs` layouts are written instead of FreeBSD's;
// - calls that would hand the engine a block the host allocator owns
//   (`getcwd(NULL)`, `vasprintf`, `realpath(path, NULL)`) are refused;
// - with the path trace on, each call prints one `[paths]` line.
// Errno is left in FreeBSD numbering unless noted; the engine reads it
// through the `__errno` translation in freebsd_abi.c.
//
// FreeBSD is the only target.

#if !defined(__FreeBSD__)
#error "system_paths.cpp is written for FreeBSD only"
#endif

#include <dirent.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <sys/types.h>
#include <unistd.h>

#include "os_compat.h"
#include "freebsd_abi.h"
#include "maps_rewrite.h"
#include "symbol_table.h"

#include <cerrno>
#include <cstdarg>
#include <cstddef>
#include <cstdint>

#include <string>
#include <atomic>
#include <mutex>
#include <unordered_map>
#include <climits>
#include <cstdio>
#include <cstdlib>
#include <cstring>

#include <netdb.h>
#include <limits.h>
#include <stdio.h>

#include <sys/param.h>
#include <sys/mount.h>
#include <string.h>

#include <vector>
#include <unordered_set>
#if defined(__FreeBSD__)
#include <sys/sysctl.h>
#include <sys/user.h>
#include <sys/auxv.h>
#include <elf.h>
#endif

// ------------------------------------------------------------ the path trace

static std::atomic<bool> g_path_trace{false};

/// The path trace: one stderr line per call that goes through the redirect,
/// `[paths] tid=<tid> <call>("<path actually used>") = <result>`, switched on
/// by bionic/mod.rs (`intoxicated_set_path_trace`) for
/// `INTOXICATED_TRACE_PATHS=1`. Every function here is fixed-arity except `open`,
/// and `openat`, whose mode is read only when the flags say one was passed, so unlike
/// `INTOXICATED_TRACE=1`, which wraps variadics with fixed-arity declarations and
/// has been seen to make the engine abort, this trace changes nothing it
/// watches. errno is kept across the line, so a traced failure reports what
/// it would have untraced.
static void trace(const char* call, const char* path, const char* result) {
    if (!g_path_trace.load(std::memory_order_relaxed)) {
        return;
    }
    const int saved = errno;
    std::fprintf(stderr, "[paths] tid=%ld %s(\"%s\") = %s\n",
                 intoxicated_gettid(), call, path ? path : "(null)", result);
    errno = saved;
}

/// `trace` with a numeric result.
static void trace_i(const char* call, const char* path, long result) {
    if (!g_path_trace.load(std::memory_order_relaxed)) {
        return;
    }
    char text[24];
    std::snprintf(text, sizeof text, "%ld", result);
    trace(call, path, text);
}

// ------------------------------------------------------------ the /system redirect

/// The directory standing in for Android's `/system`, without trailing
/// slashes. A length of 0 turns the redirect off.
static char g_root[PATH_MAX];
static std::atomic<size_t> g_root_len{0};

/// A path that begins with exactly `/system/` becomes the root followed by
/// the rest of the path from its `/` on, written into `buf` (`n` bytes).
/// Anything else, and a rewrite that would not fit with its NUL, is `path`
/// itself. Null for a null path.
static const char* remap(const char* path, char* buf, size_t n) {
    if (!path) {
        return nullptr;
    }
    // `/proc` is never redirected. Every read the engine makes there is
    // answered by the synthesis below (`INTOXICATED_FAKE_PROC`) from FreeBSD's
    // own sysctls; nothing under /compat/linux is used, ever.
    if (g_root_len == 0) {
        return path;
    }
    if (std::strncmp(path, "/system/", 8) != 0) {
        return path;
    }
    const char* rest = path + 7;
    const size_t root_len = g_root_len.load(std::memory_order_acquire);
    const size_t rest_len = std::strlen(rest);
    if (buf == nullptr || root_len + rest_len + 1 > n) {
        return path;
    }
    std::memcpy(buf, g_root, root_len);
    std::memcpy(buf + root_len, rest, rest_len + 1);
    return buf;
}

/// Declares `real`, the path the call actually uses: `p` through the redirect,
/// into a buffer on the caller's stack.
#define REMAP(p)              \
    char real_buf_[PATH_MAX]; \
    const char* real = remap((p), real_buf_, sizeof real_buf_)

#if defined(__FreeBSD__)
/// Binaries a genuine, non-rooted Android device never ships, but the FreeBSD
/// host does -- most notably `/usr/bin/su`. The engine's Android anti-cheat runs a
/// root check that probes for exactly these, and because the runtime otherwise lets
/// the engine see the host filesystem, the check *found* `/usr/bin/su` and the
/// server disconnected with reason 304 (`DisconnectAndroidAnticheatKick`,
/// "missing or corrupted files"; confirmed by RE: the finding's own detail string
/// read `Found Rel File: /usr/bin/su`). Declining to export these host binaries
/// to the engine is not the forbidden "shape /proc to pass a check": it is
/// correcting the runtime's filesystem view so a path that cannot exist on the device
/// it claims to be does not exist. Matched by basename so every probe path for a
/// tool (e.g. /usr/bin/su, /sbin/su, /system/xbin/su) reads as absent, while the
/// shared libraries the runtime legitimately dlopen()s (never named after these
/// tools) are untouched.
static bool android_absent_file(const char* path) {
    if (!path) {
        return false;
    }
    const char* base = std::strrchr(path, '/');
    base = base ? base + 1 : path;
    static const char* const tells[] = {
        "su", "busybox", "magisk", "magiskhide", "magiskinit", "magiskpolicy",
        "daemonsu", "supolicy", "ksud", "ddexe", "superuser", "Superuser.apk",
        "frida-server", "frida-helper", "re.frida.server",
    };
    for (const char* t : tells) {
        if (std::strcmp(base, t) == 0) {
            return true;
        }
    }
    return false;
}
#define ANDROID_HIDE(path, failval)      \
    do {                                 \
        if (android_absent_file(path)) { \
            errno = ENOENT;              \
            trace_i("hidden", (path), -1); \
            return (failval);            \
        }                                \
    } while (0)
#else
#define ANDROID_HIDE(path, failval) do { } while (0)
#endif

#if defined(__FreeBSD__)
// bionic's `struct stat` on x86-64 is the Linux kernel layout (~144 bytes);
// FreeBSD's is larger (~224 bytes, extra st_birthtim/st_flags/st_gen). Writing a
// FreeBSD struct into the engine's bionic-sized buffer overruns it and trips the
// caller's stack canary (seen in boost::filesystem::status). Translate.
struct bionic_stat {
    unsigned long st_dev;
    unsigned long st_ino;
    unsigned long st_nlink;
    unsigned int st_mode;
    unsigned int st_uid;
    unsigned int st_gid;
    unsigned int __pad0;
    unsigned long st_rdev;
    long st_size;
    long st_blksize;
    long st_blocks;
    struct timespec st_atim;
    struct timespec st_mtim;
    struct timespec st_ctim;
    long __reserved3[3];
};
static void to_bionic_stat(const struct stat* s, bionic_stat* b) {
    memset(b, 0, sizeof(*b));
    b->st_dev = s->st_dev;
    b->st_ino = s->st_ino;
    b->st_nlink = s->st_nlink;
    b->st_mode = s->st_mode;
    b->st_uid = s->st_uid;
    b->st_gid = s->st_gid;
    b->st_rdev = s->st_rdev;
    b->st_size = s->st_size;
    b->st_blksize = s->st_blksize;
    b->st_blocks = s->st_blocks;
    b->st_atim = s->st_atim;
    b->st_mtim = s->st_mtim;
    b->st_ctim = s->st_ctim;
}

int s_stat(const char* path, void* out) {
    ANDROID_HIDE(path, -1);
    REMAP(path);
    struct stat native;
    int r = ::stat(real, &native);
    trace_i("stat", real, r);
    if (r == 0) to_bionic_stat(&native, (bionic_stat*)out);
    return r;
}

int s_lstat(const char* path, void* out) {
    ANDROID_HIDE(path, -1);
    REMAP(path);
    struct stat native;
    int r = ::lstat(real, &native);
    trace_i("lstat", real, r);
    if (r == 0) to_bionic_stat(&native, (bionic_stat*)out);
    return r;
}

int s_fstat(int fd, void* out) {
    struct stat native;
    int r = ::fstat(fd, &native);
    if (r == 0) to_bionic_stat(&native, (bionic_stat*)out);
    return r;
}
#endif

/// `access`: the root-tool hiding, then the redirect. The mode bits agree
/// between Linux and FreeBSD.
int s_access(const char* path, int mode) {
    ANDROID_HIDE(path, -1);
    REMAP(path);
    int r = ::access(real, mode);
    trace_i("access", real, r);
    return r;
}

#if defined(__FreeBSD__)
// Whether the /proc and /sys synthesis is on; the launcher turns it on.
static bool fake_proc_on() {
    return std::getenv("INTOXICATED_FAKE_PROC") != nullptr;
}

// `/proc/self/<leaf>` or `/proc/<this process's pid>/<leaf>`.
static bool is_self_file(const char* path, const char* leaf) {
    if (path == nullptr || std::strncmp(path, "/proc/", 6) != 0) {
        return false;
    }
    const char* p = path + 6;
    if (std::strncmp(p, "self/", 5) == 0) {
        p += 5;
    } else {
        char pid[24];
        int n = std::snprintf(pid, sizeof pid, "%d/", static_cast<int>(getpid()));
        if (n <= 0 || std::strncmp(p, pid, static_cast<size_t>(n)) != 0) {
            return false;
        }
        p += n;
    }
    return std::strcmp(p, leaf) == 0;
}

static bool is_self_maps(const char* path) {
    return is_self_file(path, "maps");
}

// With the synthesis on, `/proc` is answered by it or not at all: a path it
// does not serve is absent (ENOENT), never read from the host's own procfs,
// whose FreeBSD layout the engine cannot parse and which is itself a tell.
static bool proc_answered_only_here(const char* path) {
    return path != nullptr && fake_proc_on() && std::strncmp(path, "/proc", 5) == 0 &&
           (path[5] == '/' || path[5] == '\0');
}

// A stream over a copy of `data` that the stream owns and frees on fclose,
// so a buffer the next synthesis on this thread refills cannot change what an
// open stream still reads.
static FILE* memstream_of(const char* data, size_t len) {
    FILE* f = ::fmemopen(nullptr, len + 1, "w+");
    if (f == nullptr) {
        return nullptr;
    }
    if (len > 0 && ::fwrite(data, 1, len, f) != len) {
        ::fclose(f);
        return nullptr;
    }
    ::rewind(f);
    return f;
}

// power.rs's answer for `/sys/class/power_supply/<supply>/<attribute>`: the
// battery and AC state the engine is told through BatteryManager, or -1 for a
// node this machine would not have. Handed over by the Rust side
// (`bionic::system_path_overrides`) rather than linked by name, so the native
// archives still link where the Rust crate is not part of the program.
using PowerSupplyReader = long (*)(const char* path, char* buf, size_t cap);
static std::atomic<PowerSupplyReader> g_power_supply_reader{nullptr};

// ---------- Native /proc process enumeration
//
// The engine's anticheat lists /proc and reads each /proc/<pid>/cmdline to scan
// running processes for cheat tools, plus /proc/self/fd and /proc/self/auxv.
// Served here from FreeBSD's own `kern.proc` sysctls and `elf_aux_info`, so the
// port needs no Linux procfs at all. This is NOT faking a process list: it
// reports the real processes, so the anticheat's scan sees the same thing a
// Linux procfs would show it. Gated on INTOXICATED_FAKE_PROC, and kept
// entirely off the real-directory path RbxStorage's content cache walks -- that
// path (the one the 304 dirent bug lived on) is untouched; a synthetic dir is a
// distinct, registered object `s_readdir`/`s_closedir` recognise by pointer.

struct SynthDir {
    std::vector<std::pair<std::string, unsigned char>> entries;  // (name, DT_*)
    size_t idx = 0;
};
static std::mutex g_synthdir_mu;
static std::unordered_set<SynthDir*> g_synthdirs;

// Every live PID, via KERN_PROC_PROC (processes, not threads). Empty on failure,
// which leaves the scan with nothing to walk rather than a wrong answer.
static std::vector<int> intoxicated_enumerate_pids() {
    std::vector<int> pids;
    int mib[3] = {CTL_KERN, KERN_PROC, KERN_PROC_PROC};
    size_t len = 0;
    if (sysctl(mib, 3, nullptr, &len, nullptr, 0) != 0 || len == 0) {
        return pids;
    }
    len += len / 4 + sizeof(struct kinfo_proc);  // headroom for races
    std::vector<char> buf(len);
    if (sysctl(mib, 3, buf.data(), &len, nullptr, 0) != 0) {
        return pids;
    }
    size_t n = len / sizeof(struct kinfo_proc);
    const auto* kp = reinterpret_cast<const struct kinfo_proc*>(buf.data());
    for (size_t i = 0; i < n; ++i) {
        if (kp[i].ki_pid > 0) {
            pids.push_back(kp[i].ki_pid);
        }
    }
    return pids;
}

// A pid's argv as the Linux /proc/<pid>/cmdline blob: NUL-separated, so it must
// carry a length rather than ride a strlen path. Written into `out`, returns the
// byte count (0 on failure or a kernel thread with no argv).
static size_t intoxicated_proc_cmdline(int pid, char* out, size_t cap) {
    int mib[4] = {CTL_KERN, KERN_PROC, KERN_PROC_ARGS, pid};
    size_t len = cap;
    if (sysctl(mib, 4, out, &len, nullptr, 0) != 0) {
        return 0;
    }
    return len;
}

// A Linux-shaped auxv (array of 8+8-byte type/value pairs, AT_NULL-terminated)
// built from the values FreeBSD exposes. bionic already took its real auxv off
// the stack at exec; this secondary /proc read just needs plausible, valid
// entries. Written into `out`, returns the byte count.
static size_t intoxicated_proc_auxv(char* out, size_t cap) {
    struct Aux { uint64_t type, val; };
    static unsigned char at_random[16] = {0};
    if (at_random[0] == 0 && at_random[15] == 0) {
        // Fill once from the real stack-canary source if available.
        unsigned long r = 0;
        if (elf_aux_info(AT_PAGESZ, &r, sizeof r) != 0) {
            r = 4096;
        }
        for (int i = 0; i < 16; ++i) {
            at_random[i] = static_cast<unsigned char>((getpid() * 2654435761u) >> (i % 4 * 8)) ^ (i + 1);
        }
    }
    unsigned long pagesz = 4096, clktck = 100, hwcap = 0, hwcap2 = 0;
    elf_aux_info(AT_PAGESZ, &pagesz, sizeof pagesz);
    elf_aux_info(AT_HWCAP, &hwcap, sizeof hwcap);
#ifdef AT_HWCAP2
    elf_aux_info(AT_HWCAP2, &hwcap2, sizeof hwcap2);
#endif
    const Aux aux[] = {
        {6 /*AT_PAGESZ*/, pagesz},
        {17 /*AT_CLKTCK*/, clktck},
        {16 /*AT_HWCAP*/, hwcap},
        {26 /*AT_HWCAP2*/, hwcap2},
        {11 /*AT_UID*/, (uint64_t)getuid()},
        {12 /*AT_EUID*/, (uint64_t)geteuid()},
        {13 /*AT_GID*/, (uint64_t)getgid()},
        {14 /*AT_EGID*/, (uint64_t)getegid()},
        {23 /*AT_SECURE*/, 0},
        {25 /*AT_RANDOM*/, (uint64_t)(uintptr_t)at_random},
        {0 /*AT_NULL*/, 0},
    };
    size_t n = sizeof aux;
    if (n > cap) n = cap;
    memcpy(out, aux, n);
    return n;
}

// Binary /proc content (embedded NULs), returned with an explicit length so it
// cannot ride the strlen path the text synth uses. Non-negative return means
// handled. Thread-local buffer: the engine reads these serially per thread.
static const char* synth_proc_bin(const char* path, size_t* out_len) {
    if (path == nullptr || std::getenv("INTOXICATED_FAKE_PROC") == nullptr) {
        return nullptr;
    }
    static thread_local char buf[8192];
    if (std::strcmp(path, "/proc/self/auxv") == 0) {
        *out_len = intoxicated_proc_auxv(buf, sizeof buf);
        return buf;
    }
    // /proc/<pid>/cmdline, but not self/0 (those stay the Android package name
    // the existing text synth already serves). Numeric pid only.
    size_t plen = std::strlen(path);
    if (plen > 14 && std::strncmp(path, "/proc/", 6) == 0 &&
        std::strcmp(path + plen - 8, "/cmdline") == 0) {
        int pid = 0;
        const char* p = path + 6;
        if (*p >= '1' && *p <= '9') {  // a real numeric pid, not "self" or "0"
            for (; *p >= '0' && *p <= '9'; ++p) pid = pid * 10 + (*p - '0');
            if (std::strcmp(p, "/cmdline") == 0 && pid > 0) {
                *out_len = intoxicated_proc_cmdline(pid, buf, sizeof buf);
                return buf;
            }
        }
    }
    return nullptr;
}

// A synthetic DIR for /proc and /proc/self/fd, or nullptr for anything else.
static DIR* synth_opendir(const char* path) {
    if (path == nullptr || std::getenv("INTOXICATED_FAKE_PROC") == nullptr) {
        return nullptr;
    }
    const char* p = path;
    SynthDir* sd = nullptr;
    if (std::strcmp(p, "/proc") == 0 || std::strcmp(p, "/proc/") == 0) {
        sd = new SynthDir();
        sd->entries.push_back({".", DT_DIR});
        sd->entries.push_back({"..", DT_DIR});
        sd->entries.push_back({"self", DT_LNK});
        char num[16];
        for (int pid : intoxicated_enumerate_pids()) {
            std::snprintf(num, sizeof num, "%d", pid);
            sd->entries.push_back({num, DT_DIR});
        }
    } else if (std::strcmp(p, "/proc/self/fd") == 0 ||
               std::strcmp(p, "/proc/self/fd/") == 0) {
        sd = new SynthDir();
        sd->entries.push_back({".", DT_DIR});
        sd->entries.push_back({"..", DT_DIR});
        char num[16];
        int maxfd = static_cast<int>(sysconf(_SC_OPEN_MAX));
        if (maxfd <= 0 || maxfd > 65536) maxfd = 1024;
        for (int fd = 0; fd < maxfd; ++fd) {
            if (fcntl(fd, F_GETFD) != -1) {
                std::snprintf(num, sizeof num, "%d", fd);
                sd->entries.push_back({num, DT_LNK});
            }
        }
    }
    if (sd == nullptr) {
        return nullptr;
    }
    {
        std::lock_guard<std::mutex> g(g_synthdir_mu);
        g_synthdirs.insert(sd);
    }
    return reinterpret_cast<DIR*>(sd);
}

static bool is_synth_dir(DIR* d) {
    std::lock_guard<std::mutex> g(g_synthdir_mu);
    return g_synthdirs.count(reinterpret_cast<SynthDir*>(d)) != 0;
}
#endif  // __FreeBSD__

/// `opendir`: the synthetic /proc directories, then the redirect. A failed
/// open is traced as `opendir!`.
DIR* s_opendir(const char* path) {
#if defined(__FreeBSD__)
    if (DIR* synth = synth_opendir(path)) {
        trace("opendir", path, "synth-proc");
        return synth;
    }
#endif
    REMAP(path);
    DIR* d = ::opendir(real);
    trace(d ? "opendir" : "opendir!", real, d ? "ok" : "null");
    return d;
}

#if defined(__FreeBSD__)
// Same class of bug as bionic_stat: the engine was compiled against bionic's
// `struct dirent`, whose x86-64 layout is the Linux one --
//   d_ino @0 (u64), d_off @8 (i64), d_reclen @16 (u16), d_type @18 (u8),
//   d_name @19.
// FreeBSD's `struct dirent` (post-ino64) instead has d_type @18, then a
// d_pad0 byte @19, d_namlen @20, d_pad1 @22, and d_name only @24. With
// `--host-libc`, `readdir()` resolves to FreeBSD libc and returns that layout;
// the engine then reads `d_name` at offset 19, which on FreeBSD is the zero
// d_pad0 byte -- so every name reads back EMPTY. RbxStorage's content-addressed
// cache names each file by its hash and validates the name it reads back, so
// an empty name is logged as `getSubDirFileNames, found file with invalid
// hash:` (nothing after the colon) and `getSubDirSize` cannot enumerate, which
// the server tallies into the 304 "missing or corrupted files" kick at the
// 60 s grace. Translate the record into bionic's layout, mirroring s_stat.
struct __attribute__((packed)) bionic_dirent {
    uint64_t      d_ino;
    int64_t       d_off;
    uint16_t      d_reclen;
    unsigned char d_type;
    char          d_name[256];
};
static_assert(offsetof(bionic_dirent, d_type) == 18, "bionic d_type @18");
static_assert(offsetof(bionic_dirent, d_name) == 19, "bionic d_name @19");

struct dirent* s_readdir(DIR* d) {
    // Valid until the next readdir on this thread, which matches bionic's
    // contract for the common single-stream enumeration RbxStorage does.
    thread_local bionic_dirent slot;
    // Synthetic /proc or /proc/self/fd: walk the entry list built at opendir.
    // Same single-stream, same-thread contract; this path never calls into the
    // real readdir translation RbxStorage depends on.
    if (is_synth_dir(d)) {
        SynthDir* sd = reinterpret_cast<SynthDir*>(d);
        if (sd->idx >= sd->entries.size()) {
            return nullptr;
        }
        const auto& e = sd->entries[sd->idx++];
        memset(&slot, 0, sizeof slot);
        slot.d_ino = sd->idx;  // nonzero; readers only require it be set
        slot.d_off = static_cast<int64_t>(sd->idx);
        slot.d_type = e.second;
        size_t nl = e.first.size();
        if (nl > sizeof(slot.d_name) - 1) nl = sizeof(slot.d_name) - 1;
        memcpy(slot.d_name, e.first.data(), nl);
        slot.d_name[nl] = '\0';
        slot.d_reclen = static_cast<uint16_t>(offsetof(bionic_dirent, d_name) + nl + 1);
        return reinterpret_cast<struct dirent*>(&slot);
    }
    struct ::dirent* fb = ::readdir(d);
    if (!fb) return nullptr;
    memset(&slot, 0, sizeof slot);
    slot.d_ino = fb->d_fileno;
    slot.d_off = fb->d_off;
    slot.d_type = fb->d_type;  // DT_* values match between FreeBSD and bionic
    size_t nl = fb->d_namlen;
    if (nl > sizeof(slot.d_name) - 1) nl = sizeof(slot.d_name) - 1;
    memcpy(slot.d_name, fb->d_name, nl);
    slot.d_name[nl] = '\0';
    slot.d_reclen = (uint16_t)(offsetof(bionic_dirent, d_name) + nl + 1);
    return reinterpret_cast<struct dirent*>(&slot);
}

int s_closedir(DIR* d) {
    if (is_synth_dir(d)) {
        SynthDir* sd = reinterpret_cast<SynthDir*>(d);
        {
            std::lock_guard<std::mutex> g(g_synthdir_mu);
            g_synthdirs.erase(sd);
        }
        delete sd;
        return 0;
    }
    return ::closedir(d);
}
#endif

/// `realpath`. With the caller's buffer the host resolves straight into it.
///
/// Without one, `realpath(path, NULL)` asks the host libc to allocate the
/// answer, and the engine frees what it is given through its own statically
/// linked allocator, which cannot free a host block (the old notes record one
/// such block faulting the engine's HttpClient thread). So that form never
/// returns host memory: it resolves into private storage, for the trace only,
/// and answers null, with ENOTSUP (FreeBSD's 45, which the engine reads
/// through `__errno` as Linux's 95) when the path would have resolved, and the
/// host's own errno from the failed resolution when it would not.
char* s_realpath(const char* path, char* resolved) {
    REMAP(path);
    if (resolved != nullptr) {
        char* r = ::realpath(real, resolved);
        trace("realpath", real, r != nullptr ? r : "null");
        return r;
    }
    char scratch[PATH_MAX];
    if (::realpath(real, scratch) == nullptr) {
        trace("realpath", real, "null (no caller buffer; does not resolve)");
        return nullptr;
    }
    char note[PATH_MAX + 48];
    std::snprintf(note, sizeof note, "null (no caller buffer; resolves to %s)", scratch);
    trace("realpath", real, note);
    errno = ENOTSUP;
    return nullptr;
}

// `realpath(path, NULL)` above is one of three calls the engine imports that
// can hand it a block the host libc allocated. The other two are below, and
// they get the same answer for the same reason: on FreeBSD the host allocator
// is jemalloc, which the engine's statically linked mimalloc knows no more
// about than it knew about glibc's, and libroblox.so imports no `malloc` or
// `free` and exports none either -- so there is no block the runtime could obtain
// that the engine's `free` would accept, and refusing is the only answer that
// is both safe and true. Both used to fall through the symbol table's Generic path
// straight to libc.so.7.
//
// **INFERRED, not observed:** an outside gdb watch for both across two
// signed-out runs and saw four `getcwd` calls, every one with a 128-byte
// caller buffer, and no `vasprintf` at all; nothing here was seen to fire on
// FreeBSD either. Each refusal says so on stderr the first time it happens, so
// a run that reaches one names it rather than faulting somewhere unrelated in
// the engine's allocator, the way the `realpath` case did on the HttpClient
// thread before it was caught.
//
// A caveat that matters if this file's table ever serves more than one guest
// library: the bundled libbacktrace-native.so and librenderscript-toolkit.so
// also import `vasprintf`, and unlike libroblox.so they import `free` too, so
// for them the host's answer would be correct. The runtime loads only libroblox.so
// today, so the refusal reaches nothing else.
std::atomic<bool> g_getcwd_refusal_said{false};
std::atomic<bool> g_vasprintf_refusal_said{false};

/// `getcwd`, forwarded whenever the caller supplies the buffer -- which is the
/// only form the engine has been seen to use -- and refused when it does not.
///
/// A null `buf` asks the library to `malloc` the answer, on FreeBSD for any
/// `size` (zero meaning "as large as needed"), so every null-buffer call is the
/// hazardous one. `ENOMEM` is the errno `getcwd` documents for failing to
/// allocate that buffer, and it is 12 in both the Linux and FreeBSD numbering,
/// so the engine reads it unchanged through `__errno`. The forwarded form's own
/// failures, `EINVAL` for a zero size and `ERANGE` for a short buffer, are also
/// below 35 and agree in both.
char* s_getcwd(char* buf, size_t size) {
    if (buf) {
        char* r = ::getcwd(buf, size);
        trace("getcwd", r ? r : "-", r ? "ok" : "null");
        return r;
    }
    if (!g_getcwd_refusal_said.exchange(true, std::memory_order_relaxed)) {
        std::fprintf(stderr,
                     "[alloc] getcwd(NULL, %zu) refused with ENOMEM: the host libc would "
                     "allocate the answer and the engine would free it through its own "
                     "allocator. Reached for the first time; later refusals are silent.\n",
                     size);
    }
    trace("getcwd", "(null buffer)", "null (refused: ENOMEM)");
    errno = ENOMEM;
    return nullptr;
}

/// `vasprintf`, which allocates its result unconditionally and so has no form
/// that can be forwarded.
///
/// Refused as an allocation failure: -1 and `ENOMEM`. `*strp` is documented as
/// undefined on failure; it is cleared rather than left alone, so a caller that
/// skips the return check and frees it anyway frees null instead of whatever
/// was on its stack. `ap` is never read, which is what makes the refusal safe
/// to call with any argument list.
///
/// The format is logged because it is the engine's own constant and the
/// quickest route to the call site; the arguments are not, because they are
/// whatever the engine was formatting.
int s_vasprintf(char** strp, const char* fmt, va_list ap) {
    (void)ap;
    if (strp) {
        *strp = nullptr;
    }
    if (!g_vasprintf_refusal_said.exchange(true, std::memory_order_relaxed)) {
        std::fprintf(stderr,
                     "[alloc] vasprintf(\"%.64s\") refused with ENOMEM: its result would be "
                     "a host-libc block the engine frees through its own allocator. Reached "
                     "for the first time; later refusals are silent.\n",
                     fmt ? fmt : "(null)");
    }
    errno = ENOMEM;
    return -1;
}

/// `readlink`, through the redirect.
ssize_t s_readlink(const char* path, char* buf, size_t n) {
    REMAP(path);
    const ssize_t r = ::readlink(real, buf, n);
    trace_i("readlink", real, static_cast<long>(r));
    return r;
}

#if defined(__FreeBSD__)
/// This process as Linux's /proc/self/stat and /proc/self/status report it,
/// from `kern.proc.pid`: sizes, thread count, CPU times in USER_HZ (100)
/// ticks, faults, process group and session, start time since boot.
struct SelfFacts {
    unsigned long vsize = 0;
    long rss_pages = 0;
    long text_pages = 0, data_pages = 0;
    int threads = 1;
    long utime = 0, stime = 0, cutime = 0, cstime = 0;
    unsigned long minflt = 0, majflt = 0, cminflt = 0, cmajflt = 0;
    int pgrp = 0, sid = 0, nice = 0, cpu = 0;
    unsigned long long start_ticks = 0;
};

static long user_hz_ticks(const struct timeval& tv) {
    return static_cast<long>(tv.tv_sec) * 100 + static_cast<long>(tv.tv_usec) / 10000;
}

static SelfFacts self_facts() {
    SelfFacts f;
    struct kinfo_proc kp;
    size_t len = sizeof kp;
    int mib[4] = {CTL_KERN, KERN_PROC, KERN_PROC_PID, static_cast<int>(getpid())};
    if (sysctl(mib, 4, &kp, &len, nullptr, 0) != 0 || len < sizeof kp) {
        return f;
    }
    f.vsize = static_cast<unsigned long>(kp.ki_size);
    f.rss_pages = static_cast<long>(kp.ki_rssize);
    f.text_pages = static_cast<long>(kp.ki_tsize);
    f.data_pages = static_cast<long>(kp.ki_dsize) + static_cast<long>(kp.ki_ssize);
    f.threads = kp.ki_numthreads;
    f.utime = user_hz_ticks(kp.ki_rusage.ru_utime);
    f.stime = user_hz_ticks(kp.ki_rusage.ru_stime);
    f.cutime = user_hz_ticks(kp.ki_rusage_ch.ru_utime);
    f.cstime = user_hz_ticks(kp.ki_rusage_ch.ru_stime);
    f.minflt = static_cast<unsigned long>(kp.ki_rusage.ru_minflt);
    f.majflt = static_cast<unsigned long>(kp.ki_rusage.ru_majflt);
    f.cminflt = static_cast<unsigned long>(kp.ki_rusage_ch.ru_minflt);
    f.cmajflt = static_cast<unsigned long>(kp.ki_rusage_ch.ru_majflt);
    f.pgrp = kp.ki_pgid;
    f.sid = kp.ki_sid;
    f.nice = kp.ki_nice;
    f.cpu = kp.ki_lastcpu;
    struct timeval boot {};
    size_t blen = sizeof boot;
    if (sysctlbyname("kern.boottime", &boot, &blen, nullptr, 0) == 0) {
        long long d = (static_cast<long long>(kp.ki_start.tv_sec) - boot.tv_sec) * 100 +
                      (static_cast<long long>(kp.ki_start.tv_usec) - boot.tv_usec) / 10000;
        f.start_ticks = d > 0 ? static_cast<unsigned long long>(d) : 0;
    }
    return f;
}

/// /proc/self/stat in Linux's 52-field layout, under the package name the
/// rest of the synthesis uses. Fields Linux hides from other processes
/// (code and stack addresses, signal bitmaps) are 0.
static const char* proc_self_stat() {
    static thread_local char buf[512];
    const SelfFacts f = self_facts();
    std::snprintf(buf, sizeof buf,
        "%ld (com.roblox.client) R %ld %d %d 0 -1 4194560 %lu %lu %lu %lu %ld %ld %ld %ld %d %d %d 0 %llu %lu %ld "
        "18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 %d 0 0 0 0 0 0 0 0 0 0 0 0 0\n",
        static_cast<long>(getpid()), static_cast<long>(getppid()), f.pgrp, f.sid, f.minflt, f.cminflt,
        f.majflt, f.cmajflt, f.utime, f.stime, f.cutime, f.cstime, 20 + f.nice, f.nice, f.threads,
        f.start_ticks, f.vsize, f.rss_pages, f.cpu);
    return buf;
}
/// A synthetic Android `/proc/self/mounts`, served ONLY when `INTOXICATED_FAKE_PROC`
/// is set. The engine's anti-tamper reads `/proc/self/mounts` (confirmed by
/// `INTOXICATED_TRACE_PATHS`), and the real FreeBSD ZFS mount table -- `zroot/ROOT
/// ... zfs`, `/dev/gpt/efiboot0 vfat` -- is a plain "this is not Android" tell
/// that no `ro.product.*` / `Build.*` value can hide, because it is the mount
/// table, not a device string. This deliberately shapes `/proc` to satisfy that
/// read, which `remap()` above rules out -- hence the env gate: it
/// is off by default and the honest passthrough stays the default.
static const char* synth_proc_content(const char* path) {
    if (std::getenv("INTOXICATED_FAKE_PROC") == nullptr) {
        return nullptr;
    }
    // /proc/self/stat: read several times a second in game (through
    // `__open_2`), for its CPU times and thread count. Real values.
    if (is_self_file(path, "stat")) {
        return proc_self_stat();
    }
    // /proc/self/statm: read about 18 times a second in game for the
    // resident size. Linux's seven page counts: size, resident, shared,
    // text, lib (always 0), data + stack, dirty (always 0).
    if (is_self_file(path, "statm")) {
        static thread_local char statm[160];
        const SelfFacts f = self_facts();
        std::snprintf(statm, sizeof statm, "%lu %ld 0 %ld 0 %ld 0\n", f.vsize / 4096, f.rss_pages,
                      f.text_pages, f.data_pages);
        return statm;
    }
    // Read once by the allocator to learn whether the kernel overcommits.
    // FreeBSD does by default (vm.overcommit 0), which is Linux's 0.
    if (std::strcmp(path, "/proc/sys/vm/overcommit_memory") == 0) {
        return "0\n";
    }
    // The process cmdline. On Android a Roblox process's cmdline is its package
    // name, "com.roblox.client"; the client's real cmdline is "intoxicated-run ...",
    // and the engine reads /proc/<pid>/cmdline (seen in INTOXICATED_TRACE_PATHS as
    // /proc/0/cmdline and /proc/self/cmdline). An anti-tamper that checks "is my
    // process the Roblox package?" fails on the binary's name. Serve the package
    // name instead. (Single arg, no trailing NUL: the reader uses the byte count
    // fread returns, then takes the basename -- no '/', so it sees the package.)
    if (std::strcmp(path, "/proc/self/cmdline") == 0 ||
        std::strcmp(path, "/proc/0/cmdline") == 0) {
        return "com.roblox.client";
    }
    if (std::strcmp(path, "/proc/self/mounts") == 0 ||
        std::strcmp(path, "/proc/mounts") == 0) {
        return
            "rootfs / rootfs ro,seclabel 0 0\n"
            "tmpfs /dev tmpfs rw,seclabel,nosuid,relatime,mode=755 0 0\n"
            "devpts /dev/pts devpts rw,seclabel,relatime,mode=600 0 0\n"
            "proc /proc proc rw,relatime,gid=3009,hidepid=2 0 0\n"
            "sysfs /sys sysfs rw,seclabel,relatime 0 0\n"
            "selinuxfs /sys/fs/selinux selinuxfs rw,relatime 0 0\n"
            "/dev/block/dm-0 /system ext4 ro,seclabel,relatime 0 0\n"
            "/dev/block/dm-1 /system_ext ext4 ro,seclabel,relatime 0 0\n"
            "/dev/block/dm-2 /vendor ext4 ro,seclabel,relatime 0 0\n"
            "/dev/block/dm-3 /product ext4 ro,seclabel,relatime 0 0\n"
            "/dev/block/by-name/userdata /data f2fs "
            "rw,seclabel,nosuid,nodev,noatime 0 0\n"
            "/dev/block/by-name/metadata /metadata ext4 "
            "rw,seclabel,nosuid,nodev,noatime 0 0\n"
            "tmpfs /apex tmpfs ro,seclabel,relatime,mode=755 0 0\n"
            "/dev/block/loop0 /apex/com.android.runtime ext4 "
            "ro,seclabel,relatime 0 0\n"
            "tmpfs /storage tmpfs rw,seclabel,nosuid,nodev,relatime,mode=755 0 0\n"
            "/data/media /storage/emulated sdcardfs "
            "rw,nosuid,nodev,noatime 0 0\n";
    }
    // The OOM score. Linux/Android answers a small integer; FreeBSD has no such
    // node and the real read returns null -- another "not Linux" tell.
    if (std::strcmp(path, "/proc/self/oom_score") == 0) {
        return "0\n";
    }
    // The open UNIX-domain socket table. An Android anti-cheat reads this to scan
    // for an injected cheat engine's IPC socket (Synapse/Velocity and the like
    // bind named UNIX sockets); FreeBSD has no `/proc/net/unix`, so the real read
    // returns null and the scan cannot run -- which reads as "cannot verify this
    // client is clean". Serve a plausible, exploit-free Android socket table so
    // the scan completes and finds nothing. Same env gate and the same exception.
    if (std::strcmp(path, "/proc/net/unix") == 0) {
        return
            "Num       RefCount Protocol Flags    Type St Inode Path\n"
            "0000000000000000: 00000002 00000000 00010000 0001 01 2871 "
            "/dev/socket/property_service\n"
            "0000000000000000: 00000002 00000000 00010000 0001 01 2903 "
            "/dev/socket/logdw\n"
            "0000000000000000: 00000002 00000000 00010000 0001 01 2904 "
            "/dev/socket/logdr\n"
            "0000000000000000: 00000002 00000000 00010000 0001 01 3155 "
            "/dev/socket/zygote\n"
            "0000000000000000: 00000003 00000000 00000000 0001 03 31840\n";
    }
    // /proc/self/status. The engine parses `State:` and `Threads:` from it (binary
    // carries "State: ", "Threads8", "Threads16", ...) and anti-tamper reads
    // `TracerPid:` here to detect a debugger. FreeBSD has no Linux procfs, so
    // without this a reader sees no State/Threads/TracerPid. Serve a Linux/Android
    // status with TracerPid:0 (not traced), State:R, the real pids, and the real
    // sizes and thread count, which /proc/self/stat reports as well and the two
    // must agree on.
    if (std::strcmp(path, "/proc/self/status") == 0 ||
        (std::strncmp(path, "/proc/", 6) == 0 &&
         std::strcmp(path + std::strlen(path) - 7, "/status") == 0)) {
        static thread_local char status_buf[2048];
        long pid = static_cast<long>(getpid());
        long ppid = static_cast<long>(getppid());
        const SelfFacts f = self_facts();
        const unsigned long vm_kb = f.vsize / 1024;
        const unsigned long rss_kb = static_cast<unsigned long>(f.rss_pages) * 4;
        std::snprintf(status_buf, sizeof status_buf,
            "Name:\tcom.roblox.client\n"
            "Umask:\t0077\n"
            "State:\tR (running)\n"
            "Tgid:\t%ld\n"
            "Ngid:\t0\n"
            "Pid:\t%ld\n"
            "PPid:\t%ld\n"
            "TracerPid:\t0\n"
            "Uid:\t10234\t10234\t10234\t10234\n"
            "Gid:\t10234\t10234\t10234\t10234\n"
            "FDSize:\t512\n"
            "Groups:\t3003 9997 20234 50234\n"
            "VmPeak:\t%8lu kB\n"
            "VmSize:\t%8lu kB\n"
            "VmRSS:\t%8lu kB\n"
            "Threads:\t%d\n"
            "SigQ:\t0/12000\n"
            "SigPnd:\t0000000000000000\n"
            "SigBlk:\t0000000000000000\n"
            "SigIgn:\t0000000000000000\n"
            "SigCgt:\t0000000000000000\n"
            "CapInh:\t0000000000000000\n"
            "CapPrm:\t0000000000000000\n"
            "CapEff:\t0000000000000000\n"
            "CapBnd:\t0000000000000000\n"
            "Seccomp:\t0\n"
            "Cpus_allowed:\tffff\n"
            "Cpus_allowed_list:\t0-15\n"
            "Mems_allowed:\t1\n"
            "Mems_allowed_list:\t0\n",
            pid, pid, ppid, vm_kb, vm_kb, rss_kb, f.threads);
        return status_buf;
    }
    // /proc/meminfo. The engine sizes memory from `MemTotal` (its allocator
    // arenas and the texture-streaming budget) and `build_user_agent` reports it
    // as the device's RAM. Synthesised from `hw.physmem` and the page counts,
    // which is what lets the port need no Linux procfs at all, and what fixed
    // the `0MB` the User-Agent reported while nothing answered this read.
    // Real numbers, filled once: installed RAM does not change mid-run.
    if (std::strcmp(path, "/proc/meminfo") == 0) {
        static char meminfo_buf[512];
        static bool meminfo_filled = false;
        if (!meminfo_filled) {
            unsigned long physmem = 0;
            size_t len = sizeof physmem;
            if (sysctlbyname("hw.physmem", &physmem, &len, nullptr, 0) != 0) {
                physmem = 0;
            }
            long pagesize = sysconf(_SC_PAGESIZE);
            if (pagesize <= 0) {
                pagesize = 4096;
            }
            unsigned int free_pages = 0;
            len = sizeof free_pages;
            sysctlbyname("vm.stats.vm.v_free_count", &free_pages, &len, nullptr, 0);
            unsigned int inactive_pages = 0;
            len = sizeof inactive_pages;
            sysctlbyname("vm.stats.vm.v_inactive_count", &inactive_pages, &len, nullptr, 0);
            long total_kb = static_cast<long>(physmem / 1024);
            long free_kb =
                static_cast<long>(static_cast<unsigned long>(free_pages) *
                                  static_cast<unsigned long>(pagesize) / 1024);
            long cached_kb =
                static_cast<long>(static_cast<unsigned long>(inactive_pages) *
                                  static_cast<unsigned long>(pagesize) / 1024);
            // Linux "available" counts reclaimable (here, inactive) pages too.
            long avail_kb = free_kb + cached_kb;
            if (total_kb > 0 && avail_kb > total_kb) {
                avail_kb = total_kb;
            }
            std::snprintf(meminfo_buf, sizeof meminfo_buf,
                "MemTotal:       %ld kB\n"
                "MemFree:        %ld kB\n"
                "MemAvailable:   %ld kB\n"
                "Buffers:        0 kB\n"
                "Cached:         %ld kB\n"
                "SwapCached:     0 kB\n"
                "SwapTotal:      0 kB\n"
                "SwapFree:       0 kB\n",
                total_kb, free_kb, avail_kb, cached_kb);
            meminfo_filled = true;
        }
        return meminfo_buf;
    }
    // /proc/cpuinfo. The engine counts `processor` blocks to size its worker
    // pool and reads the flags line; real codepath feature selection is CPUID,
    // not this text, so a baseline x86-64 flag set is safe. One block per
    // `hw.ncpu`, model and vendor from `hw.model`. Filled once.
    if (std::strcmp(path, "/proc/cpuinfo") == 0) {
        static std::string cpuinfo;
        static bool cpuinfo_filled = false;
        if (!cpuinfo_filled) {
            int ncpu = 0;
            size_t len = sizeof ncpu;
            if (sysctlbyname("hw.ncpu", &ncpu, &len, nullptr, 0) != 0 || ncpu <= 0) {
                ncpu = 1;
            }
            char model[256] = "x86_64 Processor";
            len = sizeof model;
            sysctlbyname("hw.model", model, &len, nullptr, 0);
            int mhz = 0;
            len = sizeof mhz;
            sysctlbyname("hw.clockrate", &mhz, &len, nullptr, 0);
            const char* vendor = std::strstr(model, "AMD") ? "AuthenticAMD"
                                                           : "GenuineIntel";
            for (int i = 0; i < ncpu; ++i) {
                char block[1024];
                std::snprintf(block, sizeof block,
                    "processor\t: %d\n"
                    "vendor_id\t: %s\n"
                    "cpu family\t: 6\n"
                    "model\t\t: 1\n"
                    "model name\t: %s\n"
                    "stepping\t: 0\n"
                    "cpu MHz\t\t: %d.000\n"
                    "cache size\t: 1024 KB\n"
                    "physical id\t: 0\n"
                    "siblings\t: %d\n"
                    "core id\t\t: %d\n"
                    "cpu cores\t: %d\n"
                    "fpu\t\t: yes\n"
                    "flags\t\t: fpu vme de pse tsc msr pae mce cx8 apic sep mtrr "
                    "pge mca cmov pat pse36 clflush mmx fxsr sse sse2 ss ht "
                    "syscall nx lm constant_tsc rep_good nopl pni pclmulqdq "
                    "ssse3 fma cx16 sse4_1 sse4_2 movbe popcnt aes xsave avx "
                    "f16c rdrand lahf_lm abm bmi1 avx2 bmi2 rdseed adx "
                    "clflushopt\n\n",
                    i, vendor, model, mhz, ncpu, i, ncpu);
                cpuinfo += block;
            }
            cpuinfo_filled = true;
        }
        return cpuinfo.c_str();
    }
    return nullptr;
}
#endif

#if defined(__FreeBSD__)
/// Android-ise `/proc/self/maps`, served only under `INTOXICATED_FAKE_PROC`. The
/// engine reads its own memory map, and the real one names FreeBSD's loader and
/// libc, the raw-mmap'd `libroblox.so` from `~/.cache`, and the
/// `intoxicated-run` binary -- every line a "not Android" tell. This reads the real
/// map and rewrites only the path column, keeping every address, so anything that
/// cross-checks an address against the map still matches. The same exception
/// as the mount fake; same env gate.
/// Synthetic Android sysfs for the CPU-frequency and battery nodes the engine
/// polls. FreeBSD has no `/sys`, so every one of these reads returns null -- and
/// the engine polls `scaling_cur_freq` hundreds of times a session, so what the
/// server's device-health report sees is a "device" whose CPU frequency, core
/// topology and battery are all unreadable: a tell no `ro.product.*` value can
/// cover, and the mirror of the `/proc/self/mounts` case. FreeBSD has no sysfs,
/// so without this every one of these reads fails. Served only
/// under `INTOXICATED_FAKE_PROC`, the same env gate and exception as the /proc fakes.
/// Values model a 16-core x86_64 Android device (what the client reports elsewhere).
static const char* synth_sys_content(const char* path) {
    if (path == nullptr || std::getenv("INTOXICATED_FAKE_PROC") == nullptr) {
        return nullptr;
    }
    if (std::strncmp(path, "/sys/devices/system/cpu/", 24) == 0) {
        if (std::strstr(path, "/cpufreq/") != nullptr) {
            if (std::strstr(path, "time_in_state") != nullptr) {
                return "2400000 1000\n1800000 3000\n1200000 8000\n";
            }
            if (std::strstr(path, "min_freq") != nullptr) {
                return "1200000\n";
            }
            if (std::strstr(path, "max_freq") != nullptr) {
                return "2400000\n";
            }
            if (std::strstr(path, "cur_freq") != nullptr) {
                return "1800000\n";
            }
        }
        if (std::strstr(path, "tsc_freq_khz") != nullptr) {
            return "2400000\n";
        }
        if (std::strstr(path, "/cpu/online") != nullptr ||
            std::strstr(path, "/cpu/present") != nullptr ||
            std::strstr(path, "/cpu/possible") != nullptr) {
            return "0-15\n";
        }
    }
    if (std::strncmp(path, "/sys/class/power_supply/", 24) == 0) {
        // What power.rs tells the engine through BatteryManager, as sysfs
        // reads it: a desktop has no battery and is on AC, a laptop has its
        // real capacity and state. A node that machine would not have is
        // absent, and the read fails as it would on Linux.
        static thread_local char supply[128];
        PowerSupplyReader read = g_power_supply_reader.load(std::memory_order_acquire);
        if (read != nullptr && read(path, supply, sizeof supply) >= 0) {
            return supply;
        }
        return nullptr;
    }
    return nullptr;
}

/// What the maps rewrite matches, computed once rather than per line. The
/// `--lib-dir` and `--apk` this process was started with arrive through
/// `intoxicated_set_maps_sources` before the engine loads; a maps read before that
/// (or from a caller that never sets them) gets the launcher's layout under
/// HOME, which is what the hardcoded `/home/pascal/...` rules used to spell.
static std::mutex g_maps_mutex;
static std::string g_maps_lib_dir;
static std::string g_maps_apk;
static intoxicated::maps::Sources* g_maps_sources = nullptr;

static intoxicated::maps::Sources maps_sources() {
    std::lock_guard<std::mutex> lock(g_maps_mutex);
    if (!g_maps_sources) {
        g_maps_sources = new intoxicated::maps::Sources(intoxicated::maps::sources_for_this_process(
            g_maps_lib_dir.c_str(), g_maps_apk.c_str()));
    }
    return *g_maps_sources;
}

/// This process's map in Linux's /proc/self/maps layout, built from FreeBSD's
/// `kern.proc.vmmap`: `start-end perms offset major:minor inode`, then from
/// column 73 the file's path, or `[stack]` for the stack that grows down.
static std::string native_maps() {
    std::string out;
    int mib[4] = {CTL_KERN, KERN_PROC, KERN_PROC_VMMAP, static_cast<int>(getpid())};
    size_t len = 0;
    if (sysctl(mib, 4, nullptr, &len, nullptr, 0) != 0 || len == 0) {
        return out;
    }
    len += len / 3;  // the map can grow between the two calls
    std::vector<char> buf(len);
    if (sysctl(mib, 4, buf.data(), &len, nullptr, 0) != 0) {
        return out;
    }
    out.reserve(len / 4);
    for (size_t off = 0; off + offsetof(struct kinfo_vmentry, kve_path) <= len;) {
        const auto* kve = reinterpret_cast<const struct kinfo_vmentry*>(buf.data() + off);
        if (kve->kve_structsize <= 0) {
            break;
        }
        off += static_cast<size_t>(kve->kve_structsize);
        const int t = kve->kve_type;
        const bool file = t == KVME_TYPE_VNODE && kve->kve_path[0] != '\0';
        const bool shared = (t == KVME_TYPE_VNODE && (kve->kve_flags & KVME_FLAG_COW) == 0) ||
                            t == KVME_TYPE_DEVICE || t == KVME_TYPE_PHYS || t == KVME_TYPE_SG ||
                            t == KVME_TYPE_MGTDEVICE;
        const dev_t dev = file ? static_cast<dev_t>(kve->kve_vn_fsid) : 0;
        char head[160];
        int n = std::snprintf(head, sizeof head, "%08lx-%08lx %c%c%c%c %08llx %02x:%02x %llu ",
                              static_cast<unsigned long>(kve->kve_start),
                              static_cast<unsigned long>(kve->kve_end),
                              (kve->kve_protection & KVME_PROT_READ) ? 'r' : '-',
                              (kve->kve_protection & KVME_PROT_WRITE) ? 'w' : '-',
                              (kve->kve_protection & KVME_PROT_EXEC) ? 'x' : '-', shared ? 's' : 'p',
                              file ? static_cast<unsigned long long>(kve->kve_offset) : 0ULL,
                              static_cast<unsigned>(major(dev) & 0xfff), static_cast<unsigned>(minor(dev) & 0xff),
                              file ? static_cast<unsigned long long>(kve->kve_vn_fileid) : 0ULL);
        if (n <= 0 || static_cast<size_t>(n) >= sizeof head) {
            continue;
        }
        std::string line(head, static_cast<size_t>(n));
        const char* name = file ? kve->kve_path : (kve->kve_flags & KVME_FLAG_GROWS_DOWN) ? "[stack]" : nullptr;
        if (name != nullptr) {
            while (line.size() < 72) {
                line += ' ';
            }
            line += ' ';
            line += name;
        }
        line += '\n';
        out += line;
    }
    return out;
}

static std::string build_synth_maps() {
    std::string out;
    const std::string real = native_maps();
    if (real.empty()) {
        return out;
    }
    const intoxicated::maps::Sources sources = maps_sources();
    out.reserve(real.size());
    size_t pos = 0;
    while (pos < real.size()) {
        size_t nl = real.find('\n', pos);
        size_t end = nl == std::string::npos ? real.size() : nl + 1;
        out += intoxicated::maps::rewrite_line(real.substr(pos, end - pos), sources);
        pos = end;
    }
    return out;
}

static FILE* synth_maps() {
    std::string out = build_synth_maps();
    if (std::getenv("INTOXICATED_DUMP_SYNTHMAPS")) {
        if (FILE* d = ::fopen("/tmp/intoxicated_synthmaps.txt", "w")) {
            ::fwrite(out.data(), 1, out.size(), d);
            ::fclose(d);
        }
    }
    if (out.empty()) {
        return nullptr;
    }
    char* buf = static_cast<char*>(std::malloc(out.size() + 1));
    if (!buf) {
        return nullptr;
    }
    std::memcpy(buf, out.data(), out.size());
    buf[out.size()] = '\0';
    // Leaked on purpose: fmemopen reads directly from this buffer, and maps is
    // read a handful of times per session, so the leak is bounded and tiny.
    return ::fmemopen(buf, out.size(), "r");
}

/// A seekable, readable fd holding `data` -- for serving the synthetic `/proc`
/// content through the raw `open()` path, not just `fopen()`. The engine's
/// anti-tamper reads `/proc/self/{maps,mounts,cmdline}` and `s_fopen` already
/// Android-ises those, but a reader that calls `open()`+`read()` bypasses the
/// stdio layer entirely and used to get the real FreeBSD procfs -- every "not
/// Android" tell intact. An anonymous temp file (created, then unlinked, so it
/// never appears in the tree) gives a real fd the reader can `read()`/`lseek()`.
static int fd_from_bytes(const char* data, size_t len) {
    char tmpl[] = "/tmp/intoxicated_synthproc_XXXXXX";
    int fd = ::mkstemp(tmpl);
    if (fd < 0) {
        return -1;
    }
    ::unlink(tmpl);
    size_t off = 0;
    while (off < len) {
        ssize_t w = ::write(fd, data + off, len - off);
        if (w <= 0) {
            ::close(fd);
            return -1;
        }
        off += static_cast<size_t>(w);
    }
    ::lseek(fd, 0, SEEK_SET);
    return fd;
}

/// Serve synthetic `/proc` or `/sys` content as a readable fd for ANY read-only
/// open/openat that reaches a path we Android-ise -- `open("/proc/self/maps")`,
/// but also `openat(dirfd, "/proc/self/maps", …)` with a real dirfd, which the
/// engine's anti-cheat uses and which otherwise skipped the synth and read the
/// real map (full of `/home/.cache`, `intoxicated-run`, `dconf` tells).
/// `path` is the original request (pre-remap); `host_flags` are post-translation
/// FreeBSD open flags. Returns a ready fd, or -1 to fall through to a real open.
// Linux filesystem magics. A synthetic `/proc` or `/sys` fd is a plain regular
// file (fd_from_bytes mkstemps one), so fstatfs() on it would report the host's
// filesystem (ZFS) -- and Roblox's anti-cheat opens `/proc/self/maps` with a RAW
// openat and fstatfs()es the fd precisely to check it is really on procfs. A
// mismatch reads as "/proc was replaced" -> tamper -> 304. Tag every synth fd
// with the magic it must report, and answer fstatfs()/statfs() from the tag.
#define INTOXICATED_PROC_SUPER_MAGIC 0x9fa0UL
#define INTOXICATED_SYSFS_MAGIC      0x62656572UL

static std::mutex g_synthfd_mu;
static std::unordered_map<int, unsigned long>& synthfd_map() {
    static std::unordered_map<int, unsigned long> m;
    return m;
}
static void synthfd_tag(int fd, unsigned long magic) {
    if (fd < 0) return;
    std::lock_guard<std::mutex> lk(g_synthfd_mu);
    synthfd_map()[fd] = magic;
}
static void synthfd_forget(int fd) {
    if (fd < 0) return;
    std::lock_guard<std::mutex> lk(g_synthfd_mu);
    synthfd_map().erase(fd);
}
// 1 + *magic if `fd` is a tagged synth fd; 0 otherwise.
extern "C" int intoxicated_synth_fd_magic(int fd, unsigned long* magic) {
    std::lock_guard<std::mutex> lk(g_synthfd_mu);
    auto it = synthfd_map().find(fd);
    if (it == synthfd_map().end()) return 0;
    if (magic) *magic = it->second;
    return 1;
}

static int try_synth_fd(const char* path, int host_flags) {
    if ((host_flags & (O_WRONLY | O_RDWR | O_CREAT)) != 0) {
        return -1;
    }
    // Binary /proc content (cmdline, auxv) first: it carries a length because it
    // has embedded NULs the strlen path below would truncate.
    {
        size_t blen = 0;
        if (const char* bsynth = synth_proc_bin(path, &blen)) {
            int fd = fd_from_bytes(bsynth, blen);
            synthfd_tag(fd, INTOXICATED_PROC_SUPER_MAGIC);
            return fd;
        }
    }
    if (const char* synth = synth_proc_content(path)) {
        int fd = fd_from_bytes(synth, std::strlen(synth));
        synthfd_tag(fd, INTOXICATED_PROC_SUPER_MAGIC);
        return fd;
    }
    if (const char* ssynth = synth_sys_content(path)) {
        int fd = fd_from_bytes(ssynth, std::strlen(ssynth));
        synthfd_tag(fd, INTOXICATED_SYSFS_MAGIC);
        return fd;
    }
    if (fake_proc_on() && is_self_maps(path)) {
        std::string m = build_synth_maps();
        if (!m.empty()) {
            int fd = fd_from_bytes(m.data(), m.size());
            synthfd_tag(fd, INTOXICATED_PROC_SUPER_MAGIC);
            return fd;
        }
    }
    return -1;
}

// Fill a bionic/Linux `struct statfs` (120 bytes) for a synth fd so the anti-cheat
// sees procfs/sysfs, or for a real fd from the host statfs with an ext4 magic (a
// genuine /data value, never ZFS). Shared by the libc `fstatfs`/`statfs` shims and
// the raw-syscall trap so both paths agree.
extern "C" int intoxicated_fill_linux_statfs(unsigned long magic, const struct ::statfs* host,
                                         void* out120) {
    unsigned long* o = static_cast<unsigned long*>(out120);
    std::memset(o, 0, 120);
    o[0]  = magic;                          // f_type
    o[1]  = host ? host->f_bsize : 4096;    // f_bsize
    o[2]  = host ? host->f_blocks : 0;      // f_blocks
    o[3]  = host ? host->f_bfree : 0;       // f_bfree
    o[4]  = host ? host->f_bavail : 0;      // f_bavail
    o[5]  = host ? host->f_files : 0;       // f_files
    o[6]  = host ? host->f_ffree : 0;       // f_ffree
    // o[7] = f_fsid (left zero)
    o[8]  = 255;                            // f_namelen
    o[9]  = host ? host->f_bsize : 4096;    // f_frsize
    o[10] = 0;                              // f_flags
    return 0;
}

int s_fstatfs(int fd, void* out) {
    unsigned long magic = 0;
    if (intoxicated_synth_fd_magic(fd, &magic)) {
        return intoxicated_fill_linux_statfs(magic, nullptr, out);
    }
    struct ::statfs host{};
    if (::fstatfs(fd, &host) != 0) {
        intoxicated_fbsd_errno_to_linux();
        return -1;
    }
    return intoxicated_fill_linux_statfs(0xEF53UL, &host, out);  // EXT4_SUPER_MAGIC
}

int s_statfs(const char* path, void* out) {
    const char* sp = path;
    if (std::strncmp(sp, "/proc", 5) == 0)
        return intoxicated_fill_linux_statfs(INTOXICATED_PROC_SUPER_MAGIC, nullptr, out);
    if (std::strncmp(sp, "/sys", 4) == 0)
        return intoxicated_fill_linux_statfs(INTOXICATED_SYSFS_MAGIC, nullptr, out);
    char _b[PATH_MAX];
    const char* real = remap(sp, _b, sizeof _b);
    struct ::statfs host{};
    if (::statfs(real ? real : path, &host) != 0) {
        intoxicated_fbsd_errno_to_linux();
        return -1;
    }
    return intoxicated_fill_linux_statfs(0xEF53UL, &host, out);
}

// Close shim: drop any synth-fd tag so a later reuse of the fd number does not
// inherit a stale procfs/sysfs magic, then close for real.
//
// This replaced freebsd_abi.c's `close` in the symbol table (d65a4c3) and with
// it the errno translation that one did, so a failed close reported FreeBSD
// numbers: ECONNRESET on a socket as 54, which Linux reads as EXFULL, not 104.
int s_close(int fd) {
    synthfd_forget(fd);
    const int r = ::close(fd);
    if (r < 0)
        intoxicated_fbsd_errno_to_linux();
    return r;
}
#endif

/// `fopen`: the root-tool hiding and the synthetic /proc and /sys content,
/// then the redirect.
FILE* s_fopen(const char* path, const char* mode) {
    ANDROID_HIDE(path, nullptr);
#if defined(__FreeBSD__)
    {
        size_t blen = 0;
        if (const char* bsynth = synth_proc_bin(path, &blen)) {
            trace("fopen", path, "synth-proc-bin");
            return memstream_of(bsynth, blen);
        }
    }
    if (const char* synth = synth_proc_content(path)) {
        trace("fopen", path, "synth-android");
        return memstream_of(synth, std::strlen(synth));
    }
    if (const char* ssynth = synth_sys_content(path)) {
        trace("fopen", path, "synth-sys");
        return memstream_of(ssynth, std::strlen(ssynth));
    }
    if (fake_proc_on() && is_self_maps(path)) {
        if (FILE* f = synth_maps()) {
            trace("fopen", path, "synth-maps");
            return f;
        }
    }
    if (proc_answered_only_here(path)) {
        errno = ENOENT;
        trace("fopen", path, "null (not synthesised)");
        return nullptr;
    }
#endif
    REMAP(path);
    FILE* f = ::fopen(real, mode);
    trace("fopen", real, f ? "ok" : "null");
    return f;
}

/// `open(path, flags, ...)`. The mode is read only when the flag word says the
/// caller passed one: reading it otherwise reads a register or stack slot
/// the caller never pushed, which is the mistake that makes `INTOXICATED_TRACE=1`
/// unsafe (see `crates/intoxicated-runtime/src/bionic/trace.rs`).
///
/// On FreeBSD the flags are the engine's *Linux* O_ bits, and the test for
/// whether a mode was passed has to be made in that vocabulary too: Linux
/// O_CREAT is 0x40, which FreeBSD calls O_ASYNC, so testing the host's O_CREAT
/// (0x200, Linux O_TRUNC) read a mode that was never passed on every truncating
/// open and skipped the one that was on every creating one.
int s_open(const char* path, int flags, ...) {
    ANDROID_HIDE(path, -1);
    unsigned mode = 0;
#if defined(__FreeBSD__)
    const bool has_mode = intoxicated_fbsd_open_takes_mode(flags) != 0;
#else
    const bool has_mode = (flags & (O_CREAT | O_TMPFILE)) != 0;
#endif
    if (has_mode) {
        va_list ap;
        va_start(ap, flags);
        mode = va_arg(ap, unsigned);
        va_end(ap);
    }
#if defined(__FreeBSD__)
    int host_flags;
    if (intoxicated_fbsd_open_flags(flags, &host_flags) != 0) {
        trace_i("open", path, -1);
        return -1;
    }
    flags = host_flags;
    // Serve the same synthetic /proc content `s_fopen` does, but through the raw
    // `open()` path: an anti-tamper that reads /proc/self/{maps,mounts,cmdline}
    // with open()+read() instead of fopen() otherwise gets the real FreeBSD
    // procfs here (full of ~/.cache and intoxicated-run tells). Only the
    // read path is synthesised; a writing/creating open falls through.
    {
        int sfd = try_synth_fd(path, flags);
        if (sfd >= 0) {
            trace_i("open", path, sfd);
            return sfd;
        }
    }
    if (proc_answered_only_here(path)) {
        errno = ENOENT;
        trace_i("open", path, -1);
        return -1;
    }
#endif
    REMAP(path);
    int r = has_mode ? ::open(real, flags, mode) : ::open(real, flags);
#if defined(__FreeBSD__)
    if (r < 0)
        intoxicated_fbsd_errno_to_linux();
#endif
    trace_i("open", real, r);
    return r;
}

/// bionic's `struct statvfs` on x86-64: eleven 8-byte fields in this order,
/// then six reserved 32-bit words, 112 bytes. FreeBSD's is 88 bytes with the
/// same fields in another order, so it is copied field by field. The ST_RDONLY
/// (1) and ST_NOSUID (2) flag bits agree.
struct bionic_statvfs {
    unsigned long f_bsize;
    unsigned long f_frsize;
    unsigned long f_blocks;
    unsigned long f_bfree;
    unsigned long f_bavail;
    unsigned long f_files;
    unsigned long f_ffree;
    unsigned long f_favail;
    unsigned long f_fsid;
    unsigned long f_flag;
    unsigned long f_namemax;
    uint32_t reserved[6];
};
static_assert(sizeof(bionic_statvfs) == 112, "bionic statvfs is 112 bytes");
static_assert(offsetof(bionic_statvfs, f_fsid) == 64, "bionic f_fsid @64");
static_assert(offsetof(bionic_statvfs, f_namemax) == 80, "bionic f_namemax @80");
static_assert(offsetof(bionic_statvfs, reserved) == 88, "bionic reserved @88");

/// `statvfs`, through the redirect, answered in bionic's layout with every
/// reserved byte zero. A null `out` gets the host's result with nothing
/// written; on failure `out` is untouched.
int s_statvfs(const char* path, void* out) {
    REMAP(path);
    struct ::statvfs host;
    const int r = ::statvfs(real, &host);
    trace_i("statvfs", real, r);
    if (r == 0 && out != nullptr) {
        bionic_statvfs b;
        std::memset(&b, 0, sizeof b);
        b.f_bsize = host.f_bsize;
        b.f_frsize = host.f_frsize;
        b.f_blocks = host.f_blocks;
        b.f_bfree = host.f_bfree;
        b.f_bavail = host.f_bavail;
        b.f_files = host.f_files;
        b.f_ffree = host.f_ffree;
        b.f_favail = host.f_favail;
        b.f_fsid = host.f_fsid;
        b.f_flag = host.f_flag;
        b.f_namemax = host.f_namemax;
        std::memcpy(out, &b, sizeof b);
    }
    return r;
}

// The engine is a bionic binary and overwhelmingly uses the `*at` syscalls, not
// the legacy ones: `access` is `faccessat`, `stat` is `newfstatat`, `open` is
// `openat`. Those were never hooked, so they reached the host directly -- which is
// how the anti-cheat's root check saw FreeBSD's `/usr/bin/su` despite `s_open`/
// `s_access` hiding it (304). Hook them too. For the ordinary AT_FDCWD case they
// delegate to the already-correct legacy shims (which do the hide, the /proc and
// /system remap, the Linux->FreeBSD flag translation and the bionic `struct stat`
// layout); a real directory fd (rare here) falls through to the host after the
// same hide + remap.
#ifndef INTOXICATED_AT_FDCWD
#define INTOXICATED_AT_FDCWD (-100) // identical on Linux and FreeBSD
#endif
int s_openat(int dirfd, const char* path, int flags, ...) {
    ANDROID_HIDE(path, -1);
    unsigned mode = 0;
#if defined(__FreeBSD__)
    const bool has_mode = intoxicated_fbsd_open_takes_mode(flags) != 0;
#else
    const bool has_mode = (flags & (O_CREAT | O_TMPFILE)) != 0;
#endif
    if (has_mode) {
        va_list ap;
        va_start(ap, flags);
        mode = va_arg(ap, unsigned);
        va_end(ap);
    }
    if (dirfd == INTOXICATED_AT_FDCWD) {
        return has_mode ? s_open(path, flags, mode) : s_open(path, flags);
    }
#if defined(__FreeBSD__)
    int host_flags;
    if (intoxicated_fbsd_open_flags(flags, &host_flags) != 0) {
        return -1;
    }
    flags = host_flags;
    // Serve synth /proc even with a real dirfd and an absolute path: the engine's
    // anti-cheat reads /proc/self/maps via openat(dirfd, "/proc/self/maps", …),
    // which never reached the AT_FDCWD→s_open synth above and so got the real
    // host-leaking map. Check the original (pre-remap) path.
    {
        int sfd = try_synth_fd(path, flags);
        if (sfd >= 0) {
            trace_i("openat", path, sfd);
            return sfd;
        }
    }
    if (proc_answered_only_here(path)) {
        errno = ENOENT;
        trace_i("openat", path, -1);
        return -1;
    }
#endif
    REMAP(path);
    int r = has_mode ? ::openat(dirfd, real, flags, mode) : ::openat(dirfd, real, flags);
#if defined(__FreeBSD__)
    // freebsd_abi.c's `openat` did this before this wrapper replaced it in the
    // symbol table; without it a caller that reads the errno slot directly
    // (a hoisted `__errno()`) sees FreeBSD numbers, ENAMETOOLONG as 63 not 36.
    if (r < 0)
        intoxicated_fbsd_errno_to_linux();
#endif
    trace_i("openat", real, r);
    return r;
}

/// FORTIFY's `open(path, flags)`, which bionic's headers route an `open`
/// with no mode to. It gets everything `open` does: the root-file hiding, the
/// Linux flag translation, the synthetic /proc and /sys and the path rewrite.
/// (The fixed ABI table's `__open_2`, which this replaces for the engine, only
/// rewrote the path, so the engine's root check reached /usr/bin/su through
/// it, and its /proc reads skipped the synthesis.) A flag word that asks for a
/// mode cannot come from a correct caller; it is given 0.
extern "C" void intoxicated_set_power_supply_reader(long (*read)(const char*, char*, size_t)) {
#if defined(__FreeBSD__)
    g_power_supply_reader.store(read, std::memory_order_release);
#else
    (void)read;
#endif
}

extern "C" int intoxicated_open_2(const char* path, int flags) {
#if defined(__FreeBSD__)
    if (intoxicated_fbsd_open_takes_mode(flags) != 0) {
        return s_open(path, flags, 0u);
    }
#endif
    return s_open(path, flags);
}

int s_faccessat(int dirfd, const char* path, int mode, int flags) {
    ANDROID_HIDE(path, -1);
    if (dirfd == INTOXICATED_AT_FDCWD) {
        return s_access(path, mode);
    }
    REMAP(path);
    int r = ::faccessat(dirfd, real, mode, flags);
    trace_i("faccessat", real, r);
    return r;
}

#if defined(__FreeBSD__)
// Linux AT_SYMLINK_NOFOLLOW is 0x100; the engine passes Linux flag values.
#define INTOXICATED_LX_AT_SYMLINK_NOFOLLOW 0x100
int s_fstatat(int dirfd, const char* path, void* out, int flags) {
    ANDROID_HIDE(path, -1);
    if (dirfd == INTOXICATED_AT_FDCWD) {
        return (flags & INTOXICATED_LX_AT_SYMLINK_NOFOLLOW) ? s_lstat(path, out)
                                                        : s_stat(path, out);
    }
    REMAP(path);
    struct stat native;
    int r = ::fstatat(dirfd, real, &native,
                      (flags & INTOXICATED_LX_AT_SYMLINK_NOFOLLOW) ? AT_SYMLINK_NOFOLLOW : 0);
    trace_i("fstatat", real, r);
    if (r == 0) {
        to_bionic_stat(&native, (bionic_stat*)out);
    }
    return r;
}
#endif

/// The directory standing in for Android's `/system`
/// (`crates/intoxicated-runtime/src/android/system.rs` builds it and links the
/// fonts into it). Copied, at most PATH_MAX bytes with the NUL, then stripped
/// of trailing slashes while longer than one character. Null or "" turns the
/// redirect off.
extern "C" void intoxicated_set_system_root(const char* root) {
    g_root_len.store(0, std::memory_order_release);
    if (root == nullptr || root[0] == '\0') {
        return;
    }
    size_t len = ::strnlen(root, sizeof g_root - 1);
    std::memcpy(g_root, root, len);
    g_root[len] = '\0';
    while (len > 1 && g_root[len - 1] == '/') {
        g_root[--len] = '\0';
    }
    g_root_len.store(len, std::memory_order_release);
}

/// The same rewrite for freebsd_abi.c, whose `open`/`__open_2`/`openat`
/// replaced this file's for the engine when the ABI layer landed and so
/// silently dropped the /proc and /system redirects: the engine then logged
/// `Failed to open /proc/meminfo` and reported 0 MB of memory. Returns `path`
/// itself when nothing applies.
extern "C" const char* intoxicated_path_remap(const char* path, char* buf, size_t n) {
    const char* r = remap(path, buf, n);
    return r ? r : path;
}

/// 1 if `path` is a host binary a genuine Android device never has (su, magisk,
/// …). Exposed with C linkage so the libc shims in `freebsd_libc_compat.c` --
/// `__open_2` and friends, which the engine's root check reaches through and
/// which call the host `open()` directly rather than `s_open` -- can decline it
/// too. See `android_absent_file`.
extern "C" int intoxicated_path_is_hidden(const char* path) {
#if defined(__FreeBSD__)
    return android_absent_file(path) ? 1 : 0;
#else
    (void)path;
    return 0;
#endif
}

/// The path trace (see `trace`): non-zero on, zero off. bionic/mod.rs turns
/// it on for `INTOXICATED_TRACE_PATHS`. Independent of the root.
extern "C" void intoxicated_set_path_trace(int on) {
    g_path_trace.store(on != 0, std::memory_order_relaxed);
}

/// The `--lib-dir` and `--apk` this process runs, for the `/proc/self/maps`
/// rewrite. Without them it can only guess the launcher's layout, and a build
/// or APK anywhere else kept its real path in the map the anticheat reads.
extern "C" void intoxicated_set_maps_sources(const char* lib_dir, const char* apk) {
#if defined(__FreeBSD__)
    std::lock_guard<std::mutex> lock(g_maps_mutex);
    g_maps_lib_dir = lib_dir ? lib_dir : "";
    g_maps_apk = apk ? apk : "";
    delete g_maps_sources;
    g_maps_sources = nullptr;
#else
    (void)lib_dir;
    (void)apk;
#endif
}

/// The table, in the order the Rust side installs it. Every name stays:
/// freebsd_libc_compat.c looks several of them up by name.
extern "C" const IntoxicatedSymbol* intoxicated_system_symbols(size_t* count) {
    static const IntoxicatedSymbol table[] = {
        {"stat", (void*)&s_stat},
        {"lstat", (void*)&s_lstat},
#if defined(__FreeBSD__)
        {"fstat", (void*)&s_fstat},
        {"fstatat", (void*)&s_fstatat},
        {"newfstatat", (void*)&s_fstatat},
#endif
        {"access", (void*)&s_access},
        {"faccessat", (void*)&s_faccessat},
        {"openat", (void*)&s_openat},
        {"opendir", (void*)&s_opendir},
#if defined(__FreeBSD__)
        {"readdir", (void*)&s_readdir},
        {"closedir", (void*)&s_closedir},
#endif
        {"realpath", (void*)&s_realpath},
        {"getcwd", (void*)&s_getcwd},
        {"vasprintf", (void*)&s_vasprintf},
        {"readlink", (void*)&s_readlink},
        {"fopen", (void*)&s_fopen},
        {"statvfs", (void*)&s_statvfs},
        {"fstatfs", (void*)&s_fstatfs},
        {"statfs", (void*)&s_statfs},
        {"close", (void*)&s_close},
        {"open", (void*)&s_open},
    };
    if (count != nullptr) {
        *count = intoxicated_table_size(table);
    }
    return table;
}
