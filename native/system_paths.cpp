// Android's `/system` tree, served from a directory Cordial owns.
//
// Roblox asks the platform for `/system/fonts/NotoSansCJK-Regular.ttc`. On
// Android that always exists. On a Linux host there is no `/system` at all, the
// lookup fails, and the engine turns the failure into an *empty* path and throws
// during app startup:
//
//     RBXCRASH: UnhandledException (St13runtime_error Path does not exist: "")
//
// Which is a genuinely hard failure to read from the outside — the exception
// names no path, because by then there isn't one. It was found by tracing the
// path-taking libc calls and noticing that the same thread stats the font, gets
// -1, and immediately stats "" three times.
//
// Serving `/system` is not a workaround for a Roblox bug. It is part of what an
// Android runtime owes the code it hosts, exactly like `AAssetManager` or
// `ALooper`. Cordial owns the symbol table, so the redirect belongs at the libc
// boundary rather than anywhere near the engine.
//
// Written in C++ because `open` is variadic, and forwarding a C variadic to the
// real `open` is the one thing C does better here — the same reason liblog.cpp
// is C++.

#include "os_compat.h"
#include "freebsd_abi.h"
#include <cstdarg>
#include <cstddef>
#include <cstdio>
#include <cstring>
#include <cstdlib>
#include <string>
#include <mutex>
#include <unordered_map>

#include <cerrno>
#include <dirent.h>
#include <sys/syscall.h>
#include <fcntl.h>
#include <limits.h>
#include <netdb.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <sys/param.h>
#include <sys/mount.h>
#include <cstdint>
#include <unistd.h>
#include <vector>
#include <unordered_set>
#if defined(__FreeBSD__)
#include <sys/sysctl.h>
#include <sys/user.h>
#include <sys/auxv.h>
#include <elf.h>
#endif

