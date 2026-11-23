// Android platform classes the engine looks up by name, registered so that
// the lookups land on real classes, with the few members that have a truthful
// answer here.
//
// libjnivm invents a class, and placeholder members answering zero or null,
// for any lookup it cannot satisfy. The rule kept here: hook a member only
// where there is a true answer, and leave the rest unhooked, so that a call
// to one shows up as an unresolved symbol in a trace build
// (INTOXICATED_JNI_TRACE=1 in cargo's environment) instead of being papered
// over.
//
// Not registered on purpose, until a run shows the engine asking:
// PackageManager, PackageInfo, Signature and SigningInfo (if that chain is
// ever answered, the certificate is read from the supplied APK at call time,
// never written down here), the five driving methods of AppRtcDeviceWrapper,
// and the memstorage `Connection` constructor, which takes the engine's own
// native handle.
//
// Each Java class has its own C++ type: libjnivm keys a class binding on the
// C++ type, and binding a second name to a type moves the binding and changes
// the descriptor of every later hook that mentions it. Receivers that may be
// objects some other file made (an Application) are taken as `jnivm::Object*`,
// since a typed receiver of the wrong type raises inside libjnivm.

#include "jni_world.h"

#include <arpa/inet.h>
#include <ifaddrs.h>
#include <net/if.h>
#include <netinet/in.h>
#include <sys/socket.h>

#include <cstdio>
#include <memory>
#include <string>

#include "jni_values.h"

namespace intoxicated {
namespace {

using java::Member;
using java::expect;
using jnivm::Class;
using jnivm::ENV;
using jnivm::Object;
using jnivm::String;

constexpr const char* kApplication = "android/app/Application";
constexpr const char* kActivityThread = "android/app/ActivityThread";
constexpr const char* kNetworkUtils = "com/roblox/engine/jni/util/NetworkUtils";
constexpr const char* kAppRtcDeviceWrapper = "com/roblox/audio/AppRtcDeviceWrapper";
constexpr const char* kConnection = "com/roblox/engine/jni/memstorage/Connection";
constexpr const char* kBuildInfo = "org/webrtc/voiceengine/BuildInfo";
constexpr const char* kWebRtcAudioTrack = "org/webrtc/voiceengine/WebRtcAudioTrack";

/// The Android package name, which is what `Application.getProcessName`
/// answers for the app's main process.
constexpr const char* kProcessName = "com.roblox.client";

std::shared_ptr<String> S(const char* text) {
    return std::make_shared<String>(std::string(text != nullptr ? text : ""));
}

/// `android.app.Application`. jni_framework.cpp has already hooked
/// `getSharedPreferences` and `checkSelfPermission` on this class by name;
/// binding the type here gets the existing class, so those stay.
class Application final : public Object {};

/// `android.app.ActivityThread`.
class ActivityThread final : public Object {
public:
    /// A new object on every call, as each call made one in the original
    /// measurement; nothing here tells them apart.
    static std::shared_ptr<ActivityThread> currentActivityThread(ENV*, Class*) {
        return std::make_shared<ActivityThread>();
    }

