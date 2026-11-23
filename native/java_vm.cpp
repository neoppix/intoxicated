// The one JavaVM of the process: libjnivm standing in for ART.
//
// `intoxicated_jni_create_vm` makes it, registers Intoxicated's Java world into
// it and hands back the `JavaVM*` the engine is given; `intoxicated_jni_call_onload`
// calls the engine's `JNI_OnLoad` with it, containing anything thrown; and
// `intoxicated::process_env()` is how every other native file reaches libjnivm
// from whichever thread it is on, the engine's own included. The Rust side
// (`crates/intoxicated-linker-sys/src/jni.rs`) owns when these run.
//
// The VM is never destroyed: engine threads hold its references until the
// process exits.
//
// The engine is handed a JavaVM of its own: a copy of libjnivm's invocation
// table whose five thread and VM entries (DestroyJavaVM, AttachCurrentThread,
// AttachCurrentThreadAsDaemon, DetachCurrentThread, GetEnv) forward to
// libjnivm's own JavaVM, whatever JavaVM pointer they are called with.
// Handing the engine libjnivm's own JavaVM instead was tried, twice, on engine
// 2.748: its JNI_OnLoad died at once in libjnivm's "Failed to get reference to
// jnivm::VM", which only those entries raise, when the JavaVM they are given
// does not lead back to the VM. Through the copy every call arrives with the
// handed-out pointer and works (traced). Why libjnivm's own JavaVM fails was
// not established. With `INTOXICATED_JNI_TRACE` in the environment at
// creation, each forwarded call also prints one line saying whether it came
// through the handed-out pointer.

#include <jnivm.h>

#include <execinfo.h>
#include <unistd.h>

#include <atomic>
#include <cstddef>
#include <cstdio>
#include <cstdlib>
#include <exception>

#include "os_compat.h"

// The Java world the engine sees (jni_framework.cpp, activity.cpp,
// jni_params.cpp), registered in this order.
namespace intoxicated {
namespace java {
void register_framework_classes(jnivm::ENV* env);
void register_activity_classes(jnivm::ENV* env);
void register_param_classes(jnivm::ENV* env);
} // namespace java
} // namespace intoxicated

namespace {

/// Set once, by `intoxicated_jni_create_vm`, and never cleared.
std::atomic<jnivm::VM*> g_vm{nullptr};
/// What the engine was given: the forwarding copy below.
JavaVM* g_handed_out = nullptr;

// ------------------------------------------------------------ the engine's JavaVM

JavaVM* g_own = nullptr;
JNIInvokeInterface g_forward_table;
JavaVM g_forward_vm;
bool g_trace = false;

void say(const char* entry, JavaVM* vm) {
    if (g_trace) {
        fprintf(stderr, "[jni] %s(vm=%p) %s\n", entry, static_cast<void*>(vm),
                vm == &g_forward_vm ? "[ours]" : "[NOT ours]");
    }
}

jint forward_destroy(JavaVM* vm) {
    say("DestroyJavaVM", vm);
    return g_own->functions->DestroyJavaVM(g_own);
}

jint forward_attach(JavaVM* vm, JNIEnv** env, void* args) {
    say("AttachCurrentThread", vm);
    return g_own->functions->AttachCurrentThread(g_own, env, args);
}

jint forward_detach(JavaVM* vm) {
    say("DetachCurrentThread", vm);
    return g_own->functions->DetachCurrentThread(g_own);
}

jint forward_get_env(JavaVM* vm, void** env, jint version) {
    say("GetEnv", vm);
    return g_own->functions->GetEnv(g_own, env, version);
}

jint forward_attach_daemon(JavaVM* vm, JNIEnv** env, void* args) {
    say("AttachCurrentThreadAsDaemon", vm);
    return g_own->functions->AttachCurrentThreadAsDaemon(g_own, env, args);
}

/// The copy keeps libjnivm's `reserved0`, the pointer libjnivm finds its VM
/// through, so every libjnivm path that starts from this JavaVM still works.
JavaVM* forwarding_copy_of(JavaVM* own) {
    g_own = own;
    g_forward_table = *own->functions;
    g_forward_table.DestroyJavaVM = forward_destroy;
    g_forward_table.AttachCurrentThread = forward_attach;
    g_forward_table.DetachCurrentThread = forward_detach;
    g_forward_table.GetEnv = forward_get_env;
    g_forward_table.AttachCurrentThreadAsDaemon = forward_attach_daemon;
    g_forward_vm.functions = &g_forward_table;
    return &g_forward_vm;
}

// ------------------------------------------------------------ terminate

/// A C++ exception nobody caught, on any thread (a libjnivm exception escaping
/// an engine thread, typically): what it was, where, and optionally the Java
/// surface reached so far, then exit 70 without running atexit handlers.
[[noreturn]] void on_terminate() {
    fprintf(stderr, "\n*** uncaught C++ exception on thread %ld ***\n", intoxicated_gettid());
    if (std::exception_ptr current = std::current_exception()) {
        try {
            std::rethrow_exception(current);
        } catch (const std::exception& e) {
            fprintf(stderr, "    what(): %s\n", e.what());
        } catch (...) {
            fprintf(stderr, "    non-standard exception\n");
        }
    }
    void* frames[32];
    const int n = backtrace(frames, 32);
    fprintf(stderr, "    %d frames:\n", n);
    fflush(stderr);
    backtrace_symbols_fd(frames, n, STDERR_FILENO);
    const char* dump = std::getenv("INTOXICATED_JNI_DUMP");
    jnivm::VM* vm = g_vm.load(std::memory_order_acquire);
    if (dump != nullptr && *dump != '\0' && vm != nullptr) {
#ifdef JNI_DEBUG
        try {
            vm->GenerateClassDump(dump);
            fprintf(stderr, "    Java surface Roblox reached for -> %s\n", dump);
        } catch (...) {
            fprintf(stderr, "    class dump failed\n");
        }
#endif
    }
    fflush(stderr);
    _exit(70);
}

} // namespace

