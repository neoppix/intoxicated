// The bionic linker (mcpelauncher-linker, `public_include/mcpelauncher/linker.h`)
// under C names the Rust side can call: `crates/intoxicated-linker-sys/src/ffi.rs`
// declares them and `src/lib.rs` wraps them. The linker's own interface takes
// a C++ map, which Rust cannot build, so this file turns parallel arrays into
// one and otherwise forwards. It keeps no state of its own beyond the dlopen
// trace switch.
//
// No C++ exception leaves these functions: the callers are Rust frames, which
// cannot be unwound through. A refusal is a null handle, which the Rust side
// reports with the linker's dlerror().

#include <mcpelauncher/linker.h>

#include <cstring>
#include <cstddef>
#include <cstdio>
#include <cstdlib>
#include <ctime>
#include <string>
#include <unordered_map>

#include "symbol_table.h"

namespace {

/// `INTOXICATED_TRACE_DLOPEN`, present at all: read on the first dlopen and
/// kept for the process.
bool dlopen_traced() {
    static const bool on = std::getenv("INTOXICATED_TRACE_DLOPEN") != nullptr;
    return on;
}

long long monotonic_us() {
    struct timespec ts {};
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return static_cast<long long>(ts.tv_sec) * 1000000LL + ts.tv_nsec / 1000;
}

} // namespace

// The runtime's hooked libc shims (s_open/s_openat/s_access/... with the root-file
// hiding, /proc redirect and bionic struct translation) are installed into the
// engine's import table. But the engine also resolves symbols dynamically via
// dlsym, and Roblox's Android anti-cheat uses that to fetch the *raw* host libc
// (e.g. `dlsym(handle, "openat")`) specifically to step around import hooks -- and
// then opened FreeBSD's /usr/bin/su through it, tripping the 304 root check. Hand
// dlsym the same shim the import table got, so there is one consistent, hooked
// view of these calls however they are reached.
extern "C" const IntoxicatedSymbol* intoxicated_system_symbols(size_t* count);

extern "C" {

/// `linker::init()`: bionic's library list and its built-in libdl.so. Once,
/// before any other call here.
void intoxicated_linker_init(void) {
    try {
        linker::init();
    } catch (...) {
        std::fprintf(stderr, "[intoxicated] linker::init threw; the linker is unusable\n");
    }
}

/// A virtual library named `soname` whose whole content is the `n` pairs
/// (`names[i]`, `addrs[i]`). The pairs are copied; the soname pointer is kept
/// by the linker, which is why the Rust side leaks it. A name given twice
/// keeps its first address, as map insertion does. Null on failure.
void* intoxicated_linker_load_library(const char* soname, const char* const* names, void* const* addrs,
                                      size_t n) {
    if (soname == nullptr || (n > 0 && (names == nullptr || addrs == nullptr))) {
        return nullptr;
    }
    try {
        std::unordered_map<std::string, void*> symbols;
        symbols.reserve(n);
        for (size_t i = 0; i < n; ++i) {
            if (names[i] != nullptr) {
                symbols.emplace(names[i], addrs[i]);
            }
        }
        return linker::load_library(soname, symbols);
    } catch (...) {
        return nullptr;
    }
}

void intoxicated_linker_update_ld_library_path(const char* path) {
    linker::update_LD_LIBRARY_PATH(path);
}

/// `linker::dlopen`, with one stderr line per request when the trace is on:
/// the name, whether a handle came back, and how long the call took.
void* intoxicated_linker_dlopen(const char* filename, int flags) {
    if (!dlopen_traced()) {
        return linker::dlopen(filename, flags);
    }
    const long long start = monotonic_us();
    void* handle = linker::dlopen(filename, flags);
    const long long took = monotonic_us() - start;
    fprintf(stderr, "[intoxicated] dlopen(%s) -> %s in %lldus\n", filename ? filename : "(null)",
            handle ? "ok" : "NULL", took);
    return handle;
}

/// A name in `intoxicated_system_symbols` answers with that table's address
/// whatever `handle` is (see above); anything else is the linker's dlsym.
void* intoxicated_linker_dlsym(void* handle, const char* symbol) {
    if (symbol) {
        size_t n = 0;
        const IntoxicatedSymbol* tab = intoxicated_system_symbols(&n);
        for (size_t i = 0; i < n; ++i) {
            if (std::strcmp(tab[i].name, symbol) == 0) {
                return tab[i].addr;
            }
        }
    }
    return linker::dlsym(handle, symbol);
}

const char* intoxicated_linker_dlerror(void) {
    return linker::dlerror();
}

size_t intoxicated_linker_get_library_base(void* handle) {
    return linker::get_library_base(handle);
}

void intoxicated_linker_get_library_code_region(void* handle, size_t* base, size_t* size) {
    size_t b = 0;
    size_t s = 0;
    linker::get_library_code_region(handle, b, s);
    if (base) {
        *base = b;
    }
    if (size) {
        *size = s;
    }
}

} // extern "C"
