// Checks for the native files rewritten clean-room from the small-native spec,
// run on every build (CMakeLists.txt). It links the six archives the Rust
// binary links and calls them the way the Rust side and the engine do:
//
// - netdb_linux.cpp against FreeBSD's own getaddrinfo, node by node, and the
//   spec's measured vectors (layouts, flag and family translation, errors);
// - android_log.cpp's line format and level switch, in child processes with
//   a controlled environment;
// - legacy_streams.cpp's `__sF` slot mapping and pass-through;
// - thread_create.cpp's trace, including an address inside a file named
//   libroblox.so mapped executable for the purpose;
// - linker_surface.cpp, java_vm.cpp (VM, JNI_OnLoad, the invocation trace,
//   the terminate report), jni_cookies.cpp, deeplink.cpp and
//   android_platform.cpp through ordinary JNI calls;
// - system_paths.cpp's redirect, statvfs layout, realpath refusal and trace.
//
// Nothing here uses the network: every lookup is numeric, `localhost` or the
// wildcard address.
//
// Stand-ins for the three Rust symbols the archives call are at the top.

#include <jnivm.h>

#include <arpa/inet.h>
#include <dirent.h>
#include <fcntl.h>
#include <ifaddrs.h>
#include <net/if.h>
#include <netdb.h>
#include <netinet/in.h>
#include <pthread.h>
#include <signal.h>
#include <spawn.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <sys/sysctl.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#include <cerrno>
#include <cstdarg>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <stdexcept>
#include <string>
#include <vector>

#include "jni_values.h"
#include "jni_world.h"
#include "jnivm_logger/log.h"
#include "os_compat.h"
#include "symbol_table.h"

extern char** environ;

// ------------------------------------------------------------ Rust stand-ins

static unsigned char g_sf[3 * 152];
static bool g_sf_null = false;

extern "C" const unsigned char* intoxicated_legacy_sf_base(void) {
    return g_sf_null ? nullptr : g_sf;
}

extern "C" size_t intoxicated_legacy_sf_stride(void) {
    return 152;
}

extern "C" void intoxicated_unimplemented_record(unsigned kind, const char* detail) {
    fprintf(stderr, "[record] kind=%u %s\n", kind, detail ? detail : "(null)");
}

// ------------------------------------------------------------ the archives' C surface

extern "C" {
const IntoxicatedSymbol* intoxicated_liblog_symbols(size_t* count);
const IntoxicatedSymbol* intoxicated_netdb_symbols(size_t* count);
const IntoxicatedSymbol* intoxicated_thread_symbols(size_t* count);
const IntoxicatedSymbol* intoxicated_system_symbols(size_t* count);
void intoxicated_set_thread_trace(int on);
void intoxicated_set_system_root(const char* root);
void intoxicated_set_path_trace(int on);
const char* intoxicated_path_remap(const char* path, char* buf, size_t n);

int intoxicated_legacy_fflush(FILE*);
int intoxicated_legacy_fclose(FILE*);
int intoxicated_legacy_fseek(FILE*, long, int);
long intoxicated_legacy_ftell(FILE*);
int intoxicated_legacy_fputs(const char*, FILE*);
int intoxicated_legacy_setvbuf(FILE*, char*, int, size_t);
size_t intoxicated_legacy_fread(void*, size_t, size_t, FILE*);
size_t intoxicated_legacy_fwrite(const void*, size_t, size_t, FILE*);
int intoxicated_legacy_fprintf(FILE*, const char*, ...);
int intoxicated_legacy_vfprintf(FILE*, const char*, va_list);
int intoxicated_legacy_fputc(int, FILE*);
char* intoxicated_legacy_fgets(char*, int, FILE*);
int intoxicated_legacy_ferror(FILE*);
int intoxicated_legacy_feof(FILE*);
int intoxicated_legacy_fileno(FILE*);

void intoxicated_linker_init(void);
void* intoxicated_linker_load_library(const char* soname, const char* const* names, void* const* addrs, size_t n);
void* intoxicated_linker_dlopen(const char* filename, int flags);
void* intoxicated_linker_dlsym(void* handle, const char* symbol);
const char* intoxicated_linker_dlerror(void);
size_t intoxicated_linker_get_library_base(void* handle);
void intoxicated_linker_get_library_code_region(void* handle, size_t* base, size_t* size);

void* intoxicated_jni_create_vm(void);
int intoxicated_jni_call_onload(void* on_load, char* err, size_t err_len);
int intoxicated_jni_dump_classes(const char* path);
int intoxicated_java_member_answered(const char* class_name, int kind, const char* name, const char* descriptor);

void intoxicated_cookies_set_host_sink(void (*sink)(const char* host));
int intoxicated_cookies_register_handler(void* native, char* err, size_t err_len);
int intoxicated_cookies_get_for_domain(void* native, const char* class_name, const char* domain, char* out,
                                       size_t out_len, size_t* needed, char* err, size_t err_len);

int intoxicated_deeplink_protocol_string(void*, const char*, char*, size_t, char*, size_t);
int intoxicated_deeplink_string_ret_string(void*, const char*, const char*, char*, size_t, char*, size_t);
int intoxicated_deeplink_two_strings_ret_string(void*, const char*, const char*, const char*, char*, size_t, char*,
                                                size_t);
int intoxicated_deeplink_cold_start(void*, const char*, const char*, int*, char*, size_t);
int intoxicated_deeplink_protocol_init(void*, const char*, char*, size_t);
}

// ------------------------------------------------------------ harness

static int g_failures = 0;
static int g_checks = 0;

#define CHECK(cond)                                                                \
    do {                                                                           \
        ++g_checks;                                                                \
        if (!(cond)) {                                                             \
            ++g_failures;                                                          \
            fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond);        \
        }                                                                          \
    } while (0)

#define CHECK_EQ_STR(a, b)                                                                     \
    do {                                                                                       \
        ++g_checks;                                                                            \
        const std::string a_ = (a), b_ = (b);                                                  \
        if (a_ != b_) {                                                                        \
            ++g_failures;                                                                      \
            fprintf(stderr, "FAIL %s:%d: \"%s\" != \"%s\"\n", __FILE__, __LINE__, a_.c_str(),  \
                    b_.c_str());                                                               \
        }                                                                                      \
    } while (0)

static void* table_entry(const IntoxicatedSymbol* table, size_t n, const char* name) {
    for (size_t i = 0; i < n; ++i) {
        if (table[i].name && strcmp(table[i].name, name) == 0) {
            return table[i].addr;
        }
    }
    return nullptr;
}

static bool contains(const std::string& haystack, const std::string& needle) {
    return haystack.find(needle) != std::string::npos;
}

static std::vector<std::string> split_lines(const std::string& text) {
    std::vector<std::string> out;
    size_t pos = 0;
    while (pos < text.size()) {
        size_t nl = text.find('\n', pos);
        if (nl == std::string::npos) nl = text.size();
        out.push_back(text.substr(pos, nl - pos));
        pos = nl + 1;
    }
    return out;
}

/// Everything written to fd `fd` between construction and `take()`.
class Capture {
public:
    explicit Capture(int fd) : fd_(fd) {
        fflush(nullptr);
        char path[] = "./intoxicated-capture-XXXXXX";
        file_ = mkstemp(path);
        unlink(path);
        saved_ = dup(fd_);
        dup2(file_, fd_);
    }
    std::string take() {
        fflush(nullptr);
        dup2(saved_, fd_);
        close(saved_);
        std::string out;
        lseek(file_, 0, SEEK_SET);
        char buf[4096];
        ssize_t n;
        while ((n = read(file_, buf, sizeof buf)) > 0) out.append(buf, static_cast<size_t>(n));
        close(file_);
        return out;
    }

private:
    int fd_;
    int file_ = -1;
    int saved_ = -1;
};

static std::string self_path() {
    int mib[4] = {CTL_KERN, KERN_PROC, KERN_PROC_PATHNAME, -1};
    char path[1024];
    size_t len = sizeof path;
    if (sysctl(mib, 4, path, &len, nullptr, 0) != 0) return "";
    return std::string(path);
}

struct ChildResult {
    int status = -1;
    std::string err;
    std::string out;
};

/// This program again in `--child <mode>`, with only PATH, HOME and `env`
/// in its environment, stdout and stderr captured.
static ChildResult run_child(const char* mode, const std::vector<std::string>& env) {
    ChildResult r;
    std::vector<std::string> envs = {"PATH=/sbin:/bin:/usr/sbin:/usr/bin", "TZ=UTC"};
    for (const auto& e : env) envs.push_back(e);
    std::vector<char*> envp;
    for (auto& e : envs) envp.push_back(&e[0]);
    envp.push_back(nullptr);
    std::string self = self_path();
    std::string child_flag = "--child";
    std::string m = mode;
    char* argv[] = {&self[0], &child_flag[0], &m[0], nullptr};
    char out_path[] = "./intoxicated-child-out-XXXXXX";
    char err_path[] = "./intoxicated-child-err-XXXXXX";
    int out_fd = mkstemp(out_path);
    int err_fd = mkstemp(err_path);
    unlink(out_path);
    unlink(err_path);
    posix_spawn_file_actions_t fa;
    posix_spawn_file_actions_init(&fa);
    posix_spawn_file_actions_adddup2(&fa, out_fd, 1);
    posix_spawn_file_actions_adddup2(&fa, err_fd, 2);
    pid_t pid = -1;
    if (posix_spawn(&pid, self.c_str(), &fa, nullptr, argv, envp.data()) == 0) {
        waitpid(pid, &r.status, 0);
    }
    posix_spawn_file_actions_destroy(&fa);
    auto slurp = [](int fd) {
        std::string s;
        lseek(fd, 0, SEEK_SET);
        char buf[4096];
        ssize_t n;
        while ((n = read(fd, buf, sizeof buf)) > 0) s.append(buf, static_cast<size_t>(n));
        close(fd);
        return s;
    };
    r.out = slurp(out_fd);
    r.err = slurp(err_fd);
    return r;
}

