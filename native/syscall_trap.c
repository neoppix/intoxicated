// syscall_trap.c — neutralise libroblox's raw `syscall` instructions on FreeBSD.
//
// Roblox's Android anti-cheat (Hyperion) does not reach the kernel only through
// libc: `libroblox.so` carries raw `syscall` (0f 05) instructions in its own
// .text — 40 of them in this build (2.738.0.1397). Seven sit in the engine's
// `initializeNativeCode` (a plain `mov $0xba,%eax; syscall` = gettid); the other
// 33 are an obfuscated gateway that loads the syscall number from a computed
// stack slot (`movq -0xe18(%rbp),%rax; syscall`, wrapped in movabs/xor/imul/rol)
// specifically so a static reader cannot tell which call it is — the textbook
// shape of an anti-hook direct-syscall layer.
//
// On Linux (where Sober runs) those raw syscalls reach the Linux kernel and
// return Linux results, matching what the (hooked) libc returns — so Hyperion's
// "does the raw syscall agree with libc" check passes. On native FreeBSD a raw
// `syscall` from this process uses the FreeBSD syscall table, so a Linux number
// executes the WRONG FreeBSD syscall (Linux getpid=39 -> FreeBSD getppid; many
// -> ENOSYS). The results disagree with cordial's libc shims, Hyperion reads
// that as "libc is hooked / the client is tampered", reports an untrusted
// status, and the server disconnects with reason 304 ("Roblox has detected
// missing or corrupted files") ~60s into the session. This is invisible to
// CORDIAL_TRACE_SYSCALL, which only sees the libc `syscall()` symbol, never a
// raw `syscall` instruction.
//
// Fix: at load, overwrite each raw `syscall` (0f 05) with `ud2` (0f 0b) — same
// two bytes — and catch the resulting SIGILL. The handler reads the Linux
// syscall number and arguments straight out of the trap frame (so the obfuscated
// dynamic numbers are handled too, since we see the value the register actually
// holds) and routes them through `bionic_syscall`, the SAME Linux->FreeBSD
// translator the engine's libc `syscall()` symbol already uses. Raw and libc now
// return identical results, which is exactly what defeats the comparison. The
// patch runs while the linker's text mapping is still writable (before the
// `CORDIAL_RX_TEXT` downgrade), and only rewrites addresses that still read as
// `0f 05`, reporting any that do not.
//
// `CORDIAL_SYSCALL_TRAP=off` disables the whole thing (control). The address
// table is specific to this libroblox build; a different build needs it
// regenerated (tools/find_raw_syscalls, from llvm-objdump -d).
#if defined(__FreeBSD__)

#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <ucontext.h>

// The libc `syscall()` the engine imports: a Linux-number -> FreeBSD translator.
// Returns libc convention (value, or -1 with errno set to a *Linux* errno).
extern long bionic_syscall(long number, ...);

// Raw-syscall instruction virtual addresses in libroblox.so 2.738.0.1397
// (build-id 5f0704edd9064f566ee3d6df2bd2fabbcc709f03), from
//   llvm-objdump -d --section=.text libroblox.so | grep '0f 05  syscall'
// Virtual addresses == runtime offset from the load base (first PT_LOAD at
// vaddr 0). Each is verified to still read `0f 05` before it is touched.
static const unsigned long CORDIAL_RAW_SYSCALL_VADDRS[] = {
    0x29e140bUL, 0x29e142eUL, 0x29e15adUL, 0x29e15fcUL,
    0x29e1626UL, 0x29e16abUL, 0x29e1b39UL, 0x35963feUL,
    0x359a77dUL, 0x359ae48UL, 0x359bf53UL, 0x359e8baUL,
    0x35a0054UL, 0x35a2357UL, 0x35a4c2aUL, 0x35a6cbcUL,
    0x35ac87dUL, 0x35ac8f7UL, 0x35adee4UL, 0x35ae6deUL,
    0x35af0b9UL, 0x35b2114UL, 0x35b228aUL, 0x35b2e16UL,
    0x35b3b76UL, 0x35b4020UL, 0x35b5498UL, 0x35b765dUL,
    0x35b7e2dUL, 0x35b9fceUL, 0x35bd066UL, 0x35c429cUL,
    0x35c6221UL, 0x35c6873UL, 0x35c92c6UL, 0x35c9b9dUL,
    0x35cb2dbUL, 0x35cda69UL, 0x35cfafcUL, 0x35d0f1eUL,
};
#define CORDIAL_RAW_SYSCALL_COUNT \
    (sizeof(CORDIAL_RAW_SYSCALL_VADDRS) / sizeof(CORDIAL_RAW_SYSCALL_VADDRS[0]))

static struct sigaction g_prev_sigill;
static unsigned char g_seen[512];

