// The engine's cookie protocol, Java side.
//
// The engine keeps its cookie jar in memory and expects the app's Java code to
// persist it (INFERRED from the old notes: no cookie file appears in a full
// `INTOXICATED_TRACE_PATHS=1` run). Intoxicated is that Java code. It hands the
// engine a `JNICookieProtocol$OnSetCookieHandler` to notify when a response
// sets cookies, and reads a domain's whole jar back out through
// `NativeSettingsInterface.nativeGetCookiesForDomain` when the Rust side
// (`crates/intoxicated-runtime/src/cookie_jar.rs`) wants to store it.
//
// A cookie jar is a live session credential. Nothing here logs, prints, traces
// or keeps a cookie value at any verbosity; the cookies array the handler is
// given is never read; and of the URL it is given only the host leaves this
// file, found without looking past the authority, since a query can carry a
// one-time sign-in ticket.

#include "jni_world.h"

#include <cstdio>
#include <cstring>
#include <memory>
#include <mutex>
#include <string>

namespace intoxicated {

jnivm::ENV* process_env();

namespace {

using java::LocalFrame;
using java::contained;
using java::report;
using java::to_jni;

constexpr const char* kHandlerClass = "com/roblox/universalapp/cookie/JNICookieProtocol$OnSetCookieHandler";
constexpr const char* kProtocolClass = "com/roblox/universalapp/cookie/JNICookieProtocol";
constexpr const char* kOnSetCookie = "([Ljava/lang/String;Ljava/lang/String;)V";

/// Where a Set-Cookie host goes. A plain store: the Rust side installs it
/// during start-up, before the engine has the HTTP thread that reads it.
void (*g_host_sink)(const char* host) = nullptr;

/// The host of `url`, by the rule the Rust side expects: after the scheme's
/// `://`, up to the first `/`, `?` or `#`; after the last `@` of that, if any;
/// without a trailing `:port` unless the authority has a `]` (an IPv6
/// literal keeps its port). Case is kept.
///
/// The scan never goes past the authority. A `://` can only end a scheme if it
/// comes before the first `/`, `?` or `#`, since it contains a `/`; one later
/// in the text is part of the path or the query and is not looked at.
std::string host_of(const char* url) {
    const char* start = url;
    const char* p = url;
    while (*p != '\0' && *p != '/' && *p != '?' && *p != '#') {
        ++p;
    }
    if (p > url && p[-1] == ':' && p[0] == '/' && p[1] == '/') {
        start = p + 2;
        p = start;
        while (*p != '\0' && *p != '/' && *p != '?' && *p != '#') {
            ++p;
        }
    }
    std::string authority(start, static_cast<size_t>(p - start));
    const size_t at = authority.rfind('@');
    if (at != std::string::npos) {
        authority.erase(0, at + 1);
    }
    if (authority.find(']') == std::string::npos) {
        const size_t colon = authority.rfind(':');
        if (colon != std::string::npos) {
            authority.erase(colon);
        }
    }
    return authority;
}

/// `JNICookieProtocol$OnSetCookieHandler`.
class OnSetCookieHandler final : public jnivm::Object {
public:
    /// `onSetCookie([Ljava/lang/String;Ljava/lang/String;)V`, on the engine's
    /// HTTP thread. Hands the sink the host and nothing else; never throws.
    void onSetCookie(jnivm::ENV*, std::shared_ptr<jnivm::Array<jnivm::String>> cookies,
                     std::shared_ptr<jnivm::String> url) {
        (void)cookies;
        void (*sink)(const char*) = g_host_sink;
        if (sink == nullptr || !url) {
            return;
        }
        try {
            const std::string host = host_of(url->c_str());
            if (!host.empty()) {
                sink(host.c_str());
            }
        } catch (...) {
        }
    }
};

/// The handler the engine was last given, kept alive for the process: the
/// engine stores the reference and may call it much later from another thread.
std::mutex g_handler_lock;
std::shared_ptr<OnSetCookieHandler> g_handler;

/// The text of a Java string the engine returned; null is empty.
std::string text_of_result(jstring s) {
    auto str = java::object_as<jnivm::String>(reinterpret_cast<jobject>(s));
    return str ? std::string(*str) : std::string();
}

} // namespace

/// Called by jni_framework.cpp's `register_framework_classes`, after the
/// framework classes.
void register_cookie_classes(jnivm::ENV* env) {
    env->GetClass<OnSetCookieHandler>(kHandlerClass);
    auto c = env->GetClass(kHandlerClass);
    c->HookInstanceFunction(env, "onSetCookie", &OnSetCookieHandler::onSetCookie);
    java::expect(c, java::Member::Method, "onSetCookie", kOnSetCookie);
}

} // namespace intoxicated