// ------------------------------------------------------------ netdb

struct LxAddrinfo {
    int ai_flags, ai_family, ai_socktype, ai_protocol;
    socklen_t ai_addrlen;
    char* ai_canonname;
    unsigned char* ai_addr;
    LxAddrinfo* ai_next;
};
using Gai = int (*)(const char*, const char*, const LxAddrinfo*, LxAddrinfo**);
using Fai = void (*)(LxAddrinfo*);

static Gai g_gai;
static Fai g_fai;

static LxAddrinfo hints(int flags, int family, int socktype, int protocol = 0) {
    LxAddrinfo h{};
    h.ai_flags = flags;
    h.ai_family = family;
    h.ai_socktype = socktype;
    h.ai_protocol = protocol;
    return h;
}

static std::vector<LxAddrinfo*> nodes(LxAddrinfo* list) {
    std::vector<LxAddrinfo*> v;
    for (; list; list = list->ai_next) v.push_back(list);
    return v;
}

static int lx_af(int host_af) {
    return host_af == AF_INET6 ? 10 : host_af;
}

/// Our list against FreeBSD's, node by node: same order and count, the family
/// renumbered, and every sockaddr byte after Linux's 2-byte family equal to
/// FreeBSD's after its sa_len and family bytes.
static void same_as_host(LxAddrinfo* ours, const struct addrinfo* host) {
    auto v = nodes(ours);
    size_t i = 0;
    for (const struct addrinfo* h = host; h; h = h->ai_next, ++i) {
        CHECK(i < v.size());
        if (i >= v.size()) return;
        const LxAddrinfo* o = v[i];
        CHECK(o->ai_flags == 0);
        CHECK(o->ai_family == lx_af(h->ai_family));
        CHECK(o->ai_socktype == h->ai_socktype);
        CHECK(o->ai_protocol == h->ai_protocol);
        CHECK(o->ai_addrlen == h->ai_addrlen);
        if (o->ai_addr && h->ai_addr && o->ai_addrlen == h->ai_addrlen && o->ai_addrlen >= 2) {
            const uint16_t fam = static_cast<uint16_t>(o->ai_addr[0] | (o->ai_addr[1] << 8));
            CHECK(fam == static_cast<uint16_t>(lx_af(h->ai_family)));
            CHECK(memcmp(o->ai_addr + 2, reinterpret_cast<const unsigned char*>(h->ai_addr) + 2,
                         o->ai_addrlen - 2) == 0);
        }
        CHECK((o->ai_canonname == nullptr) == (h->ai_canonname == nullptr));
        if (o->ai_canonname && h->ai_canonname) CHECK_EQ_STR(o->ai_canonname, h->ai_canonname);
    }
    CHECK(i == v.size());
}

/// One lookup through the table, compared with FreeBSD's for `host_hints`.
static int compare(const char* node, const char* service, const LxAddrinfo* h, const struct addrinfo* host_hints,
                   const char* host_service, LxAddrinfo** keep = nullptr) {
    LxAddrinfo* res = reinterpret_cast<LxAddrinfo*>(0x1);
    const int rc = g_gai(node, service, h, &res);
    struct addrinfo* direct = nullptr;
    const int drc = getaddrinfo(node, host_service, host_hints, &direct);
    CHECK(rc == drc);
    if (rc == 0) {
        same_as_host(res, direct);
        if (keep) {
            *keep = res;
        } else {
            g_fai(res);
        }
    } else {
        CHECK(res == nullptr);
    }
    if (direct) freeaddrinfo(direct);
    return rc;
}

