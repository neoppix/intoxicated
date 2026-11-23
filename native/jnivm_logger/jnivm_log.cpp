// libjnivm's log sink (see log.h for why it has this shape).
//
// Each message is printed the way libjnivm prints it without a logger,
// `[<tag>]: <message>` on stdout, unless `INTOXICATED_JNI_QUIET` is set. A
// message reporting that libjnivm invented a class, method or field for a
// lookup it could not answer ("Constructed Unresolved symbol", printed only
// when libjnivm is built with its JNI trace, INTOXICATED_JNI_TRACE in cargo's
// environment) also goes to the gap register in
// `crates/intoxicated-runtime/src/gap_log.rs`, quiet or not, under kind 0 (JNI).

#include "log.h"

#include <cstdarg>
#include <cstdio>
#include <cstdlib>
#include <cstring>

extern "C" void intoxicated_unimplemented_record(unsigned kind, const char* detail);

namespace {

bool quiet() {
    static const bool on = std::getenv("INTOXICATED_JNI_QUIET") != nullptr;
    return on;
}

} // namespace

namespace Log {

void debug(const char* tag, const char* format, ...) {
    if (format == nullptr) {
        return;
    }
    // 1023 characters and the NUL; a longer message is cut there.
    char message[1024];
    va_list ap;
    va_start(ap, format);
    const int n = vsnprintf(message, sizeof message, format, ap);
    va_end(ap);
    if (n < 0) {
        return;
    }
    if (!quiet()) {
        fprintf(stdout, "[%s]: %s\n", tag != nullptr ? tag : "JNIVM", message);
    }
    if (std::strstr(message, "Constructed Unresolved symbol") != nullptr) {
        intoxicated_unimplemented_record(0, message);
    }
}

} // namespace Log