namespace intoxicated {

/// This thread's libjnivm ENV, attaching the thread first if the VM has never
/// seen it. libjnivm's AttachCurrentThread takes the VM's lock and creates the
/// ENV on first use, so this is safe on any thread, the engine's own included.
/// Null before the VM exists or if the attach fails.
jnivm::ENV* process_env() {
    jnivm::VM* vm = g_vm.load(std::memory_order_acquire);
    if (vm == nullptr) {
        return nullptr;
    }
    try {
        JavaVM* own = vm->GetJavaVM();
        JNIEnv* jni = nullptr;
        if (own->functions->AttachCurrentThread(own, &jni, nullptr) != JNI_OK || jni == nullptr) {
            return nullptr;
        }
        return jnivm::ENV::FromJNIEnv(jni);
    } catch (...) {
        return nullptr;
    }
}

} // namespace intoxicated

extern "C" {

/// The process JavaVM, created and populated; null when it already exists.
/// The check is not atomic with the creation: the Rust side calls this once,
/// from one thread. A registration that throws is not survivable (the engine
/// would meet a half-built Java world), so it ends in the terminate report.
void* intoxicated_jni_create_vm(void) {
    if (g_vm.load(std::memory_order_acquire) != nullptr) {
        return nullptr;
    }
    std::set_terminate(on_terminate);
    try {
        jnivm::VM* vm = new jnivm::VM();
        g_vm.store(vm, std::memory_order_release);
        // The order is load-bearing: the framework registration binds the
        // shared classes every later descriptor names.
        jnivm::ENV* env = vm->GetEnv().get();
        intoxicated::java::register_framework_classes(env);
        intoxicated::java::register_activity_classes(env);
        intoxicated::java::register_param_classes(env);

        g_trace = std::getenv("INTOXICATED_JNI_TRACE") != nullptr;
        g_handed_out = forwarding_copy_of(vm->GetJavaVM());
        fprintf(stderr, "[jni] JavaVM=%p functions=%p reserved0=%p (expect %p)\n", static_cast<void*>(g_handed_out),
                static_cast<const void*>(g_handed_out->functions), g_handed_out->functions->reserved0,
                static_cast<void*>(vm));
        return g_handed_out;
    } catch (...) {
        std::terminate();
    }
}

/// The engine's `JNI_OnLoad(JavaVM*, void*)`, called with the handed-out VM and
/// a null reserved pointer; its result comes back unchanged. -1 without a VM
/// or a function, -2 for a thrown std::exception (its text in `err`), -3 for
/// anything else thrown.
int intoxicated_jni_call_onload(void* on_load, char* err, size_t err_len) {
    if (on_load == nullptr || g_vm.load(std::memory_order_acquire) == nullptr) {
        return -1;
    }
    using OnLoad = jint (*)(JavaVM*, void*);
    try {
        return reinterpret_cast<OnLoad>(on_load)(g_handed_out, nullptr);
    } catch (const std::exception& e) {
        if (err != nullptr && err_len > 0) {
            snprintf(err, err_len, "%s", e.what());
        }
        return -2;
    } catch (...) {
        if (err != nullptr && err_len > 0) {
            snprintf(err, err_len, "non-standard C++ exception");
        }
        return -3;
    }
}

/// libjnivm's dump of every class and member reached so far, written to
/// `path`. 0 once written (libjnivm does not report a path it could not open),
/// -1 without a VM, -2 when libjnivm was built without JNI_DEBUG, -3 when the
/// dump threw or there is no path.
int intoxicated_jni_dump_classes(const char* path) {
    jnivm::VM* vm = g_vm.load(std::memory_order_acquire);
    if (vm == nullptr) {
        return -1;
    }
#ifdef JNI_DEBUG
    if (path == nullptr) {
        return -3;
    }
    try {
        vm->GenerateClassDump(path);
        return 0;
    } catch (...) {
        return -3;
    }
#else
    (void)path;
    return -2;
#endif
}

} // extern "C"