static void check_netdb() {
    size_t n = 0;
    const IntoxicatedSymbol* t = intoxicated_netdb_symbols(&n);
    CHECK(n == 2);
    g_gai = reinterpret_cast<Gai>(table_entry(t, n, "getaddrinfo"));
    g_fai = reinterpret_cast<Fai>(table_entry(t, n, "freeaddrinfo"));
    CHECK(g_gai && g_fai);
    if (!g_gai || !g_fai) return;

    // Layouts as the engine reads them.
    static_assert(sizeof(LxAddrinfo) == 48, "bionic addrinfo");
    const struct addrinfo zero_hints {};

    // 127.0.0.1:80, numeric: the Linux sockaddr_in byte for byte.
    {
        LxAddrinfo h = hints(0xc, 2, 1);
        struct addrinfo hh {};
        hh.ai_flags = AI_NUMERICHOST | AI_NUMERICSERV;
        hh.ai_family = AF_INET;
        hh.ai_socktype = SOCK_STREAM;
        LxAddrinfo* res = nullptr;
        CHECK(compare("127.0.0.1", "80", &h, &hh, "80", &res) == 0);
        auto v = nodes(res);
        CHECK(v.size() == 1);
        if (v.size() == 1) {
            const unsigned char want[16] = {2, 0, 0, 80, 127, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0};
            CHECK(v[0]->ai_family == 2 && v[0]->ai_socktype == 1 && v[0]->ai_protocol == 6);
            CHECK(v[0]->ai_addrlen == 16 && memcmp(v[0]->ai_addr, want, 16) == 0);
        }
        g_fai(res);
    }
    // ::1:443, numeric: Linux sockaddr_in6, family 10.
    {
        LxAddrinfo h = hints(0xc, 10, 1);
        struct addrinfo hh {};
        hh.ai_flags = AI_NUMERICHOST | AI_NUMERICSERV;
        hh.ai_family = AF_INET6;
        hh.ai_socktype = SOCK_STREAM;
        LxAddrinfo* res = nullptr;
        CHECK(compare("::1", "443", &h, &hh, "443", &res) == 0);
        auto v = nodes(res);
        CHECK(v.size() == 1);
        if (v.size() == 1) {
            unsigned char want[28] = {10, 0, 1, 187};
            want[8 + 15] = 1;
            CHECK(v[0]->ai_family == 10 && v[0]->ai_protocol == 6);
            CHECK(v[0]->ai_addrlen == 28 && memcmp(v[0]->ai_addr, want, 28) == 0);
        }
        g_fai(res);
    }
    // A scoped link-local address keeps its scope id, in host order.
    {
        LxAddrinfo h = hints(0xc, 10, 2);
        LxAddrinfo* res = nullptr;
        CHECK(g_gai("fe80::1%lo0", "53", &h, &res) == 0);
        auto v = nodes(res);
        CHECK(v.size() == 1);
        if (v.size() == 1) {
            uint32_t scope = 0;
            memcpy(&scope, v[0]->ai_addr + 24, 4);
            CHECK(v[0]->ai_family == 10 && v[0]->ai_protocol == 17 && v[0]->ai_addrlen == 28);
            CHECK(v[0]->ai_addr[8] == 0xfe && v[0]->ai_addr[9] == 0x80 && v[0]->ai_addr[23] == 1);
            CHECK(scope == if_nametoindex("lo0"));
        }
        g_fai(res);
    }
    // bionic's AI_DEFAULT (V4MAPPED_CFG | ADDRCONFIG): FreeBSD refuses it
    // directly; through the table it is V4MAPPED | ADDRCONFIG.
    {
        struct addrinfo raw {};
        raw.ai_flags = 0x600;
        raw.ai_socktype = SOCK_STREAM;
        struct addrinfo* d = nullptr;
        CHECK(getaddrinfo("localhost", "80", &raw, &d) == EAI_BADFLAGS);
        LxAddrinfo h = hints(0x600, 0, 1);
        struct addrinfo hh {};
        hh.ai_flags = AI_V4MAPPED | AI_ADDRCONFIG;
        hh.ai_socktype = SOCK_STREAM;
        CHECK(compare("localhost", "80", &h, &hh, "80") == 0);
        LxAddrinfo h2 = hints(0x200, 0, 1);
        struct addrinfo hh2 {};
        hh2.ai_flags = AI_V4MAPPED;
        hh2.ai_socktype = SOCK_STREAM;
        CHECK(compare("127.0.0.1", "80", &h2, &hh2, "80") == 0);
    }
    // A family with no FreeBSD match fails at once, res cleared.
    {
        LxAddrinfo h = hints(0, 99, 1);
        LxAddrinfo* res = reinterpret_cast<LxAddrinfo*>(0x1);
        CHECK(g_gai("127.0.0.1", "80", &h, &res) == 5);
        CHECK(res == nullptr);
        LxAddrinfo u = hints(0, 1, 1);
        res = reinterpret_cast<LxAddrinfo*>(0x1);
        CHECK(g_gai("127.0.0.1", "80", &u, &res) == 4);
        CHECK(res == nullptr);
    }
    // An empty service is no service: port 0, where FreeBSD says EAI_SERVICE.
    {
        struct addrinfo raw {};
        raw.ai_socktype = SOCK_STREAM;
        struct addrinfo* d = nullptr;
        CHECK(getaddrinfo("127.0.0.1", "", &raw, &d) == EAI_SERVICE);
        LxAddrinfo h = hints(0, 0, 1);
        CHECK(compare("127.0.0.1", "", &h, &raw, nullptr) == 0);
    }
    // No hints: every socket type the host lists, in its order.
    {
        LxAddrinfo* res = nullptr;
        CHECK(compare("127.0.0.1", nullptr, nullptr, nullptr, nullptr, &res) == 0);
        auto v = nodes(res);
        CHECK(v.size() == 3);
        for (auto* o : v) CHECK(o->ai_family == 2 && o->ai_addrlen == 16);
        g_fai(res);
    }
    // The wildcard address for a passive socket: v6 and v4, as the host orders them.
    {
        LxAddrinfo h = hints(0x1, 0, 2);
        struct addrinfo hh {};
        hh.ai_flags = AI_PASSIVE;
        hh.ai_socktype = SOCK_DGRAM;
        LxAddrinfo* res = nullptr;
        CHECK(compare(nullptr, "8080", &h, &hh, "8080", &res) == 0);
        bool v4 = false, v6 = false;
        for (auto* o : nodes(res)) {
            v4 |= o->ai_family == 2 && o->ai_addrlen == 16 && o->ai_addr[2] == 0x1f && o->ai_addr[3] == 0x90;
            v6 |= o->ai_family == 10 && o->ai_addrlen == 28 && o->ai_addr[2] == 0x1f && o->ai_addr[3] == 0x90;
        }
        CHECK(v4 && v6);
        g_fai(res);
    }
    // A canonical name is copied.
    {
        LxAddrinfo h = hints(0x2, 2, 1);
        LxAddrinfo* res = nullptr;
        CHECK(g_gai("localhost", "http", &h, &res) == 0);
        CHECK(res && res->ai_canonname && strcmp(res->ai_canonname, "localhost") == 0);
        CHECK(res && res->ai_addr && res->ai_addr[2] == 0 && res->ai_addr[3] == 80);
        g_fai(res);
    }
    // Errors in bionic numbering.
    {
        LxAddrinfo* res = nullptr;
        LxAddrinfo a = hints(0x4, 2, 0);
        CHECK(g_gai("localhost", nullptr, &a, &res) == 8);
        LxAddrinfo b = hints(0x8, 2, 1);
        CHECK(g_gai("127.0.0.1", "http", &b, &res) == 8);
        LxAddrinfo c = hints(0, 2, 1);
        CHECK(g_gai("127.0.0.1", "no-such-service-xyz", &c, &res) == 9);
        CHECK(g_gai(nullptr, nullptr, &c, &res) == 8);
        LxAddrinfo d = hints(0, 0, 7);
        CHECK(g_gai("127.0.0.1", "80", &d, &res) == 10);
        // FreeBSD's EAI_BADHINTS (12) arrives as EAI_FAIL.
        LxAddrinfo e = hints(0, 2, 1, 17);
        struct addrinfo raw {};
        raw.ai_family = AF_INET;
        raw.ai_socktype = SOCK_STREAM;
        raw.ai_protocol = IPPROTO_UDP;
        struct addrinfo* dd = nullptr;
        CHECK(getaddrinfo("127.0.0.1", "80", &raw, &dd) == 12);
        CHECK(g_gai("127.0.0.1", "80", &e, &res) == 4);
    }
    // V4MAPPED with a v6-only family: ::ffff:127.0.0.1.
    {
        LxAddrinfo h = hints(0x804, 10, 1);
        LxAddrinfo* res = nullptr;
        CHECK(g_gai("127.0.0.1", "80", &h, &res) == 0);
        auto v = nodes(res);
        CHECK(v.size() == 1);
        if (v.size() == 1) {
            const unsigned char mapped[16] = {0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 127, 0, 0, 1};
            CHECK(v[0]->ai_family == 10 && v[0]->ai_addrlen == 28 && memcmp(v[0]->ai_addr + 8, mapped, 16) == 0);
        }
        g_fai(res);
    }
    // Flags FreeBSD does not know are dropped, not refused.
    for (int flags : {0x1000, 0x10}) {
        LxAddrinfo h = hints(flags, 0, 0);
        LxAddrinfo* res = nullptr;
        CHECK(compare("127.0.0.1", "80", &h, &zero_hints, "80", &res) == 0);
        CHECK(nodes(res).size() == 3);
        g_fai(res);
    }
    // Pointer fields in the hints are ignored rather than refused.
    {
        LxAddrinfo h = hints(0, 0, 1);
        h.ai_addrlen = 16;
        h.ai_canonname = const_cast<char*>("junk");
        h.ai_addr = reinterpret_cast<unsigned char*>(&h);
        h.ai_next = &h;
        LxAddrinfo* res = nullptr;
        CHECK(g_gai("127.0.0.1", "80", &h, &res) == 0);
        g_fai(res);
    }
    // No result pointer: success, and the list is freed.
    {
        LxAddrinfo h = hints(0, 0, 1);
        CHECK(g_gai("127.0.0.1", "80", &h, nullptr) == 0);
        g_fai(nullptr);
    }
}

// ------------------------------------------------------------ liblog

using LogWrite = int (*)(int, const char*, const char*);
using LogBufWrite = int (*)(int, int, const char*, const char*);
using LogPrint = int (*)(int, const char*, const char*, ...);
using LogVprint = int (*)(int, const char*, const char*, va_list);
using LogAssert = void (*)(const char*, const char*, const char*, ...);
using SetAbort = void (*)(const char*);

static const IntoxicatedSymbol* liblog(size_t* n) {
    return intoxicated_liblog_symbols(n);
}

static int vprint_through(LogVprint f, int prio, const char* tag, const char* fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    const int r = f(prio, tag, fmt, ap);
    va_end(ap);
    return r;
}

static int child_liblog() {
    size_t n = 0;
    const IntoxicatedSymbol* t = liblog(&n);
    auto w = reinterpret_cast<LogWrite>(table_entry(t, n, "__android_log_write"));
    auto bw = reinterpret_cast<LogBufWrite>(table_entry(t, n, "__android_log_buf_write"));
    auto p = reinterpret_cast<LogPrint>(table_entry(t, n, "__android_log_print"));
    auto vp = reinterpret_cast<LogVprint>(table_entry(t, n, "__android_log_vprint"));
    auto abort_message = reinterpret_cast<SetAbort>(table_entry(t, n, "android_set_abort_message"));
    int ones = 0;
    ones += w(3, "Tag", "prio 3");
    ones += w(0, "Tag", "prio 0");
    ones += w(1, "Tag", "prio 1");
    ones += w(2, "Tag", "prio 2");
    ones += w(9, "Tag", "prio 9");
    ones += w(4, nullptr, nullptr);
    ones += w(4, "a-tag-that-is-longer-than-twenty-four-chars", "long tag");
    ones += w(4, "", "empty tag");
    ones += bw(3, 6, "buf", "from a buffer");
    ones += p(5, "fmt", "%s=%d %05.1f", "x", 7, 3.14159);
    std::string big(5000, 'A');
    ones += p(4, "big", "%s", big.c_str());
    ones += vprint_through(vp, 6, "vp", "%d-%s", 42, "v");
    abort_message("last words");
    abort_message(nullptr);
    fprintf(stderr, "ones=%d\n", ones);
    return 0;
}

static int child_liblog_assert() {
    size_t n = 0;
    const IntoxicatedSymbol* t = liblog(&n);
    auto a = reinterpret_cast<LogAssert>(table_entry(t, n, "__android_log_assert"));
    a("x < 0", "Tag", "value was %d", -3);
    return 0;
}

/// `HH:MM:SS.mmm L/` at the start of the line.
static bool stamped(const std::string& line, char letter) {
    if (line.size() < 15) return false;
    for (int i : {0, 1, 3, 4, 6, 7, 9, 10, 11}) {
        if (line[i] < '0' || line[i] > '9') return false;
    }
    return line[2] == ':' && line[5] == ':' && line[8] == '.' && line[12] == ' ' && line[13] == letter &&
           line[14] == '/';
}

static const std::string* line_with(const std::vector<std::string>& lines, const std::string& text) {
    for (const auto& l : lines) {
        if (contains(l, text)) return &l;
    }
    return nullptr;
}

