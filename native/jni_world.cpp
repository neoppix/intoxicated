// Helpers shared by the three files that build the engine's Java world.
// See jni_world.h.

#include "jni_world.h"

#include <cstdio>
#include <cstring>

namespace intoxicated {
namespace java {

void declare_shared_classes(ENV* env) {
    env->GetClass<Activity>(kActivity);
    env->GetClass<Surface>(kSurface);
    env->GetClass<AssetManager>(kAssetManager);
    env->GetClass<Point>(kPoint);
    env->GetClass<Insets>(kInsets);
    env->GetClass<DisplayMetrics>(kDisplayMetrics);
    env->GetClass<Configuration>(kConfiguration);
    env->GetClass<DeviceStaticParams>(kDeviceStaticParams);
    env->GetClass<NativeTextBoxInfo>(kNativeTextBoxInfo);
}

std::shared_ptr<Object> instance_of(ENV* env, jclass cls) {
    auto object = std::make_shared<Object>();
    object->clazz = jnivm::JNITypes<std::shared_ptr<Class>>::JNICast(env, cls);
    return object;
}

jclass find_class(ENV* env, const char* name) {
    return env->GetJNIEnv()->FindClass(name);
}

bool has_hooked(const std::shared_ptr<Class>& cls, Member kind, const char* name, const char* descriptor) {
    if (!cls) {
        return false;
    }
    std::lock_guard<std::mutex> lock(cls->mtx);
    if (kind == Member::Method || kind == Member::StaticMethod) {
        const bool want_static = kind == Member::StaticMethod;
        for (const auto& m : cls->methods) {
            if (m->_static == want_static && m->name == name && m->signature == descriptor && m->nativehandle) {
                return true;
            }
        }
        return false;
    }
    const bool want_static = kind == Member::StaticField;
    for (const auto& f : cls->fields) {
        if (f->_static == want_static && f->name == name && f->type == descriptor && f->getnativehandle) {
            return true;
        }
    }
    return false;
}

void expect(const std::shared_ptr<Class>& cls, Member kind, const char* name, const char* descriptor) {
    if (!has_hooked(cls, kind, name, descriptor)) {
        fprintf(stderr, "[classes] %s.%s %s did not bind; the engine's lookup will find a placeholder\n",
                cls ? cls->nativeprefix.c_str() : "(no class)", name, descriptor);
    }
}

void report(char* err, size_t err_len, const char* message) {
    if (err && err_len > 0) {
        snprintf(err, err_len, "%s", message ? message : "");
    }
}

} // namespace java
} // namespace intoxicated

/// 1 when `class_name` answers the member with exactly that descriptor (kind:
/// 0 method, 1 static method, 2 field, 3 static field), 0 when it does not,
/// -1 before there is a JavaVM. Looks only: an unknown class is not created.
/// What the runtime's Java-world test checks every used member with.
extern "C" int intoxicated_java_member_answered(const char* class_name, int kind, const char* name,
                                                const char* descriptor) {
    using namespace intoxicated::java;
    ENV* env = intoxicated::process_env();
    if (!env) {
        return -1;
    }
    if (!class_name || !name || !descriptor || kind < 0 || kind > 3) {
        return 0;
    }
    std::shared_ptr<Class> cls;
    {
        auto* vm = env->GetVM();
        std::lock_guard<std::mutex> lock(vm->mtx);
        auto it = vm->classes.find(class_name);
        if (it != vm->classes.end()) {
            cls = it->second;
        }
    }
    return has_hooked(cls, static_cast<Member>(kind), name, descriptor) ? 1 : 0;
}
