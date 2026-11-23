// `getaddrinfo` and `freeaddrinfo` as the engine sees them: it was compiled
// against bionic, so its hints, address families and sockaddrs are Linux's,
// and the answer comes from FreeBSD's resolver.
//
// What differs, measured against /usr/include and bionic's headers on x86-64:
//
// - `struct addrinfo` itself does not: bionic and FreeBSD both put ai_flags,
//   ai_family, ai_socktype, ai_protocol at 0/4/8/12, ai_addrlen at 16,
//   ai_canonname at 24, ai_addr at 32 and ai_next at 40 (48 bytes; glibc is
//   the one that swaps ai_canonname and ai_addr). The static_asserts below
//   pin both. The list handed back is still always one this file built, so
//   that freeaddrinfo frees exactly what was allocated here.
// - Address families: Linux AF_INET6 is 10, FreeBSD's 28 (FreeBSD's 10 is
//   AF_CCITT). AF_UNSPEC, AF_UNIX, AF_INET and the SOCK_* and IPPROTO_*
//   numbers agree.
// - Sockaddrs: Linux leads with a 2-byte family, FreeBSD with a 1-byte sa_len
//   and a 1-byte family. freebsd_abi.c translates them.
// - Hint flags: the AI_* values agree, but FreeBSD answers EAI_BADFLAGS for
//   anything outside PASSIVE, CANONNAME, NUMERICHOST, NUMERICSERV, ALL,
//   ADDRCONFIG and V4MAPPED, which includes AI_V4MAPPED_CFG and so bionic's
//   AI_DEFAULT. It also answers EAI_BADHINTS for hints whose pointer and
//   length fields are not zero; only the four integers are passed on.
// - EAI_* values agree from 1 to 14. 1 to 11 and 14 pass through; FreeBSD's
//   EAI_BADHINTS (12) and EAI_PROTOCOL (13), and anything else, arrive as
//   EAI_FAIL (4).

#include "freebsd_abi.h"

#include <netdb.h>
#include <sys/socket.h>

#include <cstddef>
#include <cstdlib>
#include <cstring>

#include "symbol_table.h"

/// bionic's `struct addrinfo` on x86-64.
struct IntoxicatedBionicAddrinfo {
    int ai_flags;
    int ai_family;
    int ai_socktype;
    int ai_protocol;
    socklen_t ai_addrlen;
    char* ai_canonname;
    struct sockaddr* ai_addr;
    IntoxicatedBionicAddrinfo* ai_next;
};
using BionicAddrinfo = IntoxicatedBionicAddrinfo;

namespace {

static_assert(offsetof(BionicAddrinfo, ai_addrlen) == 16, "bionic ai_addrlen @16");
static_assert(offsetof(BionicAddrinfo, ai_canonname) == 24, "bionic ai_canonname @24");
static_assert(offsetof(BionicAddrinfo, ai_addr) == 32, "bionic ai_addr @32");
static_assert(offsetof(BionicAddrinfo, ai_next) == 40, "bionic ai_next @40");
static_assert(sizeof(BionicAddrinfo) == 48, "bionic addrinfo is 48 bytes");
static_assert(offsetof(struct addrinfo, ai_addrlen) == 16, "FreeBSD ai_addrlen @16");
static_assert(offsetof(struct addrinfo, ai_canonname) == 24, "FreeBSD ai_canonname @24");
static_assert(offsetof(struct addrinfo, ai_addr) == 32, "FreeBSD ai_addr @32");
static_assert(offsetof(struct addrinfo, ai_next) == 40, "FreeBSD ai_next @40");
static_assert(sizeof(struct addrinfo) == 48, "FreeBSD addrinfo is 48 bytes");

// bionic's AI_* bits (libc/include/netdb.h).
constexpr int kBionicPassive = 0x1;
constexpr int kBionicCanonName = 0x2;
constexpr int kBionicNumericHost = 0x4;
constexpr int kBionicNumericServ = 0x8;
constexpr int kBionicAll = 0x100;
constexpr int kBionicV4MappedCfg = 0x200;
constexpr int kBionicAddrConfig = 0x400;
constexpr int kBionicV4Mapped = 0x800;

// bionic's EAI_* numbers that matter here.
constexpr int kBionicEaiFail = 4;
constexpr int kBionicEaiMemory = 6;

/// The hint flags FreeBSD accepts, from bionic's: the six it shares, both
/// V4MAPPED spellings as FreeBSD's one, and nothing else.
int host_flags(int lx) {
    int out = 0;
    if (lx & kBionicPassive) out |= AI_PASSIVE;
    if (lx & kBionicCanonName) out |= AI_CANONNAME;
    if (lx & kBionicNumericHost) out |= AI_NUMERICHOST;
    if (lx & kBionicNumericServ) out |= AI_NUMERICSERV;
    if (lx & kBionicAll) out |= AI_ALL;
    if (lx & kBionicAddrConfig) out |= AI_ADDRCONFIG;
    if (lx & (kBionicV4Mapped | kBionicV4MappedCfg)) out |= AI_V4MAPPED;
    return out;
}

/// A FreeBSD EAI_* in bionic's numbering.
int bionic_error(int rc) {
    if ((rc >= 1 && rc <= 11) || rc == 14) {
        return rc;
    }
    return kBionicEaiFail;
}

void free_list(BionicAddrinfo* p) {
    while (p != nullptr) {
        BionicAddrinfo* next = p->ai_next;
        std::free(p->ai_addr);
        std::free(p->ai_canonname);
        std::free(p);
        p = next;
    }
}

} // namespace