static void check_liblog() {
    size_t n = 0;
    const IntoxicatedSymbol* t = liblog(&n);
    CHECK(n == 6);
    const char* names[] = {"__android_log_write", "__android_log_buf_write", "__android_log_print",
                           "__android_log_vprint", "__android_log_assert", "android_set_abort_message"};
    for (size_t i = 0; i < 6 && i < n; ++i) CHECK_EQ_STR(t[i].name, names[i]);

    // Default level (unset): DEBUG and up.
    ChildResult r = run_child("liblog", {});
    CHECK(WIFEXITED(r.status) && WEXITSTATUS(r.status) == 0);
    auto lines = split_lines(r.err);
    const std::string* l = line_with(lines, "prio 3");
    CHECK(l && stamped(*l, 'D') && l->substr(13) == "D/Tag                      prio 3");
    CHECK(!line_with(lines, "prio 0") && !line_with(lines, "prio 1") && !line_with(lines, "prio 2"));
    l = line_with(lines, "prio 9");
    CHECK(l && stamped(*l, '?') && l->substr(13) == "?/Tag                      prio 9");
    l = line_with(lines, "(no tag)");
    CHECK(l && l->substr(13) == "I/(no tag)                 (null)");
    l = line_with(lines, "long tag");
    CHECK(l && l->substr(13) == "I/a-tag-that-is-longer-than-twenty-four-chars long tag");
    l = line_with(lines, "empty tag");
    CHECK(l && l->substr(13) == "I/                         empty tag");
    l = line_with(lines, "from a buffer");
    CHECK(l && l->substr(13) == "E/buf                      from a buffer");
    l = line_with(lines, "x=7");
    CHECK(l && l->substr(13) == "W/fmt                      x=7 003.1");
    l = line_with(lines, "I/big");
    CHECK(l && l->size() == 13 + 2 + 24 + 1 + 4095 && l->back() == 'A');
    l = line_with(lines, "42-v");
    CHECK(l && l->substr(13) == "E/vp                       42-v");
    CHECK(line_with(lines, "[abort-message] last words") && line_with(lines, "[abort-message] (null)"));
    CHECK(line_with(lines, "ones=12"));

    r = run_child("liblog", {"INTOXICATED_LOG_LEVEL=V"});
    lines = split_lines(r.err);
    l = line_with(lines, "prio 2");
    CHECK(l && stamped(*l, 'V'));
    CHECK(!line_with(lines, "prio 1"));

    r = run_child("liblog", {"INTOXICATED_LOG_LEVEL=warn"});
    lines = split_lines(r.err);
    CHECK(!line_with(lines, "prio 3") && !line_with(lines, "long tag") && line_with(lines, "x=7"));
    CHECK(line_with(lines, "prio 9") && line_with(lines, "from a buffer"));

    for (const char* v : {"INTOXICATED_LOG_LEVEL=x", "INTOXICATED_LOG_LEVEL="}) {
        r = run_child("liblog", {v});
        lines = split_lines(r.err);
        CHECK(!line_with(lines, "prio 3") && line_with(lines, "long tag"));
    }

    r = run_child("liblog-assert", {});
    CHECK(WIFSIGNALED(r.status) && WTERMSIG(r.status) == SIGABRT);
    CHECK(contains(r.err, "\n*** Roblox fatal assertion ***\n    tag:       Tag\n    condition: x < 0\n"
                          "    message:   value was -3\n"));
}

// ------------------------------------------------------------ libjnivm's logger

static int child_logger() {
    Log::debug("JNIVM", "Constructed Unresolved symbol, Class=`%s`, StaticMethod=`%s`, Signature=`%s`", "a/B", "m",
               "()V");
    std::string long_text(1500, 'z');
    Log::debug("JNIVM", "123456789012345678901234567890%s", long_text.c_str());
    Log::debug(nullptr, "no tag");
    fflush(stdout);
    return 0;
}

static void check_logger() {
    const std::string unresolved = "Constructed Unresolved symbol, Class=`a/B`, StaticMethod=`m`, Signature=`()V`";
    ChildResult r = run_child("logger", {});
    CHECK(contains(r.out, "[JNIVM]: " + unresolved + "\n"));
    CHECK(contains(r.err, "[record] kind=0 " + unresolved + "\n"));
    auto lines = split_lines(r.out);
    const std::string* l = line_with(lines, "1234567890z");
    CHECK(l && l->size() == 9 + 1023);
    CHECK(line_with(lines, "[JNIVM]: no tag"));

    r = run_child("logger", {"INTOXICATED_JNI_QUIET=1"});
    CHECK(r.out.empty());
    CHECK(contains(r.err, "[record] kind=0 " + unresolved + "\n"));
}

// ------------------------------------------------------------ legacy streams

static void check_legacy_streams() {
    FILE* slot0 = reinterpret_cast<FILE*>(g_sf);
    FILE* slot1 = reinterpret_cast<FILE*>(g_sf + 152 + 7);
    FILE* slot2 = reinterpret_cast<FILE*>(g_sf + 2 * 152 + 151);
    CHECK(intoxicated_legacy_fileno(slot0) == 0);
    CHECK(intoxicated_legacy_fileno(slot1) == 1);
    CHECK(intoxicated_legacy_fileno(slot2) == 2);
    CHECK(intoxicated_legacy_ferror(slot2) == ferror(stderr));
    CHECK(intoxicated_legacy_feof(slot1) == feof(stdout));
    CHECK(intoxicated_legacy_fflush(nullptr) == 0);

    {
        Capture err(2);
        intoxicated_legacy_fprintf(slot2, "to %s %d\n", "stderr", 2);
        intoxicated_legacy_fputs("puts\n", slot2);
        intoxicated_legacy_fputc('c', slot2);
        intoxicated_legacy_fwrite("w\n", 1, 2, slot2);
        intoxicated_legacy_fflush(slot2);
        CHECK_EQ_STR(err.take(), "to stderr 2\nputs\ncw\n");
    }
    {
        Capture out(1);
        intoxicated_legacy_fprintf(slot1, "to stdout\n");
        intoxicated_legacy_fflush(slot1);
        CHECK_EQ_STR(out.take(), "to stdout\n");
    }

    // Any other FILE passes through untouched.
    FILE* f = tmpfile();
    CHECK(f != nullptr);
    if (f) {
        CHECK(intoxicated_legacy_setvbuf(f, nullptr, _IOFBF, 4096) == 0);
        CHECK(intoxicated_legacy_fputs("line one\n", f) >= 0);
        CHECK(intoxicated_legacy_fputc('x', f) == 'x');
        CHECK(intoxicated_legacy_fprintf(f, "%d\n", 12) == 3);
        CHECK(intoxicated_legacy_fwrite("tail\n", 1, 5, f) == 5);
        CHECK(intoxicated_legacy_fflush(f) == 0);
        CHECK(intoxicated_legacy_ftell(f) == 9 + 1 + 3 + 5);
        CHECK(intoxicated_legacy_fseek(f, 0, SEEK_SET) == 0);
        char buf[32];
        CHECK(intoxicated_legacy_fgets(buf, sizeof buf, f) == buf && strcmp(buf, "line one\n") == 0);
        CHECK(intoxicated_legacy_fread(buf, 1, 4, f) == 4 && memcmp(buf, "x12\n", 4) == 0);
        CHECK(intoxicated_legacy_fread(buf, 1, sizeof buf, f) == 5);
        CHECK(intoxicated_legacy_feof(f) != 0 && intoxicated_legacy_ferror(f) == 0);
        CHECK(intoxicated_legacy_fileno(f) == fileno(f));
        CHECK(intoxicated_legacy_fclose(f) == 0);
    }

    // Without the storage every pointer passes through.
    g_sf_null = true;
    CHECK(intoxicated_legacy_fileno(stderr) == 2);
    g_sf_null = false;
    // One past the last slot is not a slot (checked without calling through it).
    CHECK(intoxicated_legacy_fileno(stdout) == 1);
}

// ------------------------------------------------------------ thread trace

using PthreadCreate = int (*)(pthread_t*, const pthread_attr_t*, void* (*)(void*), void*);

static void* echo(void* arg) {
    return arg;
}

