// log.h: the logger interface libjnivm compiles against.
//
// libjnivm's CMakeLists links a target named `logger` when one exists and then
// defines HAVE_LOGGER, under which `src/jnivm/internal/log.h` includes `<log.h>`
// from that target's include directory and turns every `LOG(tag, format, ...)`
// into `Log::debug(tag, format, ...)`. The file name, the namespace, the
// function name and its signature are therefore libjnivm's, not ours to pick.
// The definition is jnivm_log.cpp.
#pragma once

namespace Log {

/// One libjnivm message, printf-style. `tag` is a string literal (libjnivm
/// passes "JNIVM").
void debug(const char* tag, const char* format, ...) __attribute__((format(printf, 2, 3)));

} // namespace Log