    /// One Application for the process, made on first use (a function-local
    /// static, so thread-safe), the same object from any ActivityThread.
    static std::shared_ptr<Application> application() {
        static const std::shared_ptr<Application> app = std::make_shared<Application>();
        return app;
    }
};

/// `com.roblox.engine.jni.util.NetworkUtils`.
class NetworkUtils final : public Object {
public:
    /// The engine's spelling. Every IPv4 address of an interface without the
    /// loopback flag, in the order getifaddrs lists them (an interface that is
    /// down is not skipped), dotted, joined by commas. Nothing is sent on the
    /// network. This tells the engine the LAN address, as the Android client
    /// does.
    static std::shared_ptr<String> getPublicIPv4Addresseses(ENV*, Class*) {
        struct ifaddrs* list = nullptr;
        if (getifaddrs(&list) != 0) {
            fprintf(stderr, "[roblox] NetworkUtils: getifaddrs failed; reporting no addresses\n");
            return S("");
        }
        std::string joined;
        for (const struct ifaddrs* i = list; i != nullptr; i = i->ifa_next) {
            if (i->ifa_addr == nullptr || i->ifa_addr->sa_family != AF_INET || (i->ifa_flags & IFF_LOOPBACK) != 0) {
                continue;
            }
            char text[INET_ADDRSTRLEN];
            const auto* in = reinterpret_cast<const struct sockaddr_in*>(i->ifa_addr);
            if (inet_ntop(AF_INET, &in->sin_addr, text, sizeof text) == nullptr) {
                continue;
            }
            if (!joined.empty()) {
                joined += ',';
            }
            joined += text;
        }
        freeifaddrs(list);
        return std::make_shared<String>(joined);
    }
};

/// WebRTC's Android audio-device wrapper, used for voice chat routing. There is
/// no Android audio routing here at all: audio goes out through FMOD's AAudio
/// output to one OSS node (`native/aaudio_oss.cpp`), and there is no
/// `AudioManager`, no communication mode and no selectable device behind any
/// of this. So the one answer is `isValid() == false`: there is no valid
/// device to wrap.
class AppRtcDeviceWrapper final : public Object {};

/// `com.roblox.engine.jni.memstorage.Connection`: the class exists, nothing
/// on it is answered.
class Connection final : public Object {};

/// WebRTC's `BuildInfo`, static getters only. Every answer is a field of
/// `device_record()` (jni_values.cpp), the device record of this process's
/// profile, so `INTOXICATED_DEVICE_PROFILE` decides them; a null field is "".
class BuildInfo final : public Object {};

/// WebRTC's `WebRtcAudioTrack`: the class exists, nothing on it is answered.
class WebRtcAudioTrack final : public Object {};

void register_application(ENV* env) {
    env->GetClass<Application>(kApplication);
    auto c = env->GetClass(kApplication);
    c->HookInstanceFunction(env, "getProcessName", [](ENV*, Object*) { return S(kProcessName); });
    expect(c, Member::Method, "getProcessName", "()Ljava/lang/String;");
}

void register_activity_thread(ENV* env) {
    env->GetClass<ActivityThread>(kActivityThread);
    auto c = env->GetClass(kActivityThread);
    c->Hook(env, "currentActivityThread", &ActivityThread::currentActivityThread);
    c->HookInstanceFunction(env, "getApplication", [](ENV*, Object*) { return ActivityThread::application(); });
    expect(c, Member::StaticMethod, "currentActivityThread", "()Landroid/app/ActivityThread;");
    expect(c, Member::Method, "getApplication", "()Landroid/app/Application;");
}

void register_network_utils(ENV* env) {
    env->GetClass<NetworkUtils>(kNetworkUtils);
    auto c = env->GetClass(kNetworkUtils);
    c->Hook(env, "getPublicIPv4Addresseses", &NetworkUtils::getPublicIPv4Addresseses);
    expect(c, Member::StaticMethod, "getPublicIPv4Addresseses", "()Ljava/lang/String;");
}

void register_webrtc(ENV* env) {
    env->GetClass<AppRtcDeviceWrapper>(kAppRtcDeviceWrapper);
    auto wrapper = env->GetClass(kAppRtcDeviceWrapper);
    wrapper->HookInstanceFunction(env, "isValid", [](ENV*, Object*) -> jboolean { return JNI_FALSE; });
    expect(wrapper, Member::Method, "isValid", "()Z");

    env->GetClass<BuildInfo>(kBuildInfo);
    auto c = env->GetClass(kBuildInfo);
#define BUILD_INFO(getter, field)                                              \
    c->Hook(env, getter, [](ENV*, Class*) -> std::shared_ptr<String> {        \
        return S(device_record().field);                                       \
    });                                                                        \
    expect(c, Member::StaticMethod, getter, "()Ljava/lang/String;")
    BUILD_INFO("getBrand", brand);
    BUILD_INFO("getDeviceManufacturer", manufacturer);
    BUILD_INFO("getDeviceModel", model);
    BUILD_INFO("getDevice", device);
    BUILD_INFO("getDeviceProduct", product);
    BUILD_INFO("getAndroidBuildId", build_id);
    BUILD_INFO("getBuildType", build_type);
    BUILD_INFO("getBuildRelease", build_release);
    BUILD_INFO("getSdkVersion", sdk_version);
#undef BUILD_INFO

    env->GetClass<WebRtcAudioTrack>(kWebRtcAudioTrack);
}

} // namespace

/// Called once, from jni_framework.cpp's `register_framework_classes`, after
/// the framework classes, cookies, FMOD, the MessageBus, preferences and local
/// storage, and before the battery classes.
void register_platform_classes(ENV* env) {
    register_application(env);
    register_activity_thread(env);
    register_network_utils(env);
    env->GetClass<Connection>(kConnection);
    register_webrtc(env);
}

} // namespace intoxicated