static void check_threads() {
    size_t n = 0;
    const IntoxicatedSymbol* t = intoxicated_thread_symbols(&n);
    CHECK(n == 1);
    auto create = reinterpret_cast<PthreadCreate>(table_entry(t, n, "pthread_create"));
    CHECK(create != nullptr);
    if (!create) return;

    // A bionic attr: stack size 256 KiB, joinable.
    unsigned char battr[56] = {};
    const size_t stack = 256 * 1024;
    memcpy(battr + 0x10, &stack, sizeof stack);
    pthread_t th;
    void* back = nullptr;
    {
        Capture err(2);
        CHECK(create(&th, reinterpret_cast<pthread_attr_t*>(battr), echo, &th) == 0);
        pthread_join(th, &back);
        CHECK(back == &th);
        CHECK(err.take().empty());
    }

    intoxicated_set_thread_trace(1);
    {
        Capture err(2);
        CHECK(create(&th, nullptr, echo, &th) == 0);
        pthread_join(th, &back);
        const std::string text = err.take();
        CHECK(back == &th);
        CHECK(contains(text, "[threads] tid="));
        CHECK(contains(text, " spawned by caller=0x"));
        CHECK(contains(text, " (outside libroblox.so) start_routine=0x"));
        CHECK(text.size() > 1 && text.back() == '\n' && text.find('\n') == text.size() - 1);
    }

    // A file named libroblox.so, mapped executable, holding `mov rax, rdi; ret`
    // at 0x1010: the start routine is reported as an offset into it.
    char dir[] = "./intoxicated-engine-XXXXXX";
    if (mkdtemp(dir) != nullptr) {
        const std::string path = std::string(dir) + "/libroblox.so";
        int fd = open(path.c_str(), O_RDWR | O_CREAT | O_TRUNC, 0700);
        std::vector<unsigned char> image(8192, 0xcc);
        const unsigned char code[] = {0x48, 0x89, 0xf8, 0xc3};
        memcpy(image.data() + 0x1010, code, sizeof code);
        CHECK(fd >= 0 && write(fd, image.data(), image.size()) == static_cast<ssize_t>(image.size()));
        void* map = mmap(nullptr, image.size(), PROT_READ | PROT_EXEC, MAP_PRIVATE, fd, 0);
        CHECK(map != MAP_FAILED);
        if (map != MAP_FAILED) {
            auto routine = reinterpret_cast<void* (*)(void*)>(static_cast<unsigned char*>(map) + 0x1010);
            Capture err(2);
            CHECK(create(&th, nullptr, routine, &th) == 0);
            pthread_join(th, &back);
            const std::string text = err.take();
            CHECK(back == &th);
            CHECK(contains(text, " (outside libroblox.so) start_routine=libroblox.so+0x1010\n"));
            munmap(map, image.size());
        }
        if (fd >= 0) close(fd);
        unlink(path.c_str());
        rmdir(dir);
    }
    intoxicated_set_thread_trace(0);
}

// ------------------------------------------------------------ the linker surface

static void check_linker() {
    intoxicated_linker_init();
    const char* names[] = {"spec_sym", "spec_sym"};
    void* addrs[] = {reinterpret_cast<void*>(0x1234), reinterpret_cast<void*>(0x5678)};
    void* h = intoxicated_linker_load_library("libspec-virtual.so", names, addrs, 2);
    CHECK(h != nullptr);
    CHECK(intoxicated_linker_dlsym(h, "spec_sym") == reinterpret_cast<void*>(0x1234));
    CHECK(intoxicated_linker_dlsym(h, "spec_nothing") == nullptr);
    CHECK(intoxicated_linker_dlopen("libspec-virtual.so", 2) == h);
    CHECK(intoxicated_linker_get_library_base(h) == 0);
    size_t base = 1, size = 1;
    intoxicated_linker_get_library_code_region(h, &base, &size);
    CHECK(base == 0 && size == 0);
    CHECK(intoxicated_linker_dlopen("libintoxicated-spec-absent.so", 2) == nullptr);
    const char* e = intoxicated_linker_dlerror();
    CHECK(e && strcmp(e, "dlopen failed: library \"libintoxicated-spec-absent.so\" not found") == 0);
    CHECK(intoxicated_linker_dlerror() == nullptr);

    size_t n = 0;
    const IntoxicatedSymbol* sys = intoxicated_system_symbols(&n);
    CHECK(intoxicated_linker_dlsym(nullptr, "openat") == table_entry(sys, n, "openat"));
}

static int child_dlopen_trace() {
    intoxicated_linker_init();
    intoxicated_linker_dlopen("libintoxicated-spec-absent.so", 2);
    return 0;
}

// ------------------------------------------------------------ system paths

static void check_system_paths() {
    size_t n = 0;
    const IntoxicatedSymbol* t = intoxicated_system_symbols(&n);
    const char* order[] = {"stat",    "lstat",   "fstat",    "fstatat",  "newfstatat", "access",  "faccessat",
                           "openat",  "opendir", "readdir",  "closedir", "realpath",   "getcwd",  "vasprintf",
                           "readlink", "fopen",  "statvfs",  "fstatfs",  "statfs",     "close",   "open"};
    CHECK(n == 21);
    for (size_t i = 0; i < 21 && i < n; ++i) CHECK_EQ_STR(t[i].name, order[i]);

    intoxicated_set_system_root("/tmp/sysroot///");
    char buf[64];
    auto remap = [&](const char* p) { return std::string(intoxicated_path_remap(p, buf, sizeof buf)); };
    CHECK_EQ_STR(remap("/system/fonts/a.ttf"), "/tmp/sysroot/fonts/a.ttf");
    CHECK_EQ_STR(remap("/system/"), "/tmp/sysroot/");
    for (const char* same : {"/system", "/systemx/y", "/data/system/x", "relative/system/x"}) {
        CHECK(intoxicated_path_remap(same, buf, sizeof buf) == same);
    }
    const std::string fits = "/system/" + std::string(50, 'f');
    CHECK(remap(fits.c_str()).size() == 63);
    const std::string too_long = "/system/" + std::string(51, 'f');
    CHECK(intoxicated_path_remap(too_long.c_str(), buf, sizeof buf) == too_long.c_str());
    intoxicated_set_system_root("/");
    CHECK_EQ_STR(remap("/system/fonts"), "//fonts");
    intoxicated_set_system_root("");
    CHECK(intoxicated_path_remap("/system/fonts", buf, sizeof buf) == std::string("/system/fonts"));
    intoxicated_set_system_root(nullptr);

    // statvfs in bionic's 112-byte layout.
    using Statvfs = int (*)(const char*, void*);
    auto sv = reinterpret_cast<Statvfs>(table_entry(t, n, "statvfs"));
    // 11 fields, 3 words of reserved zeros, and nothing written past 112 bytes.
    unsigned long b[15];
    memset(b, 0xab, sizeof b);
    struct statvfs host {};
    CHECK(statvfs("/", &host) == 0);
    CHECK(sv("/", b) == 0);
    CHECK(b[0] == host.f_bsize && b[1] == host.f_frsize && b[2] == host.f_blocks && b[3] == host.f_bfree);
    CHECK(b[4] == host.f_bavail && b[5] == host.f_files && b[6] == host.f_ffree && b[7] == host.f_favail);
    CHECK(b[8] == host.f_fsid && b[9] == host.f_flag && b[10] == host.f_namemax);
    CHECK(b[11] == 0 && b[12] == 0 && b[13] == 0 && b[14] == 0xababababababababUL);
    unsigned long untouched[15];
    memset(untouched, 0xcd, sizeof untouched);
    errno = 0;
    CHECK(sv("/nonexistent-intoxicated", untouched) == -1 && errno == ENOENT && untouched[0] == 0xcdcdcdcdcdcdcdcdUL);
    CHECK(sv("/", nullptr) == 0);

    // realpath never allocates for the engine.
    using Realpath = char* (*)(const char*, char*);
    auto rp = reinterpret_cast<Realpath>(table_entry(t, n, "realpath"));
    char resolved[PATH_MAX];
    CHECK(rp("/etc/../etc/hosts", resolved) == resolved && strcmp(resolved, "/etc/hosts") == 0);
    errno = 0;
    CHECK(rp("/etc/../etc/hosts", nullptr) == nullptr && errno == ENOTSUP);
    errno = 0;
    CHECK(rp("/nonexistent-intoxicated", nullptr) == nullptr && errno == ENOENT);

    // The trace: one line per call, the path actually used.
    char dir[] = "./intoxicated-sysroot-XXXXXX";
    if (mkdtemp(dir) == nullptr) return;
    char real_dir[PATH_MAX];
    CHECK(realpath(dir, real_dir) != nullptr);
    intoxicated_set_system_root(real_dir);
    using Access = int (*)(const char*, int);
    using Readlink = ssize_t (*)(const char*, char*, size_t);
    using Opendir = DIR* (*)(const char*);
    using Fopen = FILE* (*)(const char*, const char*);
    auto ac = reinterpret_cast<Access>(table_entry(t, n, "access"));
    auto rl = reinterpret_cast<Readlink>(table_entry(t, n, "readlink"));
    auto od = reinterpret_cast<Opendir>(table_entry(t, n, "opendir"));
    auto fo = reinterpret_cast<Fopen>(table_entry(t, n, "fopen"));
    intoxicated_set_path_trace(1);
    std::string text;
    {
        Capture err(2);
        char lb[16];
        rl("/system/nolink", lb, sizeof lb);
        ac("/system/fonts", 0);
        od("/system/nodir");
        DIR* d = od("/tmp");
        if (d) closedir(d);
        fo("/system/nofile", "r");
        sv("/", b);
        rp("/nonexistent-intoxicated", nullptr);
        text = err.take();
    }
    intoxicated_set_path_trace(0);
    const std::string root = real_dir;
    const long tid = intoxicated_gettid();
    char want[2048];
    snprintf(want, sizeof want,
             "[paths] tid=%ld readlink(\"%s/nolink\") = -1\n"
             "[paths] tid=%ld access(\"%s/fonts\") = -1\n"
             "[paths] tid=%ld opendir!(\"%s/nodir\") = null\n"
             "[paths] tid=%ld opendir(\"/tmp\") = ok\n"
             "[paths] tid=%ld fopen(\"%s/nofile\") = null\n"
             "[paths] tid=%ld statvfs(\"/\") = 0\n"
             "[paths] tid=%ld realpath(\"/nonexistent-intoxicated\") = null (no caller buffer; does not resolve)\n",
             tid, root.c_str(), tid, root.c_str(), tid, root.c_str(), tid, tid, root.c_str(), tid, tid);
    CHECK_EQ_STR(text, want);
    intoxicated_set_system_root(nullptr);
    rmdir(dir);
}

