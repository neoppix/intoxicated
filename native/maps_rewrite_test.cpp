// Checks the synthetic `/proc/self/maps` rewrite in maps_rewrite.h without a
// running engine.
//
// The rewrite matched `/home/pascal/...` literally until it was derived from
// the process. Two things have to stay true across that change and both are
// easy to break silently, because a wrong rewrite produces a perfectly
// plausible map: the output for the launcher's layout must not move by a byte
// (the engine, APK and executable lines below are what the literal version
// produced), and the same layout under another HOME, another APK and another
// executable must come out identical to it rather than with its engine, APK
// and executable lines blanked. Paths in the profile and in the inherited
// runtime's own data directory carry the client's name, so they are blanked.

#include "maps_rewrite.h"

#include <cstdio>
#include <cstdlib>
#include <string>

#include <sys/stat.h>
#include <unistd.h>

using intoxicated::maps::make_sources;
using intoxicated::maps::rewrite_line;
using intoxicated::maps::Sources;

namespace {

int g_failures = 0;

void expect(bool ok, const char* what) {
    if (!ok) {
        std::fprintf(stderr, "maps_rewrite_test: FAILED: %s\n", what);
        ++g_failures;
    }
}

std::string rewrite(const std::string& maps, const Sources& src) {
    std::string out;
    size_t at = 0;
    while (at < maps.size()) {
        size_t nl = maps.find('\n', at);
        size_t end = nl == std::string::npos ? maps.size() : nl + 1;
        out += rewrite_line(maps.substr(at, end - at), src);
        at = end;
    }
    return out;
}

std::string replaced(std::string s, const std::string& from, const std::string& to) {
    for (size_t p; (p = s.find(from)) != std::string::npos;) {
        s.replace(p, from.size(), to);
    }
    return s;
}

const std::string k_pascal =
    "00200000-00243000 r--p 00000000 00:00 1234 /home/pascal/intoxicated/target/release/intoxicated-run\n"
    "00243000-00500000 r-xp 00043000 00:00 1234 /home/pascal/intoxicated/target/release/intoxicated-run\n"
    "800000000-800020000 r--p 00000000 00:00 77 /libexec/ld-elf.so.1\n"
    "800100000-800200000 r--p 00000000 00:00 78 /lib/libc.so.7\n"
    "800300000-800400000 r--p 00000000 00:00 79 /compat/linux/usr/lib64/libc.so.6\n"
    "800400000-800500000 r--p 00000000 00:00 80 /compat/linux/lib/libz.so.1\n"
    "800500000-800600000 r--p 00000000 00:00 81 /compat/linux/etc/ld.so.cache\n"
    "900000000-904000000 r-xp 00000000 00:00 5555 /home/pascal/.cache/intoxicated/apk/lib/x86_64/libroblox.so\n"
    "904000000-905000000 r--p 00000000 00:00 5556 /home/pascal/.cache/intoxicated/apk/candidate-0.apk\n"
    "905000000-905100000 rw-p 00000000 00:00 5557 /home/pascal/.local/share/intoxicated/profiles/default/data/x.db\n"
    "905100000-905200000 rw-p 00000000 00:00 5558 /home/pascal/.local/share/intoxicated/asset-trace.log\n"
    "905200000-905300000 r--p 00000000 00:00 5559 /home/pascal/.local/share/fonts/font.ttf\n"
    "905300000-905400000 r--p 00000000 00:00 5560 /home/pascal/Downloads/blob.bin\n"
    "906000000-906100000 r--p 00000000 00:00 90 /usr/local/lib/libX11.so.6\n"
    "907000000-907100000 rw-p 00000000 00:00 0 [heap]\n"
    "7fffffde0000-7ffffffff000 rw-p 00000000 00:00 0 [stack]\n";

// What the rules produce from `k_pascal`.
const std::string k_expected =
    "00200000-00243000 r--p 00000000 00:00 1234 /apex/com.android.runtime/lib64/bionic/libc.so\n"
    "00243000-00500000 r-xp 00043000 00:00 1234 /apex/com.android.runtime/lib64/bionic/libc.so\n"
    "800000000-800020000 r--p 00000000 00:00 77\n"
    "800100000-800200000 r--p 00000000 00:00 78\n"
    "800300000-800400000 r--p 00000000 00:00 79 /apex/com.android.runtime/lib64/bionic/libc.so.6\n"
    "800400000-800500000 r--p 00000000 00:00 80 /system/lib64/libz.so.1\n"
    "800500000-800600000 r--p 00000000 00:00 81\n"
    "900000000-904000000 r-xp 00000000 00:00 5555 /data/app/~~kQ8fN2pLx==/com.roblox.client-Rz9mAoY7w==/lib/arm64/libroblox.so\n"
    "904000000-905000000 r--p 00000000 00:00 5556 /data/app/~~kQ8fN2pLx==/com.roblox.client-Rz9mAoY7w==/base.apk\n"
    "905000000-905100000 rw-p 00000000 00:00 5557\n"
    "905100000-905200000 rw-p 00000000 00:00 5558\n"
    "905200000-905300000 r--p 00000000 00:00 5559 /data/data/com.roblox.client/.local/share/fonts/font.ttf\n"
    "905300000-905400000 r--p 00000000 00:00 5560 /data/data/com.roblox.client/Downloads/blob.bin\n"
    "906000000-906100000 r--p 00000000 00:00 90\n"
    "907000000-907100000 rw-p 00000000 00:00 0 [heap]\n"
    "7fffffde0000-7ffffffff000 rw-p 00000000 00:00 0 [stack]\n";

void the_launcher_layout_is_unchanged() {
    // Nothing set from the command line: the fallback layout under HOME.
    expect(rewrite(k_pascal, make_sources("/home/pascal", "", "", "", false)) == k_expected,
           "fallback layout under /home/pascal differs from the literal rules");
    // The same layout, named explicitly the way intoxicated-run names it.
    const Sources explicit_src = make_sources(
        "/home/pascal", "/home/pascal/.cache/intoxicated/apk/lib/x86_64",
        "/home/pascal/.cache/intoxicated/apk/candidate-0.apk",
        "/home/pascal/intoxicated/target/release/intoxicated-run", false);
    expect(rewrite(k_pascal, explicit_src) == k_expected,
           "explicit launcher layout differs from the literal rules");
}

void another_user_gets_the_same_map() {
    const std::string alice =
        replaced(replaced(replaced(replaced(k_pascal,
                                            "/home/pascal/intoxicated/target/release/intoxicated-run",
                                            "/opt/intox/bin/intoxicated-run"),
                                   "/home/pascal/.cache/intoxicated/apk/lib/x86_64",
                                   "/home/alice/roblox/lib/x86_64"),
                          "/home/pascal/.cache/intoxicated/apk/candidate-0.apk",
                          "/home/alice/roblox/base.apk"),
                 "/home/pascal", "/home/alice");
    const Sources src = make_sources("/home/alice", "/home/alice/roblox/lib/x86_64",
                                     "/home/alice/roblox/base.apk", "/opt/intox/bin/intoxicated-run",
                                     false);
    const std::string out = rewrite(alice, src);
    expect(out == k_expected, "another HOME, APK and executable did not produce the same map");
    expect(out.find("alice") == std::string::npos, "a host path survived");
    // The literal rules, for contrast, blanked the engine's line outright.
    expect(rewrite(alice, make_sources("/home/pascal", "", "", "", false)) != k_expected,
           "control: the old table should not have matched another user's paths");
}

void a_root_home_rewrites_nothing_extra() {
    // A prefix rule for `/` would rewrite every path and never terminate on
    // the replacement it just wrote.
    const Sources src = make_sources("/", "", "", "", false);
    for (const auto& e : src.host_to_android) {
        expect(e.first != "/" && !e.first.empty(), "an empty or root pattern was added");
    }
    expect(rewrite("1-2 r--p 0 00:00 1 /system/lib64/libz.so\n", src) ==
               "1-2 r--p 0 00:00 1 /system/lib64/libz.so\n",
           "an Android path was disturbed");
}

void a_symlinked_home_matches_the_kernels_spelling() {
    // FreeBSD's installer makes /home a link to /usr/home, and the map names a
    // file by its resolved path, so HOME's own spelling would never match.
    char real_dir[] = "/tmp/maps-rewrite-test.XXXXXX";
    if (!::mkdtemp(real_dir)) {
        expect(false, "mkdtemp");
        return;
    }
    char real_resolved[PATH_MAX];
    if (!::realpath(real_dir, real_resolved)) {
        expect(false, "realpath");
        return;
    }
    const std::string link = "/tmp/maps-rewrite-link." + std::to_string(::getpid());
    ::unlink(link.c_str());
    expect(::symlink(real_dir, link.c_str()) == 0, "symlink");
    const Sources src = make_sources(link, "", "", "", false);
    const std::string line = std::string("1-2 r--p 0 00:00 1 ") + real_resolved + "/Downloads/a.bin\n";
    expect(rewrite(line, src) == "1-2 r--p 0 00:00 1 /data/data/com.roblox.client/Downloads/a.bin\n",
           "a resolved HOME path was not rewritten");
    const std::string as_spelled = "1-2 r--p 0 00:00 1 " + link + "/Downloads/a.bin\n";
    expect(rewrite(as_spelled, src) == "1-2 r--p 0 00:00 1 /data/data/com.roblox.client/Downloads/a.bin\n",
           "HOME as spelled was not rewritten");
    ::unlink(link.c_str());
    ::rmdir(real_dir);
}

} // namespace

int main() {
    the_launcher_layout_is_unchanged();
    another_user_gets_the_same_map();
    a_root_home_rewrites_nothing_extra();
    a_symlinked_home_matches_the_kernels_spelling();
    if (g_failures == 0) {
        std::printf("maps_rewrite_test: all checks passed\n");
    }
    return g_failures == 0 ? 0 : 1;
}