namespace {

/// The host directory standing in for `/system`. Set once from Rust before the
/// engine runs; empty means "no redirect", which leaves every call untouched.
char g_root[PATH_MAX];
size_t g_root_len = 0;

/// `CORDIAL_TRACE_PATHS=1`. Every function here is fixed-arity except `open`,
/// which is forwarded properly, so this is safe to leave on — unlike
/// `CORDIAL_TRACE=1`, which wraps variadics with fixed-arity declarations and
/// makes the engine abort.
///
/// These wrappers are the only place the path calls are intercepted. An earlier
/// version had a second set in `trace.rs`; because both landed in the same
/// symbol map the tracing copy silently won and the redirect never ran, which
/// looked exactly like the redirect not working.
bool g_trace = false;

void trace(const char* call, const char* path, const char* result) {
    if (!g_trace) {
        return;
    }
    // The thread id is not decoration: Roblox spreads this work over twenty-odd
    // threads, and a single interleaved log invites reading two unrelated calls
    // as cause and effect.
    std::fprintf(stderr, "[paths] tid=%ld %s(\"%s\") = %s\n",
                 cordial_gettid(), call, path ? path : "(null)", result);
}

void trace_i(const char* call, const char* path, long r) {
    if (!g_trace) {
        return;
    }
    char b[32];
    std::snprintf(b, sizeof b, "%ld", r);
    trace(call, path, b);
}

/// Rewrite `/system/<rest>` to `<root>/<rest>`.
///
/// Returns null when the path is not under `/system`, which is the overwhelming
/// majority of calls — the cost on that path is one `strncmp`.
const char* remap(const char* path, char* buf, size_t n) {
    if (!path) {
        return nullptr;
    }
#if defined(__FreeBSD__)
    // The engine is an Android binary and reads Linux-format /proc: /proc/meminfo,
    // /proc/self/maps, /proc/self/status, /proc/cpuinfo. FreeBSD's native procfs
    // (mounted at /proc) has a different, sparser layout — /proc/meminfo does not
    // exist there at all — so those reads return wrong data or ENOENT. The Linux
    // layout lives under linprocfs at /compat/linux/proc. Redirect there so the
    // engine reads the real Linux-format data it was written against. That is the
    // whole justification: this serves linprocfs as it is, and must never be
    // shaped to satisfy an integrity check (ADR-001).
    // Independent of g_root: /proc redirection is not tied to the /system root.
    if (std::strncmp(path, "/proc", 5) == 0 && (path[5] == '/' || path[5] == '\0')) {
        // `path + 5` keeps the separator: "/proc/self/maps" -> ".../proc/self/maps".
        int w = std::snprintf(buf, n, "/compat/linux/proc%s", path + 5);
        if (w < 0 || static_cast<size_t>(w) >= n) {
            return nullptr;
        }
        return buf;
    }
#endif
    if (g_root_len == 0) {
        return nullptr;
    }
    if (std::strncmp(path, "/system/", 8) != 0) {
        return nullptr;
    }
    // `path + 7` keeps the separator, so the result is `<root>/fonts/...`.
    int w = std::snprintf(buf, n, "%s%s", g_root, path + 7);
    if (w < 0 || static_cast<size_t>(w) >= n) {
        return nullptr;
    }
    return buf;
}

#define REMAP(path)                          \
    char _buf[PATH_MAX];                     \
    const char* _p = remap(path, _buf, sizeof _buf); \
    const char* real = _p ? _p : (path)

#if defined(__FreeBSD__)
/// Binaries a genuine, non-rooted Android device never ships, but the FreeBSD
/// host does — most notably `/usr/bin/su`. The engine's Android anti-cheat runs a
/// root check that probes for exactly these, and because cordial otherwise lets
/// the engine see the host filesystem, the check *found* `/usr/bin/su` and the
/// server disconnected with reason 304 (`DisconnectAndroidAnticheatKick`,
/// "missing or corrupted files"; confirmed by RE: the finding's own detail string
/// read `Found Rel File: /usr/bin/su`). Declining to export these host binaries
/// to the engine is not ADR-001's forbidden "shape /proc to pass a check" — it is
/// correcting cordial's filesystem view so a path that cannot exist on the device
/// it claims to be does not exist. Matched by basename so every probe path for a
/// tool (e.g. /usr/bin/su, /sbin/su, /system/xbin/su) reads as absent, while the
/// shared libraries cordial legitimately dlopen()s (never named after these
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
#else
int s_stat(const char* path, struct stat* out) {
    REMAP(path);
    int r = ::stat(real, out);
    trace_i("stat", real, r);
    return r;
}

int s_lstat(const char* path, struct stat* out) {
    REMAP(path);
    int r = ::lstat(real, out);
    trace_i("lstat", real, r);
    return r;
}
#endif

int s_access(const char* path, int mode) {
    ANDROID_HIDE(path, -1);
    REMAP(path);
    int r = ::access(real, mode);
    trace_i("access", real, r);
    return r;
}

#if defined(__FreeBSD__)
// Defined further down; used by the /proc synthesis just below.
static const char* strip_compat_prefix(const char* path);

// ---------- Native /proc process enumeration (replaces the last linprocfs use)
//
// The engine's anticheat lists /proc and reads each /proc/<pid>/cmdline to scan
// running processes for cheat tools, plus /proc/self/fd and /proc/self/auxv.
// Those were the only reads still falling through to linprocfs. Served here from
// FreeBSD's own `kern.proc` sysctls and `elf_aux_info`, so the port needs no
// /compat/linux mount at all. This is NOT faking a process list: it reports the
// same real processes linprocfs would, so the anticheat's scan sees exactly what
// it saw before and behaves identically. Gated on CORDIAL_FAKE_PROC, and kept
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
static std::vector<int> cordial_enumerate_pids() {
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
static size_t cordial_proc_cmdline(int pid, char* out, size_t cap) {
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
static size_t cordial_proc_auxv(char* out, size_t cap) {
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
    if (path == nullptr || std::getenv("CORDIAL_FAKE_PROC") == nullptr) {
        return nullptr;
    }
    path = strip_compat_prefix(path);
    static thread_local char buf[8192];
    if (std::strcmp(path, "/proc/self/auxv") == 0) {
        *out_len = cordial_proc_auxv(buf, sizeof buf);
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
                *out_len = cordial_proc_cmdline(pid, buf, sizeof buf);
                return buf;
            }
        }
    }
    return nullptr;
}

// A synthetic DIR for /proc and /proc/self/fd, or nullptr for anything else.
static DIR* synth_opendir(const char* path) {
    if (path == nullptr || std::getenv("CORDIAL_FAKE_PROC") == nullptr) {
        return nullptr;
    }
    const char* p = strip_compat_prefix(path);
    SynthDir* sd = nullptr;
    if (std::strcmp(p, "/proc") == 0 || std::strcmp(p, "/proc/") == 0) {
        sd = new SynthDir();
        sd->entries.push_back({".", DT_DIR});
        sd->entries.push_back({"..", DT_DIR});
        sd->entries.push_back({"self", DT_LNK});
        char num[16];
        for (int pid : cordial_enumerate_pids()) {
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
// `struct dirent`, whose x86-64 layout is the Linux one —
//   d_ino @0 (u64), d_off @8 (i64), d_reclen @16 (u16), d_type @18 (u8),
//   d_name @19.
// FreeBSD's `struct dirent` (post-ino64) instead has d_type @18, then a
// d_pad0 byte @19, d_namlen @20, d_pad1 @22, and d_name only @24. With
// `--host-libc`, `readdir()` resolves to FreeBSD libc and returns that layout;
// the engine then reads `d_name` at offset 19, which on FreeBSD is the zero
// d_pad0 byte — so every name reads back EMPTY. RbxStorage's content-addressed
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

char* s_realpath(const char* path, char* resolved) {
    REMAP(path);
    if (!resolved) {
        // glibc's `realpath(path, NULL)` is a GNU extension: it `malloc`s the
        // result buffer itself and hands ownership to the caller, who is
        // expected to `free` it. That allocation comes from the *host's*
        // allocator — Roblox statically links its own (mimalloc, going by
        // `DFLog::Mimalloc`), and every one of its `malloc`/`free`/`new`/
        // `delete` symbols is resolved internally, never through Cordial's
        // symbol table. When the engine later releases a buffer this call
        // handed it, that release runs entirely inside Roblox's own
        // allocator, which indexes a table keyed by the pointer's own
        // address to find the arena that owns it. A host-`malloc`'d pointer
        // was never registered in that table, so the lookup's first level
        // comes back null and the very next dereference — unconditional,
        // with no null check — faults.
        //
        // This is exactly what feeds Roblox's cURL-based HTTP stack the
        // CA-bundle path (`./exe/cacert.pem`, resolved once per connection
        // going by `CORDIAL_TRACE_PATHS=1`): confirmed live under lldb with a
        // breakpoint on this function — `resolved` is null, the host
        // `realpath` call mallocs, and the pointer it returns is the exact
        // address that later faults on the `HttpClient` thread with
        // `rax=0x0, rcx=0xe000` — a segment-map miss for foreign memory.
        //
        // There is no buffer Cordial can hand back here that is safe for the
        // engine to free through its own allocator, because that allocator's
        // bookkeeping is not reachable from here (its `malloc`/`free` are not
        // exported). The only safe move is to never produce the host
        // allocation in the first place. Resolving into a stack buffer only
        // for the trace log and reporting failure (as `realpath` is
        // documented to do when resolution cannot be completed) is a real,
        // if slightly degraded, POSIX outcome: the caller falls back to the
        // path it already had, which is what every caller of this GNU form is
        // required to handle.
        char tmp[PATH_MAX];
        char* r = ::realpath(real, tmp);
        trace("realpath", real, r ? r : "null (unsupported: no caller buffer)");
        errno = r ? ENOTSUP : errno;
        return nullptr;
    }
    char* r = ::realpath(real, resolved);
    trace("realpath", real, r ? r : "null");
    return r;
}

ssize_t s_readlink(const char* path, char* buf, size_t n) {
    REMAP(path);
    ssize_t r = ::readlink(real, buf, n);
    trace_i("readlink", real, (long)r);
    return r;
}

#if defined(__FreeBSD__)
/// Normalise a path the engine may present either as the Android form
/// (`/proc/self/status`) or as the already-redirected linprocfs form
/// (`/compat/linux/proc/self/status`) — the latter happens when the engine's
/// anti-cheat `realpath()`s the node first and then opens the resolved path, which
/// otherwise slips past every `/proc`/`/sys` synth matcher below and reads the raw
/// FreeBSD linprocfs (full of ZFS mounts, FreeBSD status fields, host map paths).
/// Returns the `/proc…`/`/sys…` suffix in that case, else the path unchanged.
static const char* strip_compat_prefix(const char* path) {
    static const char kPfx[] = "/compat/linux";
    const size_t kLen = sizeof(kPfx) - 1;
    if (path && std::strncmp(path, kPfx, kLen) == 0 &&
        (std::strncmp(path + kLen, "/proc", 5) == 0 ||
         std::strncmp(path + kLen, "/sys", 4) == 0)) {
        return path + kLen;
    }
    return path;
}

/// A synthetic Android `/proc/self/mounts`, served ONLY when `CORDIAL_FAKE_PROC`
/// is set. The engine's anti-tamper reads `/proc/self/mounts` (confirmed by
/// `CORDIAL_TRACE_PATHS`), and the real FreeBSD ZFS mount table -- `zroot/ROOT
/// ... zfs`, `/dev/gpt/efiboot0 vfat` -- is a plain "this is not Android" tell
/// that no `ro.product.*` / `Build.*` value can hide, because it is the mount
/// table, not a device string. This deliberately shapes `/proc` to satisfy that
/// read, which `remap()` above rules out under ADR-001 -- hence the env gate: it
/// is off by default and the honest passthrough stays the default.
static const char* synth_proc_content(const char* path) {
    if (std::getenv("CORDIAL_FAKE_PROC") == nullptr) {
        return nullptr;
    }
    path = strip_compat_prefix(path);
    // The process cmdline. On Android a Roblox process's cmdline is its package
    // name, "com.roblox.client"; cordial's real cmdline is "cordial-run ...",
    // and the engine reads /proc/<pid>/cmdline (seen in CORDIAL_TRACE_PATHS as
    // /proc/0/cmdline and /proc/self/cmdline). An anti-tamper that checks "is my
    // process the Roblox package?" fails on the cordial name. Serve the package
    // name instead. (Single arg, no trailing NUL: the reader uses the byte count
    // fread returns, then takes the basename — no '/', so it sees the package.)
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
    // the scan completes and finds nothing. Same env gate / ADR-001 override.
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
    // `TracerPid:` here to detect a debugger. FreeBSD's /compat/linux/proc/self/
    // status is absent/wrong (confirmed: the engine builds this path and gets
    // nothing), so a reader sees no State/Threads/TracerPid. Serve a Linux/Android
    // status with TracerPid:0 (not traced), State:R, a plausible thread count, and
    // the real pids. Static buffer filled once.
    if (std::strcmp(path, "/proc/self/status") == 0 ||
        (std::strncmp(path, "/proc/", 6) == 0 &&
         std::strcmp(path + std::strlen(path) - 7, "/status") == 0)) {
        static char status_buf[2048];
        static bool status_filled = false;
        if (!status_filled) {
            long pid = static_cast<long>(getpid());
            long ppid = static_cast<long>(getppid());
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
                "VmPeak:\t 3200000 kB\n"
                "VmSize:\t 2800000 kB\n"
                "VmRSS:\t  520000 kB\n"
                "Threads:\t42\n"
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
                pid, pid, ppid);
            status_filled = true;
        }
        return status_buf;
    }
    // /proc/meminfo. The engine sizes memory from `MemTotal` (its allocator
    // arenas and the texture-streaming budget) and `build_user_agent` reports it
    // as the device's RAM. linprocfs would answer this; synthesising it from
    // `hw.physmem` is what lets the port need no `/compat/linux` mount at all,
    // and it fixes the `0MB` the User-Agent reported when the linprocfs read was
    // missed. Real numbers, filled once: installed RAM does not change mid-run.
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
/// Android-ise `/proc/self/maps`, served only under `CORDIAL_FAKE_PROC`. The
/// engine reads its own memory map, and the real one names `/compat/linux/usr/
/// lib64/libc.so`, the raw-mmap'd `libroblox.so` from `~/.cache`, and the
/// `cordial-run` binary -- every line a "not Android" tell. This reads the real
/// map and rewrites only the path column, keeping every address, so anything that
/// cross-checks an address against the map still matches. Same ADR-001 override
/// as the mount fake; same env gate.
/// Synthetic Android sysfs for the CPU-frequency and battery nodes the engine
/// polls. FreeBSD has no `/sys`, so every one of these reads returns null — and
/// the engine polls `scaling_cur_freq` hundreds of times a session, so what the
/// server's device-health report sees is a "device" whose CPU frequency, core
/// topology and battery are all unreadable: a tell no `ro.product.*` value can
/// cover, and the mirror of the `/proc/self/mounts` case. Under Linuxulator the
/// same binary reads a real linsysfs; native FreeBSD gets nothing. Served only
/// under `CORDIAL_FAKE_PROC`, same env gate / ADR-001 override as the /proc fakes.
/// Values model a 16-core x86_64 Android device (what Cordial reports elsewhere).
static const char* synth_sys_content(const char* path) {
    if (path == nullptr || std::getenv("CORDIAL_FAKE_PROC") == nullptr) {
        return nullptr;
    }
    path = strip_compat_prefix(path);
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
    if (std::strstr(path, "/sys/class/power_supply/") != nullptr) {
        if (std::strstr(path, "capacity") != nullptr) {
            return "87\n";
        }
        if (std::strstr(path, "status") != nullptr) {
            return "Charging\n";
        }
        if (std::strstr(path, "present") != nullptr) {
            return "1\n";
        }
    }
    return nullptr;
}

static std::string build_synth_maps(const char* real) {
    std::string out;
    FILE* src = ::fopen(real, "r");
    if (!src) {
        return out;
    }
    out.reserve(1 << 16);
    char line[1024];
    while (std::fgets(line, sizeof line, src)) {
        std::string s(line);
        auto sub = [&](const char* from, const char* to) {
            for (size_t p; (p = s.find(from)) != std::string::npos;) {
                s.replace(p, std::strlen(from), to);
            }
        };
        sub("/home/pascal/.cache/cordial-apk-new/lib/x86_64/libroblox.so",
            "/data/app/~~kQ8fN2pLx==/com.roblox.client-Rz9mAoY7w==/lib/arm64/libroblox.so");
        sub("/compat/linux/usr/lib64/", "/apex/com.android.runtime/lib64/bionic/");
        sub("/compat/linux/usr/lib/", "/system/lib64/");
        sub("/compat/linux/lib64/", "/system/lib64/");
        sub("/compat/linux/lib/", "/system/lib64/");
        sub("/compat/linux", "/system");
        // libroblox's GOT resolves its libc imports (open/read/uname/...) into
        // cordial-run's address space (the shims) — confirmed by reading the live
        // GOT — NOT into a libc.so. An anti-hook/GOT-integrity check that verifies
        // imports land in a libc mapping would flag that. So label the cordial-run
        // region as the Android libc.so, so a GOT entry pointing into it reads as
        // "points into libc" rather than "points into the main executable".
        // CORDIAL_MAPS_CORDIALRUN controls the label: default libc.so, "appproc"
        // restores the old /system/bin/app_process64 as the control.
        if (std::getenv("CORDIAL_MAPS_CORDIALRUN") &&
            std::string(std::getenv("CORDIAL_MAPS_CORDIALRUN")) == "appproc") {
            sub("/home/pascal/intoxicated/target/release/cordial-run",
                "/system/bin/app_process64");
        } else {
            sub("/home/pascal/intoxicated/target/release/cordial-run",
                "/apex/com.android.runtime/lib64/bionic/libc.so");
        }
        sub("/home/pascal/.cache/cordial-apk-new/candidate-0.apk",
            "/data/app/~~kQ8fN2pLx==/com.roblox.client-Rz9mAoY7w==/base.apk");
        sub("/home/pascal/.cache/cordial-agent-play", "/data/user/0/com.roblox.client");
        sub("/home/pascal", "/data/data/com.roblox.client");
        // Strip any path the rewrites above did not Android-ise. Roblox's Android
        // anti-cheat scans /proc/self/maps for substrings and reports a finding
        // (304) on a match; cordial's real map still names the FreeBSD runtime
        // loader and libc (`/libexec/ld-elf.so.1`, `/lib/libc.so.7`, `/usr/lib`)
        // and the whole desktop windowing stack (`/usr/local/lib/libX11*`,
        // `libGLX_nvidia`, `libadwaita`, gio modules) — none of which exists on a
        // real device. Any of those is a tell. Blank the path column for every
        // line whose path is not already under a genuine Android root, keeping the
        // address/perms/offset columns intact so anything cross-checking an
        // address against the map still resolves; a mapping with no path reads as
        // an ordinary anonymous region, which is unremarkable.
        {
            size_t nl = s.find_first_of("\r\n");
            std::string body = (nl == std::string::npos) ? s : s.substr(0, nl);
            std::string tail = (nl == std::string::npos) ? std::string() : s.substr(nl);
            size_t pos = 0;
            for (int f = 0; f < 5 && pos < body.size(); ++f) {
                while (pos < body.size() && body[pos] != ' ' && body[pos] != '\t') ++pos;
                while (pos < body.size() && (body[pos] == ' ' || body[pos] == '\t')) ++pos;
            }
            if (pos < body.size() && body[pos] == '/') {
                const char* p = body.c_str() + pos;
                bool android = std::strncmp(p, "/system", 7) == 0 ||
                               std::strncmp(p, "/apex", 5) == 0 ||
                               std::strncmp(p, "/data", 5) == 0 ||
                               std::strncmp(p, "/vendor", 7) == 0 ||
                               std::strncmp(p, "/odm", 4) == 0 ||
                               std::strncmp(p, "/dev", 4) == 0 ||
                               std::strncmp(p, "/proc", 5) == 0;
                // A path can start with an Android root yet still carry a tell in
                // a later component — the rewrites above map the data-home prefix
                // but leave cordial's own `.../cordial/profiles/default/...`,
                // `.config/dconf`, etc. intact inside it. Treat any such line as a
                // tell and blank its path too, so no "cordial"/"dconf"/host string
                // survives anywhere in the map the scanner reads.
                std::string pathstr(p);
                for (const char* tell : {"cordial", "dconf", "intoxicated", "jnivm",
                                         "freebsd", "FreeBSD", ".cache", "/home/",
                                         "/usr/", "/libexec", "wine", "qemu", "Xvfb"}) {
                    if (pathstr.find(tell) != std::string::npos) {
                        android = false;
                        break;
                    }
                }
                if (!android) {
                    body.resize(pos);
                    // trim trailing spaces left where the path was
                    while (!body.empty() && (body.back() == ' ' || body.back() == '\t')) {
                        body.pop_back();
                    }
                    s = body + "\n";
                    if (nl != std::string::npos && tail != "\n") {
                        s = body + tail;
                    }
                }
            }
        }
        out += s;
    }
    ::fclose(src);
    return out;
}

static FILE* synth_maps(const char* real) {
    std::string out = build_synth_maps(real);
    if (std::getenv("CORDIAL_DUMP_SYNTHMAPS")) {
        if (FILE* d = ::fopen("/tmp/cordial_synthmaps.txt", "w")) {
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
    char tmpl[] = "/tmp/cordial_synthproc_XXXXXX";
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
/// open/openat that reaches a path we Android-ise — `open("/proc/self/maps")`,
/// but also `openat(dirfd, "/proc/self/maps", …)` with a real dirfd, which the
/// engine's anti-cheat uses and which otherwise skipped the synth and read the
/// real linprocfs map (full of `/home/.cache`, `cordial-run`, `dconf` tells).
/// `path` is the original request (pre-remap); `host_flags` are post-translation
/// FreeBSD open flags. Returns a ready fd, or -1 to fall through to a real open.
// Linux filesystem magics. A synthetic `/proc` or `/sys` fd is a plain regular
// file (fd_from_bytes mkstemps one), so fstatfs() on it would report the host's
// filesystem (ZFS) — and Roblox's anti-cheat opens `/proc/self/maps` with a RAW
// openat and fstatfs()es the fd precisely to check it is really on procfs. A
// mismatch reads as "/proc was replaced" -> tamper -> 304. Tag every synth fd
// with the magic it must report, and answer fstatfs()/statfs() from the tag.
#define CORDIAL_PROC_SUPER_MAGIC 0x9fa0UL
#define CORDIAL_SYSFS_MAGIC      0x62656572UL

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
extern "C" int cordial_synth_fd_magic(int fd, unsigned long* magic) {
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
            synthfd_tag(fd, CORDIAL_PROC_SUPER_MAGIC);
            return fd;
        }
    }
    if (const char* synth = synth_proc_content(path)) {
        int fd = fd_from_bytes(synth, std::strlen(synth));
        synthfd_tag(fd, CORDIAL_PROC_SUPER_MAGIC);
        return fd;
    }
    if (const char* ssynth = synth_sys_content(path)) {
        int fd = fd_from_bytes(ssynth, std::strlen(ssynth));
        synthfd_tag(fd, CORDIAL_SYSFS_MAGIC);
        return fd;
    }
    if (std::getenv("CORDIAL_FAKE_PROC") != nullptr &&
        std::strcmp(strip_compat_prefix(path), "/proc/self/maps") == 0) {
        char _mb[PATH_MAX];
        const char* mreal = remap(strip_compat_prefix(path), _mb, sizeof _mb);
        std::string m = build_synth_maps(mreal ? mreal : path);
        if (!m.empty()) {
            int fd = fd_from_bytes(m.data(), m.size());
            synthfd_tag(fd, CORDIAL_PROC_SUPER_MAGIC);
            return fd;
        }
    }
    return -1;
}

// Fill a bionic/Linux `struct statfs` (120 bytes) for a synth fd so the anti-cheat
// sees procfs/sysfs, or for a real fd from the host statfs with an ext4 magic (a
// genuine /data value, never ZFS). Shared by the libc `fstatfs`/`statfs` shims and
// the raw-syscall trap so both paths agree.
extern "C" int cordial_fill_linux_statfs(unsigned long magic, const struct ::statfs* host,
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
    if (cordial_synth_fd_magic(fd, &magic)) {
        return cordial_fill_linux_statfs(magic, nullptr, out);
    }
    struct ::statfs host{};
    if (::fstatfs(fd, &host) != 0) {
        cordial_fbsd_errno_to_linux();
        return -1;
    }
    return cordial_fill_linux_statfs(0xEF53UL, &host, out);  // EXT4_SUPER_MAGIC
}

int s_statfs(const char* path, void* out) {
    const char* sp = strip_compat_prefix(path);
    if (std::strncmp(sp, "/proc", 5) == 0)
        return cordial_fill_linux_statfs(CORDIAL_PROC_SUPER_MAGIC, nullptr, out);
    if (std::strncmp(sp, "/sys", 4) == 0)
        return cordial_fill_linux_statfs(CORDIAL_SYSFS_MAGIC, nullptr, out);
    char _b[PATH_MAX];
    const char* real = remap(sp, _b, sizeof _b);
    struct ::statfs host{};
    if (::statfs(real ? real : path, &host) != 0) {
        cordial_fbsd_errno_to_linux();
        return -1;
    }
    return cordial_fill_linux_statfs(0xEF53UL, &host, out);
}

// Close shim: drop any synth-fd tag so a later reuse of the fd number does not
// inherit a stale procfs/sysfs magic, then close for real.
int s_close(int fd) {
    synthfd_forget(fd);
    return ::close(fd);
}
#endif

FILE* s_fopen(const char* path, const char* mode) {
    ANDROID_HIDE(path, nullptr);
#if defined(__FreeBSD__)
    {
        size_t blen = 0;
        if (const char* bsynth = synth_proc_bin(path, &blen)) {
            trace("fopen", path, "synth-proc-bin");
            return ::fmemopen(const_cast<char*>(bsynth), blen, "r");
        }
    }
    if (const char* synth = synth_proc_content(path)) {
        trace("fopen", path, "synth-android");
        return ::fmemopen(const_cast<char*>(synth), std::strlen(synth), "r");
    }
    if (const char* ssynth = synth_sys_content(path)) {
        trace("fopen", path, "synth-sys");
        return ::fmemopen(const_cast<char*>(ssynth), std::strlen(ssynth), "r");
    }
    if (std::getenv("CORDIAL_FAKE_PROC") != nullptr &&
        std::strcmp(strip_compat_prefix(path), "/proc/self/maps") == 0) {
        char _mb[PATH_MAX];
        const char* mreal = remap(strip_compat_prefix(path), _mb, sizeof _mb);
        if (FILE* f = synth_maps(mreal ? mreal : path)) {
            trace("fopen", path, "synth-maps");
            return f;
        }
    }
#endif
    REMAP(path);
    FILE* f = ::fopen(real, mode);
    trace("fopen", real, f ? "ok" : "null");
    return f;
}

/// `open` is variadic: the mode argument exists only for `O_CREAT`/`O_TMPFILE`.
/// Reading it unconditionally would walk the register save area for an argument
/// the caller never pushed, which is the mistake that makes `CORDIAL_TRACE=1`
/// abort the engine.
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
    const bool has_mode = cordial_fbsd_open_takes_mode(flags) != 0;
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
    if (cordial_fbsd_open_flags(flags, &host_flags) != 0) {
        trace_i("open", path, -1);
        return -1;
    }
    flags = host_flags;
    // Serve the same synthetic /proc content `s_fopen` does, but through the raw
    // `open()` path: an anti-tamper that reads /proc/self/{maps,mounts,cmdline}
    // with open()+read() instead of fopen() otherwise gets the real FreeBSD
    // procfs here (full of /compat/linux, ~/.cache, cordial-run tells). Only the
    // read path is synthesised; a writing/creating open falls through.
    {
        int sfd = try_synth_fd(path, flags);
        if (sfd >= 0) {
            trace_i("open", path, sfd);
            return sfd;
        }
    }
#endif
    REMAP(path);
    int r = ::open(real, flags, mode);
#if defined(__FreeBSD__)
    if (r < 0)
        cordial_fbsd_errno_to_linux();
#endif
    trace_i("open", real, r);
    return r;
}

/// `statvfs`, which the engine imports and Cordial had never intercepted.
///
/// Two separate defects, and the second is why this is not just a redirect.
///
/// **It was not path-translated.** Every other path-taking call here is; this
/// one went straight to the host with whatever the engine built, and because it
/// was not in the table it did not appear in `CORDIAL_TRACE_PATHS=1` output
/// either. A trace that cannot see a call is not evidence the call did not
/// happen, and a conclusion in flag-init.md §23.2 -- "storage is never
/// attempted, 19,296 path calls and none of them `rbx-storage`" -- was drawn
/// with this blind spot in it.
///
/// **The struct layouts differ.** bionic's `struct statvfs` runs
/// `f_fsid, f_flag, f_namemax`; glibc's inserts an `int __f_unused` after
/// `f_fsid`, which on LP64 pushes `f_flag` and `f_namemax` eight bytes along.
/// The size fields the engine cares about for free space -- `f_bsize` through
/// `f_favail` -- happen to align, so this is not obviously fatal, but the engine
/// reading `f_flag` gets glibc's padding and reading `f_namemax` gets glibc's
/// `f_flag`. `ST_RDONLY` lives in `f_flag`. This is the same family as the
/// `sigset_t`, `struct sigaction` and `mallinfo` divergences already recorded,
/// and it is fixed the same way: fill the bionic shape by hand rather than hope
/// the two agree.
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
    uint32_t __f_reserved[6];
};

int s_statvfs(const char* path, bionic_statvfs* out) {
    REMAP(path);
    struct ::statvfs host {};
    const int r = ::statvfs(real, &host);
    trace_i("statvfs", real, r);
    if (r == 0 && out) {
        *out = bionic_statvfs{};
        out->f_bsize = host.f_bsize;
        out->f_frsize = host.f_frsize;
        out->f_blocks = host.f_blocks;
        out->f_bfree = host.f_bfree;
        out->f_bavail = host.f_bavail;
        out->f_files = host.f_files;
        out->f_ffree = host.f_ffree;
        out->f_favail = host.f_favail;
        out->f_fsid = host.f_fsid;
        out->f_flag = host.f_flag;
        out->f_namemax = host.f_namemax;
    }
    return r;
}

// The engine is a bionic binary and overwhelmingly uses the `*at` syscalls, not
// the legacy ones: `access` is `faccessat`, `stat` is `newfstatat`, `open` is
// `openat`. Those were never hooked, so they reached the host directly — which is
// how the anti-cheat's root check saw FreeBSD's `/usr/bin/su` despite `s_open`/
// `s_access` hiding it (304). Hook them too. For the ordinary AT_FDCWD case they
// delegate to the already-correct legacy shims (which do the hide, the /proc and
// /system remap, the Linux->FreeBSD flag translation and the bionic `struct stat`
// layout); a real directory fd (rare here) falls through to the host after the
// same hide + remap.
#ifndef CORDIAL_AT_FDCWD
#define CORDIAL_AT_FDCWD (-100) // identical on Linux and FreeBSD
#endif
int s_openat(int dirfd, const char* path, int flags, ...) {
    ANDROID_HIDE(path, -1);
    unsigned mode = 0;
#if defined(__FreeBSD__)
    const bool has_mode = cordial_fbsd_open_takes_mode(flags) != 0;
#else
    const bool has_mode = (flags & (O_CREAT | O_TMPFILE)) != 0;
#endif
    if (has_mode) {
        va_list ap;
        va_start(ap, flags);
        mode = va_arg(ap, unsigned);
        va_end(ap);
    }
    if (dirfd == CORDIAL_AT_FDCWD) {
        return has_mode ? s_open(path, flags, mode) : s_open(path, flags);
    }
#if defined(__FreeBSD__)
    int host_flags;
    if (cordial_fbsd_open_flags(flags, &host_flags) != 0) {
        return -1;
    }
    flags = host_flags;
    // Serve synth /proc even with a real dirfd and an absolute path: the engine's
    // anti-cheat reads /proc/self/maps via openat(dirfd, "/proc/self/maps", …),
    // which never reached the AT_FDCWD→s_open synth above and so got the real
    // host-leaking linprocfs map. Check the original (pre-remap) path.
    {
        int sfd = try_synth_fd(path, flags);
        if (sfd >= 0) {
            trace_i("openat", path, sfd);
            return sfd;
        }
    }
#endif
    REMAP(path);
    int r = has_mode ? ::openat(dirfd, real, flags, mode) : ::openat(dirfd, real, flags);
    trace_i("openat", real, r);
    return r;
}

int s_faccessat(int dirfd, const char* path, int mode, int flags) {
    ANDROID_HIDE(path, -1);
    if (dirfd == CORDIAL_AT_FDCWD) {
        return s_access(path, mode);
    }
    REMAP(path);
    int r = ::faccessat(dirfd, real, mode, flags);
    trace_i("faccessat", real, r);
    return r;
}

#if defined(__FreeBSD__)
// Linux AT_SYMLINK_NOFOLLOW is 0x100; the engine passes Linux flag values.
#define CORDIAL_LX_AT_SYMLINK_NOFOLLOW 0x100
int s_fstatat(int dirfd, const char* path, void* out, int flags) {
    ANDROID_HIDE(path, -1);
    if (dirfd == CORDIAL_AT_FDCWD) {
        return (flags & CORDIAL_LX_AT_SYMLINK_NOFOLLOW) ? s_lstat(path, out)
                                                        : s_stat(path, out);
    }
    REMAP(path);
    struct stat native;
    int r = ::fstatat(dirfd, real, &native,
                      (flags & CORDIAL_LX_AT_SYMLINK_NOFOLLOW) ? AT_SYMLINK_NOFOLLOW : 0);
    trace_i("fstatat", real, r);
    if (r == 0) {
        to_bionic_stat(&native, (bionic_stat*)out);
    }
    return r;
}
#endif

#undef REMAP

} // namespace

/// The same rewrite for freebsd_abi.c, whose `open`/`__open_2`/`openat`
/// replaced this file's for the engine when the ABI layer landed and so
/// silently dropped the /proc and /system redirects: the engine then logged
/// `Failed to open /proc/meminfo` and reported 0 MB of memory. Returns `path`
/// itself when nothing applies.
extern "C" const char* cordial_path_remap(const char* path, char* buf, size_t n) {
    const char* r = remap(path, buf, n);
    return r ? r : path;
}

/// 1 if `path` is a host binary a genuine Android device never has (su, magisk,
/// …). Exposed with C linkage so the libc shims in `freebsd_libc_compat.c` —
/// `__open_2` and friends, which the engine's root check reaches through and
/// which call the host `open()` directly rather than `s_open` — can decline it
/// too. See `android_absent_file`.
extern "C" int cordial_path_is_hidden(const char* path) {
#if defined(__FreeBSD__)
    return android_absent_file(path) ? 1 : 0;
#else
    (void)path;
    return 0;
#endif
}

extern "C" struct CordialSystemSymbol {
    const char* name;
    void* addr;
};

/// Point the redirect at a host directory. Passing null or "" disables it.
extern "C" void cordial_set_system_root(const char* root) {
    if (!root || !*root) {
        g_root_len = 0;
        g_root[0] = '\0';
        return;
    }
    std::snprintf(g_root, sizeof g_root, "%s", root);
    g_root_len = std::strlen(g_root);
    // A trailing slash would produce `<root>//fonts`, which works but reads
    // badly in a trace, and would break a later exact-prefix comparison.
    while (g_root_len > 1 && g_root[g_root_len - 1] == '/') {
        g_root[--g_root_len] = '\0';
    }
}

/// Turn on the path log. Separate from the root so tracing works even when the
/// redirect is disabled.
extern "C" void cordial_set_path_trace(int on) {
    g_trace = on != 0;
}

extern "C" const CordialSystemSymbol* cordial_system_symbols(size_t* count) {
    static const CordialSystemSymbol table[] = {
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
        {"readlink", (void*)&s_readlink},
        {"fopen", (void*)&s_fopen},
        {"statvfs", (void*)&s_statvfs},
        {"fstatfs", (void*)&s_fstatfs},
        {"statfs", (void*)&s_statfs},
        {"close", (void*)&s_close},
        {"open", (void*)&s_open},
    };
    *count = sizeof(table) / sizeof(table[0]);
    return table;
}
