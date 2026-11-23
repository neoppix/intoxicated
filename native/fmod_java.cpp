// The Java half of FMOD's Android output, as the engine reaches it through
// libjnivm.
//
// org.fmod.FMOD holds the three static predicates FMOD asks before it touches
// any native audio library. supportsAAudio() is the real gate: true sends FMOD
// to dlopen("libaaudio.so") (aaudio_oss.cpp), false sends it to the Java
// org.fmod.AudioDevice path below. The answer is a real probe of the playback
// node streams will use (oss_device.cpp), cached for the process, because once
// FMOD has been told yes a refused output stream makes it give up on audio for
// good rather than fall back.
//
// org.fmod.AudioDevice is that fallback: init(channels, rate, frames, buffers),
// write(byte[], length) of interleaved S16, close(). write blocks until the
// device has room, as AudioTrack.write does in blocking mode, because FMOD's
// mixer thread uses that block as its clock. It plays to the same OSS node as
// AAudio would, so it only helps when the probe failed for a reason that a
// plain S16 stream at FMOD's own rate does not share.
//
// org.fmod.MediaCodec is looked up by name and nothing on it has been seen
// called; it is registered so the lookup lands on a real class.
#include "oss_device.h"

#include <jnivm.h>

#include <atomic>
#include <cerrno>
#include <cstdio>
#include <cstring>
#include <memory>
#include <mutex>
#include <string>

#include <sys/ioctl.h>
#include <sys/soundcard.h>
#include <unistd.h>

namespace intoxicated {

using jnivm::Class;
using jnivm::ENV;
using jnivm::Object;

namespace {

class FmodGate : public Object {
public:
    static jboolean checkInit(ENV*, Class*) { return JNI_TRUE; }
    static jboolean supportsLowLatency(ENV*, Class*) { return JNI_FALSE; }
    static jboolean supportsAAudio(ENV*, Class*) {
        const bool ok = intoxicated::audio::output_available();
        static std::once_flag once;
        std::call_once(once, [ok] {
            fprintf(stderr, "[audio] org.fmod.FMOD.supportsAAudio() -> %s\n", ok ? "true" : "false");
        });
        return ok ? JNI_TRUE : JNI_FALSE;
    }

    static void Register(ENV* env) {
        env->GetClass<FmodGate>("org/fmod/FMOD");
        auto c = env->GetClass("org/fmod/FMOD");
        c->Hook(env, "checkInit", &FmodGate::checkInit);
        c->Hook(env, "supportsLowLatency", &FmodGate::supportsLowLatency);
        c->Hook(env, "supportsAAudio", &FmodGate::supportsAAudio);
    }
};

class FmodAudioDevice : public Object {
public:
    // `io_m` is held across a blocking write and by close; close first halts
    // the device so a write in progress returns promptly instead of making
    // close wait out a full buffer.
    std::mutex io_m;
    std::atomic<int> fd{-1};
    std::atomic<bool> closing{false};

    static std::shared_ptr<FmodAudioDevice> ctor(ENV*, Class*) { return std::make_shared<FmodAudioDevice>(); }

    jboolean init(ENV*, jint channels, jint rate, jint frames, jint buffers) {
        std::lock_guard<std::mutex> lock(io_m);
        if (fd.load() >= 0) return JNI_TRUE;
        const std::string path = intoxicated::audio::device_path();
        intoxicated::audio::Negotiated n;
        std::string why;
        if (!intoxicated::audio::open_output_exact_s16(path, channels, rate, frames, buffers, &n, &why)) {
            fprintf(stderr, "[audio] org.fmod.AudioDevice.init(%d, %d, %d, %d) -> false: %s\n", channels, rate,
                    frames, buffers, why.c_str());
            return JNI_FALSE;
        }
        fd.store(n.fd);
        closing.store(false);
        fprintf(stderr, "[audio] org.fmod.AudioDevice.init(%d, %d, %d, %d) -> true on %s, %d x %d frames\n",
                channels, rate, frames, buffers, path.c_str(), n.fragment_count, n.burst_frames);
        return JNI_TRUE;
    }

    void write(ENV*, std::shared_ptr<jnivm::Array<jbyte>> data, jint length) {
        if (!data || length <= 0 || closing.load()) return;
        if (length > data->getSize()) length = data->getSize();
        std::lock_guard<std::mutex> lock(io_m);
        const int f = fd.load();
        if (f < 0) return;
        const char* p = reinterpret_cast<const char*>(data->getArray());
        size_t left = static_cast<size_t>(length);
        while (left > 0 && !closing.load()) {
            const ssize_t w = ::write(f, p, left);
            if (w < 0) {
                if (errno == EINTR) continue;
                return;
            }
            p += w;
            left -= static_cast<size_t>(w);
        }
    }

    void close(ENV*) {
        closing.store(true);
        const int f = fd.load();
        if (f >= 0) (void)ioctl(f, SNDCTL_DSP_HALT_OUTPUT, nullptr);
        std::lock_guard<std::mutex> lock(io_m);
        const int g = fd.exchange(-1);
        if (g >= 0) ::close(g);
    }

    static void Register(ENV* env) {
        env->GetClass<FmodAudioDevice>("org/fmod/AudioDevice");
        auto c = env->GetClass("org/fmod/AudioDevice");
        c->Hook(env, "<init>", &FmodAudioDevice::ctor);
        c->HookInstanceFunction(env, "init", &FmodAudioDevice::init);
        c->HookInstanceFunction(env, "write", &FmodAudioDevice::write);
        c->HookInstanceFunction(env, "close", &FmodAudioDevice::close);
    }
};

class FmodMediaCodec : public Object {
public:
    static void Register(ENV* env) { env->GetClass<FmodMediaCodec>("org/fmod/MediaCodec"); }
};

} // namespace

/// Called once from jni_framework.cpp's class bring-up.
void register_fmod_classes(ENV* env) {
    FmodGate::Register(env);
    FmodAudioDevice::Register(env);
    FmodMediaCodec::Register(env);
}

} // namespace intoxicated