// ------------------------------------------------------------ the JavaVM

static jint fake_on_load(JavaVM* vm, void* reserved) {
    JNIEnv* env = nullptr;
    if (reserved != nullptr || vm->GetEnv(reinterpret_cast<void**>(&env), JNI_VERSION_1_6) != JNI_OK || !env) {
        return -1;
    }
    JavaVM* back = nullptr;
    if (env->GetJavaVM(&back) != JNI_OK || back == nullptr) return -1;
    return 0x10006;
}

static jint throwing_on_load(JavaVM*, void*) {
    throw std::runtime_error("on load threw");
}

static jint int_throwing_on_load(JavaVM*, void*) {
    throw 7;
}

static jint traced_on_load(JavaVM* vm, void*) {
    JNIEnv* env = nullptr;
    vm->GetEnv(reinterpret_cast<void**>(&env), JNI_VERSION_1_6);
    vm->AttachCurrentThread(&env, nullptr);
    return 0x10006;
}

static int child_jni_trace() {
    void* vm = intoxicated_jni_create_vm();
    if (!vm) return 2;
    char err[128] = {};
    return intoxicated_jni_call_onload(reinterpret_cast<void*>(&traced_on_load), err, sizeof err) == 0x10006 ? 0 : 3;
}

static void* boom(void*) {
    throw std::runtime_error("boom");
}

static int child_terminate() {
    if (!intoxicated_jni_create_vm()) return 2;
    pthread_t th;
    pthread_create(&th, nullptr, boom, nullptr);
    pthread_join(th, nullptr);
    return 4;
}

// Cookies.

static std::vector<std::string> g_hosts;
static void record_host(const char* host) {
    g_hosts.push_back(host);
}

static jobject g_handler;
static jclass g_protocol_seen;
static void fake_update(JNIEnv* env, jclass cls, jobject handler) {
    g_handler = env->NewGlobalRef(handler);
    g_protocol_seen = static_cast<jclass>(env->NewGlobalRef(cls));
    // The engine may call back during the registration.
    jclass hc = env->GetObjectClass(handler);
    jmethodID m = env->GetMethodID(hc, "onSetCookie", "([Ljava/lang/String;Ljava/lang/String;)V");
    env->CallVoidMethod(handler, m, nullptr, env->NewStringUTF("https://during.example/x"));
}

static std::string g_domain_seen;
static const char* g_jar = "a=1; b=2";
static jstring fake_get(JNIEnv* env, jclass, jstring domain) {
    const char* d = env->GetStringUTFChars(domain, nullptr);
    g_domain_seen = d ? d : "(null)";
    env->ReleaseStringUTFChars(domain, d);
    return g_jar ? env->NewStringUTF(g_jar) : nullptr;
}

static void* get_from_thread(void*) {
    char out[64];
    size_t needed = 0;
    char err[128] = {};
    const int rc = intoxicated_cookies_get_for_domain(reinterpret_cast<void*>(&fake_get), "com/example/Cookies",
                                                      "thread.example", out, sizeof out, &needed, err, sizeof err);
    return reinterpret_cast<void*>(static_cast<intptr_t>(rc == 0 && needed == strlen(g_jar) ? 1 : 0));
}

static void check_cookies(JNIEnv* env) {
    char err[512] = {};
    CHECK(intoxicated_cookies_register_handler(nullptr, err, sizeof err) == -1);
    CHECK_EQ_STR(err, "no JavaVM, or updateOnSetCookieHandler is not exported");

    intoxicated_cookies_set_host_sink(record_host);
    memset(err, 0, sizeof err);
    CHECK(intoxicated_cookies_register_handler(reinterpret_cast<void*>(&fake_update), err, sizeof err) == 0);
    CHECK(g_handler != nullptr);
    CHECK(g_hosts.size() == 1 && g_hosts[0] == "during.example");
    CHECK(env->IsSameObject(g_protocol_seen, env->FindClass("com/roblox/universalapp/cookie/JNICookieProtocol")));
    if (!g_handler) return;

    jclass hc = env->GetObjectClass(g_handler);
    CHECK(env->IsSameObject(hc, env->FindClass("com/roblox/universalapp/cookie/JNICookieProtocol$OnSetCookieHandler")));
    jmethodID m = env->GetMethodID(hc, "onSetCookie", "([Ljava/lang/String;Ljava/lang/String;)V");
    CHECK(m != nullptr);
    jobjectArray cookies = env->NewObjectArray(1, env->FindClass("java/lang/String"), env->NewStringUTF("SECRET=1"));
    struct Case {
        std::string url;
        const char* host;
    };
    const Case cases[] = {
        {"https://apis.roblox.com/browser-tracker-api/device/initialize", "apis.roblox.com"},
        {std::string("https://user:pw") + "@" + "www.roblox.com:443/path?x=y", "www.roblox.com"},
        {"roblox.com/foo", "roblox.com"},
        {"https://host.example?ticket=abc", "host.example"},
        {"https://host.example#frag", "host.example"},
        {std::string("http://ex.com:80") + "@" + "evil.example/", "evil.example"},
        {"HTTPS://UPPER.Example.COM/", "UPPER.Example.COM"},
        {"https://host.example:", "host.example"},
        {"justahost", "justahost"},
        {std::string("ftp://a") + "@" + "b" + "@" + "c.example:21/x", "c.example"},
        {"https://[::1]:8080/", "[::1]:8080"},
        {"roblox.com/x?next=https://other.example/", "roblox.com"},
        {"", nullptr},
        {"https://", nullptr},
    };
    for (const auto& c : cases) {
        g_hosts.clear();
        env->CallVoidMethod(g_handler, m, cookies, env->NewStringUTF(c.url.c_str()));
        if (c.host) {
            CHECK(g_hosts.size() == 1 && g_hosts[0] == c.host);
            if (g_hosts.size() == 1 && g_hosts[0] != c.host) fprintf(stderr, "  host of %s: %s\n", c.url.c_str(), g_hosts[0].c_str());
        } else {
            CHECK(g_hosts.empty());
        }
    }
    g_hosts.clear();
    env->CallVoidMethod(g_handler, m, cookies, nullptr);
    CHECK(g_hosts.empty());
    env->CallVoidMethod(g_handler, m, nullptr, env->NewStringUTF("https://x.example/"));
    CHECK(g_hosts.size() == 1 && g_hosts[0] == "x.example");
    intoxicated_cookies_set_host_sink(nullptr);
    g_hosts.clear();
    env->CallVoidMethod(g_handler, m, cookies, env->NewStringUTF("https://x.example/"));
    CHECK(g_hosts.empty());
    CHECK(!env->ExceptionCheck());

    // The jar.
    auto get = reinterpret_cast<void*>(&fake_get);
    char out[64];
    size_t needed = 99;
    memset(err, 0, sizeof err);
    CHECK(intoxicated_cookies_get_for_domain(nullptr, "com/example/Cookies", "example.com", out, sizeof out, &needed,
                                             err, sizeof err) == -1);
    CHECK_EQ_STR(err, "no JavaVM, or the cookie native is not exported");
    CHECK(needed == 99);
    CHECK(intoxicated_cookies_get_for_domain(get, nullptr, "example.com", out, sizeof out, &needed, err,
                                             sizeof err) == -1);
    CHECK(intoxicated_cookies_get_for_domain(get, "com/example/Cookies", "example.com", out, 64, &needed, err,
                                             sizeof err) == 0);
    CHECK(needed == 8 && strcmp(out, "a=1; b=2") == 0 && g_domain_seen == "example.com");
    memset(err, 0, sizeof err);
    CHECK(intoxicated_cookies_get_for_domain(get, "com/example/Cookies", "example.com", out, 8, &needed, err,
                                             sizeof err) == -1);
    CHECK(needed == 8);
    CHECK_EQ_STR(err, "the jar for this domain is 8 bytes and the buffer is 8");
    CHECK(intoxicated_cookies_get_for_domain(get, "com/example/Cookies", "example.com", out, 9, &needed, err,
                                             sizeof err) == 0);
    CHECK(intoxicated_cookies_get_for_domain(get, "com/example/Cookies", nullptr, out, 9, &needed, err,
                                             sizeof err) == 0);
    CHECK(g_domain_seen.empty());
    g_jar = nullptr;
    CHECK(intoxicated_cookies_get_for_domain(get, "com/example/Cookies", "example.com", out, 9, &needed, err,
                                             sizeof err) == 0);
    CHECK(needed == 0 && out[0] == '\0');
    g_jar = "z";
    memset(err, 0, sizeof err);
    CHECK(intoxicated_cookies_get_for_domain(get, "com/example/Cookies", "example.com", out, 0, &needed, err,
                                             sizeof err) == -1);
    CHECK_EQ_STR(err, "the jar for this domain is 1 bytes and the buffer is 0");
    // A 6-byte message buffer gets the first five characters.
    char tiny[6];
    CHECK(intoxicated_cookies_get_for_domain(nullptr, "c", "d", out, 9, &needed, tiny, sizeof tiny) == -1);
    CHECK_EQ_STR(tiny, "no Ja");
    g_jar = "a=1; b=2";
    // From a thread libjnivm has never seen.
    pthread_t th;
    void* ok = nullptr;
    pthread_create(&th, nullptr, get_from_thread, nullptr);
    pthread_join(th, &ok);
    CHECK(ok == reinterpret_cast<void*>(1));
}