extern "C" {

int intoxicated_getaddrinfo(const char* node, const char* service, const BionicAddrinfo* hints,
                            BionicAddrinfo** res) {
    struct addrinfo host_hints;
    const struct addrinfo* hp = nullptr;
    if (hints != nullptr) {
        std::memset(&host_hints, 0, sizeof host_hints);
        host_hints.ai_flags = host_flags(hints->ai_flags);
        // SOCK_* and IPPROTO_* agree everywhere. AF_* agree between bionic and
        // glibc, but not with FreeBSD: bionic's AF_INET6 is 10, FreeBSD's 28, and
        // FreeBSD 10 is AF_CCITT -- a v6-only lookup came back EAI_FAMILY.
        host_hints.ai_family = intoxicated_fbsd_af_from_linux(hints->ai_family);
        if (host_hints.ai_family < 0) {
            if (res) *res = nullptr;
            return 5; // bionic EAI_FAMILY
        }
        host_hints.ai_socktype = hints->ai_socktype;
        host_hints.ai_protocol = hints->ai_protocol;
        hp = &host_hints;
    }

    // bionic/glibc treat an empty service string ("") as "no service" (port 0)
    // and resolve the node successfully; FreeBSD's getaddrinfo instead returns
    // EAI_SERVICE. The engine's RUPP / RakPeer path resolves its own machine
    // address with getaddrinfo(gethostname(), "") -- confirmed live:
    // getaddrinfo(host="pascal", serv="") = 9 while serv=NULL = 0 -- and that
    // failure left the trust token with no local address, so the game server
    // rejected the session at its ~60 s grace with the 304 AndroidAnticheatKick.
    // Normalise to NULL so the call behaves as it does on Android.
    if (service && service[0] == '\0') {
        service = nullptr;
    }

    struct addrinfo* found = nullptr;
    const int rc = ::getaddrinfo(node, service, hp, &found);
    if (rc != 0) {
        if (res) *res = nullptr;
        return bionic_error(rc);
    }

    // One node per host node, in the host's order, every block allocated here.
    BionicAddrinfo* head = nullptr;
    BionicAddrinfo** tail = &head;
    bool out_of_memory = false;
    for (const struct addrinfo* g = found; g != nullptr && !out_of_memory; g = g->ai_next) {
        auto* b = static_cast<BionicAddrinfo*>(std::calloc(1, sizeof(BionicAddrinfo)));
        if (b == nullptr) {
            out_of_memory = true;
            break;
        }
        *tail = b;
        tail = &b->ai_next;
        b->ai_flags = 0;
        b->ai_socktype = g->ai_socktype;
        b->ai_protocol = g->ai_protocol;
        // FreeBSD's sockaddrs lead with a 1-byte sa_len and a 1-byte family, and
        // number AF_INET6 28. Copied as they were, the engine read an IPv4
        // result's family as 0x0210 and a v6 one as a family it has no name
        // for -- every address it was handed was unusable. Translate to the
        // Linux layout the engine was compiled against.
        b->ai_family = intoxicated_fbsd_af_to_linux(g->ai_family);
        b->ai_addrlen = 0;
        if (g->ai_addr && g->ai_addrlen > 0) {
            unsigned char tmp[sizeof(struct sockaddr_storage)];
            size_t n = intoxicated_fbsd_sockaddr_to_linux(g->ai_addr, tmp, sizeof tmp);
            if (n > 0) {
                b->ai_addr = static_cast<struct sockaddr*>(std::malloc(n));
                if (b->ai_addr) {
                    std::memcpy(b->ai_addr, tmp, n);
                    b->ai_addrlen = static_cast<socklen_t>(n);
                } else {
                    out_of_memory = true;
                }
            }
        }
        if (g->ai_canonname != nullptr) {
            b->ai_canonname = strdup(g->ai_canonname);
            if (b->ai_canonname == nullptr) {
                out_of_memory = true;
            }
        }
    }
    ::freeaddrinfo(found);

    if (out_of_memory) {
        free_list(head);
        if (res) *res = nullptr;
        return kBionicEaiMemory;
    }
    if (res == nullptr) {
        free_list(head);
        return 0;
    }
    *res = head;
    return 0;
}

/// Frees a list `intoxicated_getaddrinfo` returned, and nothing else: every
/// node, its address and its canonical name. Null is a no-op.
void intoxicated_freeaddrinfo(BionicAddrinfo* list) {
    free_list(list);
}

} // extern "C"

namespace {

const IntoxicatedSymbol kNetdb[] = {
    {"getaddrinfo", reinterpret_cast<void*>(&intoxicated_getaddrinfo)},
    {"freeaddrinfo", reinterpret_cast<void*>(&intoxicated_freeaddrinfo)},
};

} // namespace

extern "C" const IntoxicatedSymbol* intoxicated_netdb_symbols(size_t* count) {
    if (count != nullptr) {
        *count = intoxicated_table_size(kNetdb);
    }
    return kNetdb;
}
