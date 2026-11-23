// syscall_trap.c: neutralise libroblox's raw `syscall` instructions on FreeBSD.
//
// Roblox's Android anti-cheat (Hyperion) does not reach the kernel only through
// libc: `libroblox.so` carries raw `syscall` (0f 05) instructions in its own
// .text: 40 of them in 2.738.0.1397, 16 in 2.748.1527. Seven sit in the engine's
// `initializeNativeCode` (a plain `mov $0xba,%eax; syscall` = gettid); the rest
// (33, then nine) are an obfuscated gateway that loads the syscall number from a computed
// stack slot (`movq -0xe18(%rbp),%rax; syscall`, wrapped in movabs/xor/imul/rol)
// specifically so a static reader cannot tell which call it is -- the textbook
// shape of an anti-hook direct-syscall layer.
//
// On Linux (where Sober runs) those raw syscalls reach the Linux kernel and
// return Linux results, matching what the (hooked) libc returns -- so Hyperion's
// "does the raw syscall agree with libc" check passes. On native FreeBSD a raw
// `syscall` from this process uses the FreeBSD syscall table, so a Linux number
// executes the WRONG FreeBSD syscall (Linux getpid=39 -> FreeBSD getppid; many
// -> ENOSYS). The results disagree with the runtime's libc shims, Hyperion reads
// that as "libc is hooked / the client is tampered", reports an untrusted
// status, and the server disconnects with reason 304 ("Roblox has detected
// missing or corrupted files") ~60s into the session. This is invisible to
// INTOXICATED_TRACE_SYSCALL, which only sees the libc `syscall()` symbol, never a
// raw `syscall` instruction.
//
// Fix: at load, overwrite each raw `syscall` (0f 05) with `ud2` (0f 0b) -- same
// two bytes -- and catch the resulting SIGILL. The handler reads the Linux
// syscall number and arguments straight out of the trap frame (so the obfuscated
// dynamic numbers are handled too, since we see the value the register actually
// holds) and routes them through `bionic_syscall`, the SAME Linux->FreeBSD
// translator the engine's libc `syscall()` symbol already uses. Raw and libc now
// return identical results, which is exactly what defeats the comparison. The
// patch runs while the linker's text mapping is still writable (before the
// `INTOXICATED_RX_TEXT` downgrade), and only rewrites addresses that still read as
// `0f 05`, reporting any that do not.
//
// `INTOXICATED_SYSCALL_TRAP=off` disables the whole thing (control). The sites are
// not listed here: they move with every Roblox build, so the loader finds them
// in the library file at load (crates/intoxicated-runtime/src/syscall_sites.rs) and
// passes the list in.
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

static struct sigaction g_prev_sigill;
static unsigned char g_seen[512];

static void intoxicated_raw_syscall_handler(int sig, siginfo_t* si, void* ucv) {
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

    if (getenv("INTOXICATED_TRACE_RAWSYS")) {
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
// the library's load base (lib.base() on the Rust side); `vaddrs` holds `count`
// site virtual addresses, which for this library (first PT_LOAD at vaddr 0) are
// offsets from `base`. Each is verified to still read `0f 05` before it is
// touched. Must be called while the text mapping is still writable. Returns the
// number of sites patched, or -1 if the handler could not be installed. No-op
// (returns 0) under INTOXICATED_SYSCALL_TRAP=off.
int intoxicated_install_raw_syscall_trap(unsigned long base, const unsigned long* vaddrs,
                                     size_t count) {
    const char* off = getenv("INTOXICATED_SYSCALL_TRAP");
    if (off && strcmp(off, "off") == 0) {
        fprintf(stderr, "[rawsys] disabled (INTOXICATED_SYSCALL_TRAP=off)\n");
        return 0;
    }

    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = intoxicated_raw_syscall_handler;
    sa.sa_flags = SA_SIGINFO | SA_NODEFER;
    sigemptyset(&sa.sa_mask);
    if (sigaction(SIGILL, &sa, &g_prev_sigill) != 0) {
        fprintf(stderr, "[rawsys] sigaction(SIGILL) failed: %s\n", strerror(errno));
        return -1;
    }

    int patched = 0, mismatch = 0;
    for (size_t i = 0; i < count; i++) {
        unsigned char* p = (unsigned char*)(uintptr_t)(base + vaddrs[i]);
        if (p[0] == 0x0f && p[1] == 0x05) {
            p[1] = 0x0b;  // 0f 05 (syscall) -> 0f 0b (ud2)
            patched++;
        } else if (p[0] == 0x0f && p[1] == 0x0b) {
            patched++;  // already patched (double call)
        } else {
            mismatch++;
            if (getenv("INTOXICATED_TRACE_RAWSYS")) {
                fprintf(stderr,
                        "[rawsys] site %zu @%#lx reads %02x %02x, not a syscall; skipped\n",
                        i, base + vaddrs[i], p[0], p[1]);
            }
        }
    }
    fprintf(stderr,
            "[rawsys] neutralised %d/%zu raw syscall sites (base=%#lx%s)\n",
            patched, count, base,
            mismatch ? ", some sites did not read 0f 05 and were skipped" : "");
    return patched;
}

#endif /* __FreeBSD__ */