// Deep links.

static const char* g_answer = "urlKey";
static jstring fake_protocol_string(JNIEnv* env, jclass) {
    return g_answer ? env->NewStringUTF(g_answer) : nullptr;
}

static std::string text_of(JNIEnv* env, jstring s) {
    if (!s) return "(null)";
    const char* p = env->GetStringUTFChars(s, nullptr);
    std::string out = p ? p : "";
    env->ReleaseStringUTFChars(s, p);
    return out;
}

static std::string g_url_seen;
static jboolean g_handled = 1;
static jboolean fake_cold_start(JNIEnv* env, jclass, jstring url) {
    g_url_seen = text_of(env, url);
    return g_handled;
}

static jstring fake_join(JNIEnv* env, jclass, jstring a, jstring b) {
    return env->NewStringUTF((text_of(env, a) + "|" + text_of(env, b)).c_str());
}

static jstring fake_wrap(JNIEnv* env, jclass, jstring a) {
    return env->NewStringUTF(("<" + text_of(env, a) + ">").c_str());
}

static bool g_context_ok = false;
static void fake_init(JNIEnv* env, jclass, jobject context) {
    g_context_ok = context && env->IsSameObject(env->GetObjectClass(context), env->FindClass("java/lang/Object"));
}

static void check_deeplink() {
    char out[64];
    char err[512] = {};
    auto ps = reinterpret_cast<void*>(&fake_protocol_string);
    const char* cls = "com/roblox/protocols/webview/WebViewProtocol";
    CHECK(intoxicated_deeplink_protocol_string(ps, cls, out, 16, err, sizeof err) == 0 && strcmp(out, "urlKey") == 0);
    CHECK(intoxicated_deeplink_protocol_string(ps, cls, out, 6, err, sizeof err) == -1);
    CHECK_EQ_STR(err, "the answer is 6 bytes and the buffer is 6");
    CHECK(intoxicated_deeplink_protocol_string(ps, cls, out, 7, err, sizeof err) == 0);
    g_answer = nullptr;
    CHECK(intoxicated_deeplink_protocol_string(ps, cls, out, 7, err, sizeof err) == 0 && out[0] == '\0');
    g_answer = "urlKey";
    CHECK(intoxicated_deeplink_protocol_string(nullptr, cls, out, 7, err, sizeof err) == -1);
    CHECK_EQ_STR(err, "no JavaVM, or the native is not exported");

    auto cs = reinterpret_cast<void*>(&fake_cold_start);
    int handled = -1;
    const char* lp = "com/roblox/universalapp/linking/JNIBaseUrlProtocol";
    CHECK(intoxicated_deeplink_cold_start(cs, lp, "roblox://experiences/start?placeId=1", &handled, err,
                                          sizeof err) == 0);
    CHECK(handled == 1 && g_url_seen == "roblox://experiences/start?placeId=1");
    g_handled = 0;
    CHECK(intoxicated_deeplink_cold_start(cs, lp, nullptr, &handled, err, sizeof err) == 0);
    CHECK(handled == 0 && g_url_seen.empty());
    g_handled = 2;
    CHECK(intoxicated_deeplink_cold_start(cs, lp, "x", &handled, err, sizeof err) == 0 && handled == 1);
    CHECK(intoxicated_deeplink_cold_start(cs, lp, "x", nullptr, err, sizeof err) == 0);
    handled = -1;
    CHECK(intoxicated_deeplink_cold_start(nullptr, lp, "x", &handled, err, sizeof err) == -1 && handled == -1);

    auto j = reinterpret_cast<void*>(&fake_join);
    const char* bus = "com/roblox/universalapp/messagebus/MessageBus";
    CHECK(intoxicated_deeplink_two_strings_ret_string(j, bus, "Proto", "method", out, sizeof out, err,
                                                      sizeof err) == 0);
    CHECK_EQ_STR(out, "Proto|method");
    CHECK(intoxicated_deeplink_two_strings_ret_string(j, bus, nullptr, nullptr, out, sizeof out, err,
                                                      sizeof err) == 0);
    CHECK_EQ_STR(out, "|");
    CHECK(intoxicated_deeplink_two_strings_ret_string(j, bus, "Proto", "method", out, 12, err, sizeof err) == -1);
    CHECK_EQ_STR(err, "the answer is 12 bytes and the buffer is 12");

    auto w = reinterpret_cast<void*>(&fake_wrap);
    CHECK(intoxicated_deeplink_string_ret_string(w, bus, "{\"a\":1}", out, sizeof out, err, sizeof err) == 0);
    CHECK_EQ_STR(out, "<{\"a\":1}>");

    CHECK(intoxicated_deeplink_protocol_init(reinterpret_cast<void*>(&fake_init),
                                             "com/roblox/universalapp/linking/JNIWebLoginProtocol", err,
                                             sizeof err) == 0);
    CHECK(g_context_ok);
}

// Platform classes.

static std::string expected_ipv4() {
    struct ifaddrs* list = nullptr;
    if (getifaddrs(&list) != 0) return "";
    std::string joined;
    for (auto* i = list; i; i = i->ifa_next) {
        if (!i->ifa_addr || i->ifa_addr->sa_family != AF_INET || (i->ifa_flags & IFF_LOOPBACK)) continue;
        char text[INET_ADDRSTRLEN];
        inet_ntop(AF_INET, &reinterpret_cast<sockaddr_in*>(i->ifa_addr)->sin_addr, text, sizeof text);
        if (!joined.empty()) joined += ',';
        joined += text;
    }
    freeifaddrs(list);
    return joined;
}

static std::string static_string(JNIEnv* env, const char* cls, const char* name) {
    jclass c = env->FindClass(cls);
    jmethodID m = env->GetStaticMethodID(c, name, "()Ljava/lang/String;");
    return text_of(env, static_cast<jstring>(env->CallStaticObjectMethod(c, m)));
}

