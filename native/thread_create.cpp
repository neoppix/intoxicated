// `pthread_create` for the engine: bionic's `pthread_attr_t` translated for
// FreeBSD, and an optional line per new thread saying who made it.
//
// The trace ties a thread id seen in a log to the code that spawned it.
// Gated behind `INTOXICATED_TRACE_THREADS=1`, matching `INTOXICATED_TRACE_PATHS` and
// `INTOXICATED_TRACE_PROPS`: off by default, and a plain `fprintf(stderr, ...)` per
// thread when on, printed by the new thread itself before its start routine
// runs. It is a switch of its own rather than part of the path trace because
// that one prints a line for every file call, many a second in game (the
// engine reads /proc/self/statm about 18 times a second), which a question
// about threads should not have to wade through. The line:
//
//   [threads] tid=<kernel thread id> spawned by caller=<A> start_routine=<B>
//
// A is the return address of the engine's pthread_create call and B the start
// routine, each as `libroblox.so+0x<offset>` when it lies inside the engine's
// mapping and `0x<address> (outside libroblox.so)` otherwise. The mapping is
// found in FreeBSD's own record of this process's address space
// (`kern.proc.vmmap`, the kinfo_vmentry list): every file-backed entry whose
// path ends in `/libroblox.so`, bounded below and above. The bionic linker maps
// the engine with mmap, so the host's dl_iterate_phdr does not know it, and
// FreeBSD has no /proc/self/maps.

#include <pthread.h>

#include "os_compat.h"
#include <climits>

#include <sys/types.h>
#include <sys/sysctl.h>
#include <sys/user.h>
#include <unistd.h>

#include <atomic>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <mutex>
#include <new>
#include <vector>

#include "symbol_table.h"

#if defined(__FreeBSD__)
#include <pthread_np.h>
#ifndef PTHREAD_STACK_MIN
#define PTHREAD_STACK_MIN (2 * 4096)
#endif
#endif

namespace {

std::atomic<bool> g_trace{false};

#if defined(__FreeBSD__)
// bionic's `pthread_attr_t` is a by-value 56-byte struct; FreeBSD's is an opaque
// pointer. On glibc both are by-value structs, so forwarding `attr` untouched is
// correct (and stays the default off-FreeBSD) -- but on FreeBSD a bionic attr
// handed to host pthread_create is dereferenced as a `struct pthread_attr *` and
// derefs its `flags` qword as a pointer -> SIGSEGV in _pthread_create. So on
// FreeBSD we read the bionic layout and build a real FreeBSD attr from it.
struct BionicPthreadAttr {
    uint32_t flags;          // 0x00  bit0 DETACHED, bit1 USER_ALLOCATED_STACK
    uint32_t _pad;           // 0x04
    void*    stack_base;     // 0x08
    size_t   stack_size;     // 0x10
    size_t   guard_size;     // 0x18
    int32_t  sched_policy;   // 0x20
    int32_t  sched_priority; // 0x24
    char     __reserved[16]; // 0x28
};

// Fill `out` (an initialised FreeBSD attr) from a bionic attr. Returns true if a
// translation was built (caller must pthread_attr_destroy(out)); false when
// `battr` is NULL and the caller should just pass NULL through.
bool bionic_attr_to_freebsd(const pthread_attr_t* battr, pthread_attr_t* out) {
    if (!battr) return false;
    const BionicPthreadAttr* b = reinterpret_cast<const BionicPthreadAttr*>(battr);
    pthread_attr_init(out);
    size_t ss = b->stack_size;
    if (ss != 0 && ss < (size_t)PTHREAD_STACK_MIN) ss = (size_t)PTHREAD_STACK_MIN;
    // Only trust a caller-provided stack base when the flag says it owns one;
    // otherwise stack_base may be stale (e.g. copied from getattr_np) and using
    // it as a real stack would fault. Common case: caller sets size only.
    if ((b->flags & 2u) && b->stack_base) {
        pthread_attr_setstack(out, b->stack_base, ss);
    } else if (ss != 0) {
        pthread_attr_setstacksize(out, ss);
    }
    if (b->guard_size) pthread_attr_setguardsize(out, b->guard_size);
    if (b->flags & 1u) pthread_attr_setdetachstate(out, PTHREAD_CREATE_DETACHED);
    return true;
}
#endif

/// Where libroblox.so is mapped: [lo, hi), and the address of file offset 0.
struct EngineRange {
    uintptr_t lo = 0;
    uintptr_t hi = 0;
    uintptr_t base = 0;
};

bool ends_with_engine(const char* path) {
    static const char kName[] = "/libroblox.so";
    const size_t n = std::strlen(path);
    const size_t k = sizeof kName - 1;
    return n >= k && std::memcmp(path + n - k, kName, k) == 0;
}

/// The engine's mapping, from `kern.proc.vmmap`; false when it is not mapped.
bool scan_vmmap(EngineRange* out) {
    int mib[4] = {CTL_KERN, KERN_PROC, KERN_PROC_VMMAP, static_cast<int>(getpid())};
    size_t len = 0;
    if (sysctl(mib, 4, nullptr, &len, nullptr, 0) != 0 || len == 0) {
        return false;
    }
    len += len / 3; // the map can grow between the two calls
    std::vector<char> buf(len);
    if (sysctl(mib, 4, buf.data(), &len, nullptr, 0) != 0) {
        return false;
    }
    bool found = false;
    uintptr_t lowest_offset = 0;
    for (size_t off = 0; off + offsetof(struct kinfo_vmentry, kve_path) <= len;) {
        const auto* kve = reinterpret_cast<const struct kinfo_vmentry*>(buf.data() + off);
        if (kve->kve_structsize <= 0) {
            break;
        }
        off += static_cast<size_t>(kve->kve_structsize);
        if (kve->kve_type != KVME_TYPE_VNODE || kve->kve_path[0] == '\0' || !ends_with_engine(kve->kve_path)) {
            continue;
        }
        const auto start = static_cast<uintptr_t>(kve->kve_start);
        const auto end = static_cast<uintptr_t>(kve->kve_end);
        if (!found || start < out->lo) {
            out->lo = start;
            lowest_offset = static_cast<uintptr_t>(kve->kve_offset);
        }
        if (!found || end > out->hi) {
            out->hi = end;
        }
        found = true;
    }
    if (found) {
        out->base = out->lo - lowest_offset;
    }
    return found;
}

/// The mapping, looked up once it exists and kept: the engine never moves.
/// Before it is mapped (a host thread) every lookup tries again.
std::mutex g_range_lock;
bool g_range_known = false;
EngineRange g_range;

bool engine_range(EngineRange* out) {
    std::lock_guard<std::mutex> lock(g_range_lock);
    if (!g_range_known) {
        g_range_known = scan_vmmap(&g_range);
    }
    if (g_range_known) {
        *out = g_range;
    }
    return g_range_known;
}

void describe(uintptr_t address, const EngineRange* range, char* out, size_t n) {
    if (range != nullptr && address >= range->lo && address < range->hi) {
        std::snprintf(out, n, "libroblox.so+0x%lx", static_cast<unsigned long>(address - range->base));
    } else {
        std::snprintf(out, n, "0x%lx (outside libroblox.so)", static_cast<unsigned long>(address));
    }
}

struct ThreadTraceCtx {
    void* (*start_routine)(void*);
    void* arg;
    uintptr_t caller;
    uintptr_t start;
};

void* trampoline(void* p) {
    const ThreadTraceCtx ctx = *static_cast<ThreadTraceCtx*>(p);
    delete static_cast<ThreadTraceCtx*>(p);
    long tid = intoxicated_gettid();
    EngineRange range;
    const bool mapped = engine_range(&range);
    char caller[64];
    char start[64];
    describe(ctx.caller, mapped ? &range : nullptr, caller, sizeof caller);
    describe(ctx.start, mapped ? &range : nullptr, start, sizeof start);
    std::fprintf(stderr, "[threads] tid=%ld spawned by caller=%s start_routine=%s\n", tid, caller, start);
    return ctx.start_routine(ctx.arg);
}

} // namespace