static void cordial_raw_syscall_handler(int sig, siginfo_t* si, void* ucv) {
    ucontext_t* uc = (ucontext_t*)ucv;
    mcontext_t* m = &uc->uc_mcontext;
    const unsigned char* pc = (const unsigned char*)(uintptr_t)m->mc_rip;

    // Only our patched `ud2` (0f 0b) is ours to emulate. A genuine #UD anywhere
    // else is a real fault: chain to whatever handler was installed before us
    // (default = terminate) rather than swallow it.
    if (!pc || pc[0] != 0x0f || pc[1] != 0x0b) {
        if (g_prev_sigill.sa_flags & SA_SIGINFO) {
            if (g_prev_sigill.sa_sigaction) {
                g_prev_sigill.sa_sigaction(sig, si, ucv);
                return;
            }
        } else if (g_prev_sigill.sa_handler != SIG_IGN &&
                   g_prev_sigill.sa_handler != SIG_DFL &&
                   g_prev_sigill.sa_handler) {
            g_prev_sigill.sa_handler(sig);
            return;
        }
        // No usable previous handler: restore default and let it re-fault.
        signal(SIGILL, SIG_DFL);
        return;
    }

    long nr = (long)m->mc_rax;
    // Linux x86-64 syscall ABI: args in rdi, rsi, rdx, r10, r8, r9.
    long a0 = (long)m->mc_rdi, a1 = (long)m->mc_rsi, a2 = (long)m->mc_rdx;
    long a3 = (long)m->mc_r10, a4 = (long)m->mc_r8, a5 = (long)m->mc_r9;

    errno = 0;
    long r = bionic_syscall(nr, a0, a1, a2, a3, a4, a5);
    // Raw `syscall` returns kernel convention: a negative -errno on failure, not
    // libc's -1/errno. bionic_syscall already set a *Linux* errno on failure.
    long kret = (r == -1) ? -(long)errno : r;

    if (getenv("CORDIAL_TRACE_RAWSYS")) {
        // openat(257): dirfd in rdi, path in rsi. open(2): path in rdi.
        const char* path = NULL;
        if (nr == 257) path = (const char*)a1;
        else if (nr == 2) path = (const char*)a0;
        if (path) {
            fprintf(stderr, "[rawsys] nr=%ld path=\"%s\" -> %ld\n", nr, path, kret);
        } else if (nr >= 0 && nr < 512) {
            if (!g_seen[nr]) {
                g_seen[nr] = 1;
                fprintf(stderr, "[rawsys] first Linux nr=%ld -> %ld\n", nr, kret);
            }
        } else {
            fprintf(stderr, "[rawsys] Linux nr=%ld -> %ld\n", nr, kret);
        }
    }

    m->mc_rax = (uint64_t)kret;
    m->mc_rip += 2;  // step past the 2-byte ud2
}

// Patch libroblox's raw `syscall` sites to `ud2` and install the trap. `base` is
// the library's load base (lib.base() on the Rust side). Must be called while
// the text mapping is still writable. Returns the number of sites patched, or
// -1 if the handler could not be installed. No-op (returns 0) under
// CORDIAL_SYSCALL_TRAP=off.
int cordial_install_raw_syscall_trap(unsigned long base) {
    const char* off = getenv("CORDIAL_SYSCALL_TRAP");
    if (off && strcmp(off, "off") == 0) {
        fprintf(stderr, "[rawsys] disabled (CORDIAL_SYSCALL_TRAP=off)\n");
        return 0;
    }

    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = cordial_raw_syscall_handler;
    sa.sa_flags = SA_SIGINFO | SA_NODEFER;
    sigemptyset(&sa.sa_mask);
    if (sigaction(SIGILL, &sa, &g_prev_sigill) != 0) {
        fprintf(stderr, "[rawsys] sigaction(SIGILL) failed: %s\n", strerror(errno));
        return -1;
    }

    int patched = 0, mismatch = 0;
    for (size_t i = 0; i < CORDIAL_RAW_SYSCALL_COUNT; i++) {
        unsigned char* p = (unsigned char*)(uintptr_t)(base + CORDIAL_RAW_SYSCALL_VADDRS[i]);
        if (p[0] == 0x0f && p[1] == 0x05) {
            p[1] = 0x0b;  // 0f 05 (syscall) -> 0f 0b (ud2)
            patched++;
        } else if (p[0] == 0x0f && p[1] == 0x0b) {
            patched++;  // already patched (double call)
        } else {
            mismatch++;
            if (getenv("CORDIAL_TRACE_RAWSYS")) {
                fprintf(stderr,
                        "[rawsys] site %zu @%#lx reads %02x %02x, not a syscall; skipped\n",
                        i, base + CORDIAL_RAW_SYSCALL_VADDRS[i], p[0], p[1]);
            }
        }
    }
    fprintf(stderr,
            "[rawsys] neutralised %d/%zu raw syscall sites (base=%#lx%s)\n",
            patched, CORDIAL_RAW_SYSCALL_COUNT, base,
            mismatch ? ", some sites did not match — table may be stale for this build" : "");
    return patched;
}

#endif /* __FreeBSD__ */