static void check_platform(JNIEnv* env) {
    struct Hooked {
        const char* cls;
        int kind;
        const char* name;
        const char* descriptor;
    };
    const Hooked hooked[] = {
        {"android/app/Application", 0, "getProcessName", "()Ljava/lang/String;"},
        {"android/app/Application", 0, "getSharedPreferences", "(Ljava/lang/String;I)Landroid/content/SharedPreferences;"},
        {"android/app/Application", 0, "checkSelfPermission", "(Ljava/lang/String;)I"},
        {"android/app/ActivityThread", 1, "currentActivityThread", "()Landroid/app/ActivityThread;"},
        {"android/app/ActivityThread", 0, "getApplication", "()Landroid/app/Application;"},
        {"com/roblox/engine/jni/util/NetworkUtils", 1, "getPublicIPv4Addresseses", "()Ljava/lang/String;"},
        {"com/roblox/audio/AppRtcDeviceWrapper", 0, "isValid", "()Z"},
        {"org/webrtc/voiceengine/BuildInfo", 1, "getBrand", "()Ljava/lang/String;"},
        {"org/webrtc/voiceengine/BuildInfo", 1, "getDeviceManufacturer", "()Ljava/lang/String;"},
        {"org/webrtc/voiceengine/BuildInfo", 1, "getDeviceModel", "()Ljava/lang/String;"},
        {"org/webrtc/voiceengine/BuildInfo", 1, "getDevice", "()Ljava/lang/String;"},
        {"org/webrtc/voiceengine/BuildInfo", 1, "getDeviceProduct", "()Ljava/lang/String;"},
        {"org/webrtc/voiceengine/BuildInfo", 1, "getAndroidBuildId", "()Ljava/lang/String;"},
        {"org/webrtc/voiceengine/BuildInfo", 1, "getBuildType", "()Ljava/lang/String;"},
        {"org/webrtc/voiceengine/BuildInfo", 1, "getBuildRelease", "()Ljava/lang/String;"},
        {"org/webrtc/voiceengine/BuildInfo", 1, "getSdkVersion", "()Ljava/lang/String;"},
        {"com/roblox/universalapp/cookie/JNICookieProtocol$OnSetCookieHandler", 0, "onSetCookie",
         "([Ljava/lang/String;Ljava/lang/String;)V"},
    };
    for (const auto& h : hooked) {
        const int r = intoxicated_java_member_answered(h.cls, h.kind, h.name, h.descriptor);
        CHECK(r == 1);
        if (r != 1) fprintf(stderr, "  not hooked: %s %s %s\n", h.cls, h.name, h.descriptor);
    }
    // A static lookup never finds an instance hook, and the reverse.
    CHECK(intoxicated_java_member_answered("android/app/ActivityThread", 0, "currentActivityThread",
                                           "()Landroid/app/ActivityThread;") == 0);
    CHECK(intoxicated_java_member_answered("android/app/Application", 1, "getProcessName", "()Ljava/lang/String;") == 0);

    // The classes with no members exist all the same.
    jnivm::VM* vm = intoxicated::process_env()->GetVM();
    for (const char* name : {"com/roblox/engine/jni/memstorage/Connection", "org/webrtc/voiceengine/WebRtcAudioTrack"}) {
        std::lock_guard<std::mutex> lock(vm->mtx);
        CHECK(vm->classes.count(name) == 1);
    }
    {
        std::lock_guard<std::mutex> lock(vm->mtx);
        auto it = vm->typecheck.find(typeid(jnivm::Object));
        CHECK(it != vm->typecheck.end() && it->second && it->second->nativeprefix == "java/lang/Object");
    }

    jclass at = env->FindClass("android/app/ActivityThread");
    jmethodID current = env->GetStaticMethodID(at, "currentActivityThread", "()Landroid/app/ActivityThread;");
    jobject t1 = env->CallStaticObjectMethod(at, current);
    jobject t2 = env->CallStaticObjectMethod(at, current);
    CHECK(t1 && t2 && !env->IsSameObject(t1, t2));
    CHECK(t1 && env->IsSameObject(env->GetObjectClass(t1), at));
    jmethodID get_app = env->GetMethodID(at, "getApplication", "()Landroid/app/Application;");
    jobject a1 = env->CallObjectMethod(t1, get_app);
    jobject a2 = env->CallObjectMethod(t2, get_app);
    jclass app = env->FindClass("android/app/Application");
    CHECK(a1 && a2 && env->IsSameObject(a1, a2));
    CHECK(a1 && env->IsSameObject(env->GetObjectClass(a1), app));
    jmethodID process_name = env->GetMethodID(app, "getProcessName", "()Ljava/lang/String;");
    CHECK_EQ_STR(text_of(env, static_cast<jstring>(env->CallObjectMethod(a1, process_name))), "com.roblox.client");

    const std::string ips = static_string(env, "com/roblox/engine/jni/util/NetworkUtils", "getPublicIPv4Addresseses");
    CHECK_EQ_STR(ips, expected_ipv4());
    CHECK(!contains(ips, "127.0.0.1"));

    jclass wrapper = env->FindClass("com/roblox/audio/AppRtcDeviceWrapper");
    auto device = intoxicated::java::instance_of(intoxicated::process_env(), wrapper);
    jobject dev = intoxicated::java::to_jni(intoxicated::process_env(), device);
    CHECK(env->CallBooleanMethod(dev, env->GetMethodID(wrapper, "isValid", "()Z")) == JNI_FALSE);

    const intoxicated::DeviceRecord& d = intoxicated::device_record();
    auto s = [](const char* p) { return std::string(p ? p : ""); };
    const char* bi = "org/webrtc/voiceengine/BuildInfo";
    CHECK_EQ_STR(static_string(env, bi, "getBrand"), s(d.brand));
    CHECK_EQ_STR(static_string(env, bi, "getDeviceManufacturer"), s(d.manufacturer));
    CHECK_EQ_STR(static_string(env, bi, "getDeviceModel"), s(d.model));
    CHECK_EQ_STR(static_string(env, bi, "getDevice"), s(d.device));
    CHECK_EQ_STR(static_string(env, bi, "getDeviceProduct"), s(d.product));
    CHECK_EQ_STR(static_string(env, bi, "getAndroidBuildId"), s(d.build_id));
    CHECK_EQ_STR(static_string(env, bi, "getBuildType"), s(d.build_type));
    CHECK_EQ_STR(static_string(env, bi, "getBuildRelease"), s(d.build_release));
    CHECK_EQ_STR(static_string(env, bi, "getSdkVersion"), s(d.sdk_version));
    CHECK(!env->ExceptionCheck());
}

static void check_jni() {
    char err[512] = {};
    CHECK(intoxicated_jni_call_onload(reinterpret_cast<void*>(&fake_on_load), err, sizeof err) == -1);
    CHECK(intoxicated_jni_dump_classes("/nonexistent/intoxicated-class-dump") == -1);
    CHECK(intoxicated::process_env() == nullptr);

    std::string vm_line;
    void* vm = nullptr;
    {
        Capture e(2);
        vm = intoxicated_jni_create_vm();
        vm_line = e.take();
    }
    CHECK(vm != nullptr);
    if (!vm) return;
    CHECK(contains(vm_line, "[jni] JavaVM=") && contains(vm_line, " reserved0=") && contains(vm_line, " (expect "));
    CHECK(intoxicated_jni_create_vm() == nullptr);

    // The handed-out entries reach libjnivm's VM whatever JavaVM pointer they
    // are called with, null included; libjnivm's own entries throw for that.
    auto* handed = static_cast<JavaVM*>(vm);
    JNIEnv* through_null = nullptr;
    CHECK(handed->functions->GetEnv(nullptr, reinterpret_cast<void**>(&through_null), JNI_VERSION_1_6) == JNI_OK);
    CHECK(through_null != nullptr);
    JavaVM copy = *handed;
    JNIEnv* through_copy = nullptr;
    CHECK(copy.AttachCurrentThread(&through_copy, nullptr) == JNI_OK && through_copy == through_null);

    CHECK(intoxicated_jni_call_onload(nullptr, err, sizeof err) == -1);
    CHECK(intoxicated_jni_call_onload(reinterpret_cast<void*>(&fake_on_load), err, sizeof err) == 0x10006);
    memset(err, 0, sizeof err);
    CHECK(intoxicated_jni_call_onload(reinterpret_cast<void*>(&throwing_on_load), err, sizeof err) == -2);
    CHECK_EQ_STR(err, "on load threw");
    memset(err, 0, sizeof err);
    CHECK(intoxicated_jni_call_onload(reinterpret_cast<void*>(&int_throwing_on_load), err, sizeof err) == -3);
    CHECK_EQ_STR(err, "non-standard C++ exception");

    char dump[] = "./intoxicated-dump-XXXXXX";
    int fd = mkstemp(dump);
    if (fd >= 0) close(fd);
    CHECK(intoxicated_jni_dump_classes(dump) == 0);
    struct stat st {};
    CHECK(stat(dump, &st) == 0 && st.st_size > 0);
    unlink(dump);
    CHECK(intoxicated_jni_dump_classes("/nonexistent/intoxicated-class-dump") == 0);

    jnivm::ENV* penv = intoxicated::process_env();
    CHECK(penv != nullptr);
    if (!penv) return;
    JNIEnv* env = penv->GetJNIEnv();
    env->PushLocalFrame(256);
    check_cookies(env);
    check_deeplink();
    check_platform(env);
    env->PopLocalFrame(nullptr);

    ChildResult r = run_child("jni-trace", {"INTOXICATED_JNI_TRACE="});
    CHECK(WIFEXITED(r.status) && WEXITSTATUS(r.status) == 0);
    CHECK(contains(r.err, "[jni] GetEnv(vm=0x") && contains(r.err, ") [ours]\n"));
    CHECK(contains(r.err, "[jni] AttachCurrentThread(vm=0x"));
    r = run_child("jni-trace", {});
    CHECK(WIFEXITED(r.status) && WEXITSTATUS(r.status) == 0 && !contains(r.err, "[jni] GetEnv("));

    char dump_path[] = "./intoxicated-terminate-dump-XXXXXX";
    fd = mkstemp(dump_path);
    if (fd >= 0) close(fd);
    unlink(dump_path);
    r = run_child("terminate", {std::string("INTOXICATED_JNI_DUMP=") + dump_path});
    CHECK(WIFEXITED(r.status) && WEXITSTATUS(r.status) == 70);
    CHECK(contains(r.err, "\n*** uncaught C++ exception on thread "));
    CHECK(contains(r.err, "\n    what(): boom\n"));
    CHECK(contains(r.err, " frames:\n"));
    CHECK(contains(r.err, std::string("    Java surface Roblox reached for -> ") + dump_path + "\n"));
    CHECK(stat(dump_path, &st) == 0 && st.st_size > 0);
    unlink(dump_path);
}

static void check_dlopen_trace() {
    ChildResult r = run_child("dlopen-trace", {"INTOXICATED_TRACE_DLOPEN=1"});
    CHECK(contains(r.err, "[intoxicated] dlopen(libintoxicated-spec-absent.so) -> NULL in "));
    CHECK(contains(r.err, "us\n"));
}

int main(int argc, char** argv) {
    if (argc == 3 && strcmp(argv[1], "--child") == 0) {
        const std::string mode = argv[2];
        if (mode == "liblog") return child_liblog();
        if (mode == "liblog-assert") return child_liblog_assert();
        if (mode == "logger") return child_logger();
        if (mode == "jni-trace") return child_jni_trace();
        if (mode == "terminate") return child_terminate();
        if (mode == "dlopen-trace") return child_dlopen_trace();
        return 64;
    }
    check_netdb();
    check_liblog();
    check_logger();
    check_legacy_streams();
    check_threads();
    check_linker();
    check_system_paths();
    check_dlopen_trace();
    check_jni();
    if (g_failures != 0) {
        fprintf(stderr, "small native checks: %d of %d failed\n", g_failures, g_checks);
        return 1;
    }
    printf("small native checks: all %d passed\n", g_checks);
    return 0;
}
