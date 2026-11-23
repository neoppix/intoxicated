// Android's liblog for the engine, written to the host's stderr. The engine
// narrates its start-up through it.
//
// Reached only through `intoxicated_liblog_symbols` (installed by
// `crates/intoxicated-runtime/src/bionic/mod.rs`) and the virtual liblog.so built
// from it. The functions themselves have internal linkage on purpose:
// liblinker.a already defines all six names globally (bionic's own liblog,
// for the linker), and exporting them again here would be a duplicate at the
// final link.
//
// No Rust code parses these lines; the engine's own log file is a different
// channel (`game_log`, `engine_log.rs`).
//
// Line shape, one fprintf per message:
//   HH:MM:SS.mmm L/<tag, left-justified in 24 columns> <text>
// with local time of day, L one of V D I W E F S for priorities 2 to 8 and ?
// for anything else. A line is printed when its priority is at least the
// minimum, which `INTOXICATED_LOG_LEVEL` sets by its first letter (v d i w e,
// either case; anything else INFO) and which is DEBUG when the variable is
// unset. Priorities above SILENT are never filtered.

#include <cctype>
#include <cstdarg>
#include <cstddef>
#include <cstdio>
#include <cstdlib>
#include <ctime>

#include <atomic>

#include "symbol_table.h"

namespace {

constexpr int kVerbose = 2;
constexpr int kDebug = 3;
constexpr int kInfo = 4;
constexpr int kWarn = 5;
constexpr int kError = 6;

/// A formatted message (print, vprint, assert) is cut at 4095 bytes.
constexpr size_t kFormatted = 4096;

std::atomic<int> g_minimum{-1};

int level_of(const char* value) {
    if (value == nullptr) {
        return kDebug;
    }
    switch (std::tolower(static_cast<unsigned char>(value[0]))) {
    case 'v':
        return kVerbose;
    case 'd':
        return kDebug;
    case 'i':
        return kInfo;
    case 'w':
        return kWarn;
    case 'e':
        return kError;
    default:
        return kInfo;
    }
}

/// The minimum priority, read on the first log call and kept.
int minimum() {
    int m = g_minimum.load(std::memory_order_relaxed);
    if (m < 0) {
        m = level_of(std::getenv("INTOXICATED_LOG_LEVEL"));
        g_minimum.store(m, std::memory_order_relaxed);
    }
    return m;
}

bool shown(int prio) {
    return prio >= minimum();
}

char letter(int prio) {
    static const char letters[] = "VDIWEFS";
    return prio >= 2 && prio <= 8 ? letters[prio - 2] : '?';
}

/// `HH:MM:SS.mmm`, or `??:??:??.???` when local time cannot be computed.
void stamp(char (&out)[16]) {
    struct timespec now {};
    struct tm local {};
    if (clock_gettime(CLOCK_REALTIME, &now) != 0 || localtime_r(&now.tv_sec, &local) == nullptr) {
        snprintf(out, sizeof out, "??:??:??.???");
        return;
    }
    snprintf(out, sizeof out, "%02d:%02d:%02d.%03ld", local.tm_hour, local.tm_min, local.tm_sec,
             static_cast<long>(now.tv_nsec / 1000000));
}

void emit(int prio, const char* tag, const char* text) {
    char when[16];
    stamp(when);
    fprintf(stderr, "%s %c/%-24s %s\n", when, letter(prio), tag != nullptr ? tag : "(no tag)",
            text != nullptr ? text : "(null)");
}

int log_write(int prio, const char* tag, const char* text) {
    if (shown(prio)) {
        emit(prio, tag, text);
    }
    return 1;
}

/// The buffer id (main, radio, events, system, crash) changes nothing here.
int log_buf_write(int buffer, int prio, const char* tag, const char* text) {
    (void)buffer;
    return log_write(prio, tag, text);
}

int log_vprint(int prio, const char* tag, const char* fmt, va_list ap) {
    if (!shown(prio)) {
        return 1;
    }
    if (fmt == nullptr) {
        emit(prio, tag, nullptr);
        return 1;
    }
    char text[kFormatted];
    vsnprintf(text, sizeof text, fmt, ap);
    emit(prio, tag, text);
    return 1;
}

int log_print(int prio, const char* tag, const char* fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    const int r = log_vprint(prio, tag, fmt, ap);
    va_end(ap);
    return r;
}

/// Printed whatever the level, then abort(). The runtime's always-on
/// diagnostic in `bionic/trace.rs` replaces this entry in the final table, so
/// this one is reached only if that override goes away.
[[noreturn]] void log_assert(const char* cond, const char* tag, const char* fmt, ...) {
    char message[kFormatted];
    if (fmt != nullptr) {
        va_list ap;
        va_start(ap, fmt);
        vsnprintf(message, sizeof message, fmt, ap);
        va_end(ap);
    } else {
        snprintf(message, sizeof message, "assertion failed: %s", cond != nullptr ? cond : "(unknown)");
    }
    fprintf(stderr,
            "\n*** Roblox fatal assertion ***\n"
            "    tag:       %s\n"
            "    condition: %s\n"
            "    message:   %s\n",
            tag != nullptr ? tag : "(no tag)", cond != nullptr ? cond : "(none)", message);
    abort();
}

/// Printed, not kept: nothing here reads it back.
void set_abort_message(const char* msg) {
    fprintf(stderr, "[abort-message] %s\n", msg != nullptr ? msg : "(null)");
}

const IntoxicatedSymbol kLiblog[] = {
    {"__android_log_write", reinterpret_cast<void*>(&log_write)},
    {"__android_log_buf_write", reinterpret_cast<void*>(&log_buf_write)},
    {"__android_log_print", reinterpret_cast<void*>(&log_print)},
    {"__android_log_vprint", reinterpret_cast<void*>(&log_vprint)},
    {"__android_log_assert", reinterpret_cast<void*>(&log_assert)},
    {"android_set_abort_message", reinterpret_cast<void*>(&set_abort_message)},
};

} // namespace

extern "C" const IntoxicatedSymbol* intoxicated_liblog_symbols(size_t* count) {
    if (count != nullptr) {
        *count = intoxicated_table_size(kLiblog);
    }
    return kLiblog;
}