extern "C" {

/// The engine's `pthread_create`.
int intoxicated_pthread_create(pthread_t* thread, const pthread_attr_t* attr, void* (*start_routine)(void*),
                               void* arg) {
    const pthread_attr_t* eff = attr;
#if defined(__FreeBSD__)
    pthread_attr_t fa;
    bool xlated = bionic_attr_to_freebsd(attr, &fa);
    if (xlated) eff = &fa;
#endif
    int rc;
    ThreadTraceCtx* ctx = nullptr;
    if (!g_trace.load(std::memory_order_relaxed) ||
        (ctx = new (std::nothrow) ThreadTraceCtx{start_routine, arg, 0, 0}) == nullptr) {
        rc = ::pthread_create(thread, eff, start_routine, arg);
    } else {
        // `__builtin_return_address(0)` reads the address `call` pushed for this
        // frame -- a compiler-known fixed slot, not a walk of the frame-pointer
        // chain, so it is exact regardless of `-fomit-frame-pointer` and does
        // not run into the "no frame pointers" caveat that makes anything past
        // the innermost frame guesswork elsewhere in this codebase.
        void* caller = __builtin_return_address(0);
        ctx->caller = (uintptr_t)caller;
        ctx->start = (uintptr_t)start_routine;
        rc = ::pthread_create(thread, eff, trampoline, ctx);
        if (rc != 0) {
            // The thread never started, so nothing will reach the `delete`
            // inside `trampoline`.
            delete ctx;
        }
    }
#if defined(__FreeBSD__)
    if (xlated) pthread_attr_destroy(&fa);
#endif
    return rc;
}

/// Turn on the thread-creation log. `INTOXICATED_TRACE_THREADS=1` -- see the file
/// comment for why this is a separate flag from `INTOXICATED_TRACE_PATHS` rather
/// than part of it. Non-zero on, zero off; bionic/mod.rs calls it before the
/// engine runs.
void intoxicated_set_thread_trace(int on) {
    g_trace.store(on != 0, std::memory_order_relaxed);
}

} // extern "C"

namespace {

const IntoxicatedSymbol kThreads[] = {
    {"pthread_create", reinterpret_cast<void*>(&intoxicated_pthread_create)},
};

} // namespace

extern "C" const IntoxicatedSymbol* intoxicated_thread_symbols(size_t* count) {
    if (count != nullptr) {
        *count = intoxicated_table_size(kThreads);
    }
    return kThreads;
}