extern "C" {

/// Installs the receiver of Set-Cookie hosts, or clears it with null. The host
/// string is valid only for the call.
void intoxicated_cookies_set_host_sink(void (*sink)(const char* host)) {
    intoxicated::g_host_sink = sink;
}

/// `JNICookieProtocol.updateOnSetCookieHandler(handler)`, static native
/// `void (JNIEnv*, jclass, jobject)`, with a new handler that is retained
/// before the call (the engine may call it back during the call).
int intoxicated_cookies_register_handler(void* native, char* err, size_t err_len) {
    using namespace intoxicated;
    jnivm::ENV* env = native != nullptr ? process_env() : nullptr;
    if (env == nullptr) {
        report(err, err_len, "no JavaVM, or updateOnSetCookieHandler is not exported");
        return -1;
    }
    std::shared_ptr<OnSetCookieHandler> previous;
    const int rc = contained(err, err_len, [&]() -> int {
        LocalFrame frame(env, 8);
        auto handler = std::make_shared<OnSetCookieHandler>();
        {
            std::lock_guard<std::mutex> lock(g_handler_lock);
            previous = g_handler;
            g_handler = handler;
        }
        jclass protocol = java::find_class(env, kProtocolClass);
        auto object = static_cast<jobject>(to_jni(env, handler));
        using Update = void (*)(JNIEnv*, jclass, jobject);
        reinterpret_cast<Update>(native)(env->GetJNIEnv(), protocol, object);
        return 0;
    });
    if (rc != 0) {
        // The new handler is let go; the one the engine had before stays.
        std::lock_guard<std::mutex> lock(g_handler_lock);
        g_handler = previous;
    }
    return rc;
}

/// `nativeGetCookiesForDomain(domain)` on `class_name`, static native
/// `jstring (JNIEnv*, jclass, jstring)`: the engine's whole jar for `domain`
/// (null as ""), copied into `out` with a NUL. `*needed` gets the jar's byte
/// length before the size check. A jar that does not fit is refused naming
/// both sizes, never shortened; the message carries sizes only.
int intoxicated_cookies_get_for_domain(void* native, const char* class_name, const char* domain, char* out,
                                       size_t out_len, size_t* needed, char* err, size_t err_len) {
    using namespace intoxicated;
    jnivm::ENV* env = native != nullptr && class_name != nullptr ? process_env() : nullptr;
    if (env == nullptr) {
        report(err, err_len, "no JavaVM, or the cookie native is not exported");
        return -1;
    }
    return contained(err, err_len, [&]() -> int {
        LocalFrame frame(env, 8);
        jclass cls = java::find_class(env, class_name);
        auto jdomain = static_cast<jstring>(to_jni(env, java::jstr(domain != nullptr ? domain : "")));
        using Get = jstring (*)(JNIEnv*, jclass, jstring);
        const std::string jar = text_of_result(reinterpret_cast<Get>(native)(env->GetJNIEnv(), cls, jdomain));
        if (needed != nullptr) {
            *needed = jar.size();
        }
        if (out == nullptr || jar.size() >= out_len) {
            char message[96];
            snprintf(message, sizeof message, "the jar for this domain is %zu bytes and the buffer is %zu",
                     jar.size(), out == nullptr ? static_cast<size_t>(0) : out_len);
            report(err, err_len, message);
            return -1;
        }
        std::memcpy(out, jar.data(), jar.size());
        out[jar.size()] = '\0';
        return 0;
    });
}

} // extern "C"
