// The path rewrite behind the synthetic `/proc/self/maps` in system_paths.cpp,
// kept in a header of its own so maps_rewrite_test.cpp can drive it without the
// rest of that file's libc shims.
//
// The rewrite used to match literal paths under `/home/pascal`: the engine, the
// APK, `intoxicated-run` and the home directory itself. For any other user, or for
// a build or APK somewhere else, none of those matched, so the real lines fell
// through to the blanking pass below. The anticheat then saw a map in which the
// engine's own `libroblox.so` and the APK had no path at all, which no real
// device produces. The matching is now derived from the process (its HOME, the
// `--lib-dir` and `--apk` it was started with, its own executable); the Android
// paths written in their place are unchanged, byte for byte.
#pragma once

#include <cstddef>
#include <cstdlib>
#include <cstring>
#include <initializer_list>
#include <string>
#include <utility>
#include <vector>

#include <limits.h>
#include <pwd.h>
#include <unistd.h>
#if defined(__FreeBSD__)
#include <sys/auxv.h>
#endif

namespace intoxicated::maps {

/// What to look for, as `{host path, Android path}` pairs applied in order.
/// The order matters: a longer path has to be rewritten before a prefix of it,
/// or the prefix rule turns it into something no later rule matches.
struct Sources {
    std::vector<std::pair<std::string, std::string>> host_to_android;
};

inline std::string trimmed(std::string p) {
    while (p.size() > 1 && p.back() == '/') {
        p.pop_back();
    }
    return p;
}

/// The path the kernel will name in the map. A mapping's path is the vnode's,
/// so a symlinked HOME (FreeBSD's default installer makes `/home` a link to
/// `/usr/home`) shows up resolved, and matching the unresolved spelling alone
/// would miss every line.
inline std::string resolved(const std::string& p) {
    char buf[PATH_MAX];
    if (!p.empty() && ::realpath(p.c_str(), buf) != nullptr) {
        return buf;
    }
    return p;
}

/// The invoking user's home directory. `$HOME` first, as every other path in
/// the tree derives from it; the password database if it is unset or unusable.
/// A HOME of `/` is refused outright, because a prefix rule for `/` would
/// rewrite every path in the map.
inline std::string home_dir() {
    const char* h = std::getenv("HOME");
    std::string home = (h && h[0] == '/') ? trimmed(h) : std::string();
    if (home.size() <= 1) {
        const passwd* pw = ::getpwuid(::getuid());
        home = (pw && pw->pw_dir && pw->pw_dir[0] == '/') ? trimmed(pw->pw_dir) : std::string();
    }
    return home.size() > 1 ? home : std::string();
}

/// This process's own executable, as the kernel recorded it at exec.
inline std::string exe_path() {
#if defined(__FreeBSD__)
    char buf[PATH_MAX];
    if (::elf_aux_info(AT_EXECPATH, buf, sizeof buf) == 0 && buf[0] == '/') {
        return resolved(buf);
    }
#endif
    return std::string();
}

/// Build the table once. `lib_dir`, `apk` and `exe` may be empty, in which case
/// the layout `packaging/freebsd/intoxicated` installs under HOME stands in,
/// which is what the old literals spelled for the one user they named.
inline Sources make_sources(const std::string& home_in, const std::string& lib_dir,
                            const std::string& apk, const std::string& exe,
                            bool label_exe_as_app_process) {
    Sources s;
    auto& t = s.host_to_android;
    const std::string home = trimmed(home_in);
    const std::string real_home = home.empty() ? home : resolved(home);
    const std::string apk_dir = real_home.empty() ? real_home : real_home + "/.cache/intoxicated/apk";
    // Each is empty when neither the process nor a home can name it, and an
    // empty one is skipped below.
    const std::string engine = !lib_dir.empty() ? resolved(trimmed(lib_dir)) + "/libroblox.so"
                               : !apk_dir.empty() ? apk_dir + "/lib/x86_64/libroblox.so"
                                                  : std::string();
    const std::string apk_path = !apk.empty()     ? resolved(apk)
                                 : !apk_dir.empty() ? apk_dir + "/candidate-0.apk"
                                                    : std::string();
    const std::string exe_p = !exe.empty()       ? exe
                              : !real_home.empty() ? real_home + "/intoxicated/target/release/intoxicated-run"
                                                   : std::string();

    auto add = [&](const std::string& from, const char* to) {
        // An empty pattern matches everywhere and would never terminate, and
        // one for `/` would rewrite every path in the map.
        if (from.empty() || from == "/") {
            return;
        }
        for (const auto& e : t) {
            if (e.first == from) {
                return;
            }
        }
        t.emplace_back(from, to);
    };

    add(engine, "/data/app/~~kQ8fN2pLx==/com.roblox.client-Rz9mAoY7w==/lib/arm64/libroblox.so");
    add("/compat/linux/usr/lib64/", "/apex/com.android.runtime/lib64/bionic/");
    add("/compat/linux/usr/lib/", "/system/lib64/");
    add("/compat/linux/lib64/", "/system/lib64/");
    add("/compat/linux/lib/", "/system/lib64/");
    add("/compat/linux", "/system");
    // libroblox's GOT resolves its libc imports (open/read/uname/...) into
    // intoxicated-run's address space (the shims) -- confirmed by reading the live
    // GOT -- NOT into a libc.so. An anti-hook/GOT-integrity check that verifies
    // imports land in a libc mapping would flag that. So label the intoxicated-run
    // region as the Android libc.so, so a GOT entry pointing into it reads as
    // "points into libc" rather than "points into the main executable".
    // INTOXICATED_MAPS_EXE=appproc restores the old /system/bin/app_process64
    // as the control.
    add(exe_p, label_exe_as_app_process ? "/system/bin/app_process64"
                                        : "/apex/com.android.runtime/lib64/bionic/libc.so");
    add(apk_path, "/data/app/~~kQ8fN2pLx==/com.roblox.client-Rz9mAoY7w==/base.apk");
    // The profiles live under the data home, by default inside HOME, and every
    // path in them names `intoxicated/profiles`, so the pass below blanks them
    // whichever prefix rule rewrote the start of the line.
    if (!real_home.empty()) {
        add(real_home, "/data/data/com.roblox.client");
        // The spelling HOME itself uses, when the kernel's differs from it.
        add(home, "/data/data/com.roblox.client");
    }
    return s;
}

/// The table for this process, from its environment.
inline Sources sources_for_this_process(const char* lib_dir, const char* apk) {
    const char* label = std::getenv("INTOXICATED_MAPS_EXE");
    return make_sources(home_dir(), lib_dir ? lib_dir : "", apk ? apk : "", exe_path(),
                        label && std::strcmp(label, "appproc") == 0);
}

/// Rewrite one line of a Linux-format maps file, newline included.
inline std::string rewrite_line(std::string s, const Sources& src) {
    for (const auto& [from, to] : src.host_to_android) {
        for (size_t p; (p = s.find(from)) != std::string::npos;) {
            s.replace(p, from.size(), to);
        }
    }
    // Strip any path the rewrites above did not Android-ise. Roblox's Android
    // anti-cheat scans /proc/self/maps for substrings and reports a finding
    // (304) on a match; the client's real map still names the FreeBSD runtime
    // loader and libc (`/libexec/ld-elf.so.1`, `/lib/libc.so.7`, `/usr/lib`)
    // and the whole desktop windowing stack (`/usr/local/lib/libX11*`,
    // `libGLX_nvidia`, `libadwaita`, gio modules) -- none of which exists on a
    // real device. Any of those is a tell. Blank the path column for every
    // line whose path is not already under a genuine Android root, keeping the
    // address/perms/offset columns intact so anything cross-checking an
    // address against the map still resolves; a mapping with no path reads as
    // an ordinary anonymous region, which is unremarkable.
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
        // a later component -- the rewrites above map the HOME prefix but
        // leave the client's own `.../intoxicated/profiles/default/...`,
        // `.config/dconf`, etc. intact inside it. Treat any such line as a
        // tell and blank its path too, so no client name, "dconf" or host
        // string survives anywhere in the map the scanner reads.
        std::string pathstr(p);
        for (const char* tell : {"dconf", "intoxicated", "jnivm",
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
    return s;
}

} // namespace intoxicated::maps
