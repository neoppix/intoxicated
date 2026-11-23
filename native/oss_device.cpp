// sound(4) device handling for Intoxicated's audio. See oss_device.h for the
// shape; this file holds the decisions and the reasons for them.
//
// Negotiation order follows the OSS API: SNDCTL_DSP_SETFRAGMENT before any
// format ioctl or write, then SETFMT, CHANNELS, SPEED. Every ioctl writes back
// what was granted and that granted value is what gets reported upward.
//
// Format. FMOD mixes in float, but sound(4) is not obliged to take float (the
// ALSA oss plugin on this very machine refuses FLOAT_LE), so streams write
// integer PCM and convert in userland. The integer width follows the device's
// mixing format: `dev.pcm.N.play.vchanformat` when vchans are on (s16le:2.0 on
// both the JBL headset and the Realtek codec here), otherwise the widest of
// S32/S16 the device lists. Writing S16 to an s16le vchan means the kernel
// feeder does no conversion at all.
//
// Fragments. Without SETFRAGMENT the kernel picks from hw.snd.latency, which
// measured 8 x 1024 B (42.7 ms) here. The default plan is 4 fragments of about
// 2.67 ms (128 frames at 48 kHz), the 4 x 512 B shape the spec measured: 8 to
// 10.7 ms queued in steady state, and three fragments (8 ms) still queued when
// the writer wakes. The kernel rounds the count up to a power of two (3 became
// 4 when measured), so the reachable totals at 48 kHz S16 stereo are 10.7 ms
// and 21.3 ms. In game FMOD's callback measured 2 to 10 us, so 8 ms of slack is
// a wide margin. `INTOXICATED_AUDIO_FRAGMENT=COUNTxFRAMES` overrides it.
#include "oss_device.h"

#include <cerrno>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <mutex>

#include <fcntl.h>
#include <sys/ioctl.h>
#include <sys/soundcard.h>
#include <sys/types.h>
#include <unistd.h>
#if defined(__FreeBSD__)
#include <sys/sysctl.h>
#endif

namespace intoxicated::audio {
namespace {

constexpr const char* kDefaultNode = "/dev/dsp";

std::string errno_text(const char* what) {
    return std::string(what) + ": " + strerror(errno);
}

/// The pcm unit a node path names, or -1. `/dev/dsp` itself is
/// hw.snd.default_unit; `/dev/dspN` and `/dev/dspN.M` are unit N.
int unit_of(const std::string& path) {
    const char* p = strstr(path.c_str(), "dsp");
    if (!p) return -1;
    p += 3;
    if (*p >= '0' && *p <= '9') return atoi(p);
    if (*p != '\0') return -1;
#if defined(__FreeBSD__)
    int unit = -1;
    size_t len = sizeof(unit);
    if (sysctlbyname("hw.snd.default_unit", &unit, &len, nullptr, 0) == 0) return unit;
#endif
    return -1;
}

/// The /dev/sndstat line for `unit` without its "pcmN: " prefix, or empty.
std::string sndstat_line(int unit) {
    if (unit < 0) return {};
    FILE* f = fopen("/dev/sndstat", "r");
    if (!f) return {};
    char prefix[32];
    snprintf(prefix, sizeof(prefix), "pcm%d: ", unit);
    std::string found;
    char line[512];
    while (fgets(line, sizeof(line), f)) {
        if (strncmp(line, prefix, strlen(prefix)) == 0) {
            found = line + strlen(prefix);
            while (!found.empty() && (found.back() == '\n' || found.back() == '\r')) found.pop_back();
            break;
        }
    }
    fclose(f);
    return found;
}

/// The integer width the device mixes at, from its vchan format when vchans
/// are on. Returns 0 when that cannot be read.
int vchan_bits(int unit, bool playback) {
#if defined(__FreeBSD__)
    if (unit < 0) return 0;
    char name[64];
    snprintf(name, sizeof(name), "dev.pcm.%d.%s.vchanformat", unit, playback ? "play" : "rec");
    char value[64] = {};
    size_t len = sizeof(value) - 1;
    if (sysctlbyname(name, value, &len, nullptr, 0) != 0) return 0;
    if (strncmp(value, "s32", 3) == 0 || strncmp(value, "s24", 3) == 0) return 32;
    if (strncmp(value, "s16", 3) == 0) return 16;
#else
    (void)unit;
    (void)playback;
#endif
    return 0;
}

int afmt_of(DeviceFormat f) { return f == DeviceFormat::S16 ? AFMT_S16_LE : AFMT_S32_LE; }

DeviceFormat choose_format(int fd, int unit) {
    const int bits = vchan_bits(unit, true);
    if (bits == 32) return DeviceFormat::S32;
    if (bits == 16) return DeviceFormat::S16;
    int mask = 0;
    if (ioctl(fd, SNDCTL_DSP_GETFMTS, &mask) == 0) {
        if (mask & AFMT_S32_LE) return DeviceFormat::S32;
    }
    return DeviceFormat::S16;
}

/// SETFMT, CHANNELS, SPEED, then the geometry readback. `space_ioctl` is
/// GETOSPACE or GETISPACE.
bool negotiate(int fd, DeviceFormat want, int want_channels, int want_rate, unsigned long space_ioctl,
               Negotiated* out, std::string* why) {
    int v = afmt_of(want);
    if (ioctl(fd, SNDCTL_DSP_SETFMT, &v) != 0) {
        *why = errno_text("SNDCTL_DSP_SETFMT");
        return false;
    }
    if (v == AFMT_S16_LE) {
        out->format = DeviceFormat::S16;
    } else if (v == AFMT_S32_LE) {
        out->format = DeviceFormat::S32;
    } else {
        char buf[96];
        snprintf(buf, sizeof(buf), "device granted format 0x%x, which is neither S16_LE nor S32_LE", v);
        *why = buf;
        return false;
    }
    v = want_channels;
    if (ioctl(fd, SNDCTL_DSP_CHANNELS, &v) != 0 || v < 1 || v > 8) {
        *why = errno_text("SNDCTL_DSP_CHANNELS");
        return false;
    }
    out->channels = v;
    v = want_rate;
    if (ioctl(fd, SNDCTL_DSP_SPEED, &v) != 0 || v < 8000 || v > 384000) {
        *why = errno_text("SNDCTL_DSP_SPEED");
        return false;
    }
    out->rate = v;
    audio_buf_info info = {};
    if (ioctl(fd, space_ioctl, &info) != 0 || info.fragsize <= 0 || info.fragstotal <= 0) {
        *why = errno_text("SNDCTL_DSP_GET[IO]SPACE");
        return false;
    }
    const int bpf = sample_bytes(out->format) * out->channels;
    out->fragment_bytes = info.fragsize;
    out->fragment_count = info.fragstotal;
    out->burst_frames = intoxicated_oss_fragment_frames(info.fragsize, bpf);
    out->buffer_frames = static_cast<int>(static_cast<int64_t>(info.fragsize) * info.fragstotal / bpf);
    if (out->burst_frames <= 0) {
        *why = "device fragment is smaller than one frame";
        return false;
    }
    return true;
}

std::once_flag g_probe_once;
bool g_probe_ok = false;
std::string g_probe_detail;

void run_probe() {
    const std::string path = device_path();
    Negotiated n;
    std::string why;
    const FragmentPlan plan = output_fragment_plan(48000);
    if (open_output(path, 2, 48000, plan, true, &n, &why)) {
        close(n.fd);
        char buf[256];
        snprintf(buf, sizeof(buf), "%s %d ch %d Hz, %d x %d frames (%.1f ms buffer)",
                 n.format == DeviceFormat::S16 ? "S16_LE" : "S32_LE", n.channels, n.rate,
                 n.fragment_count, n.burst_frames,
                 intoxicated_oss_frames_to_us(n.buffer_frames, n.rate) / 1000.0);
        g_probe_detail = buf;
        g_probe_ok = true;
    } else {
        g_probe_detail = why;
        g_probe_ok = false;
    }
}

} // namespace

std::string device_path() {
    const char* env = getenv("INTOXICATED_AUDIO_DEVICE");
    if (env && *env) return env;
    return kDefaultNode;
}

std::string describe_device(const std::string& path) {
    const int unit = unit_of(path);
    const char* env = getenv("INTOXICATED_AUDIO_DEVICE");
    const bool overridden = env && *env;
    std::string line = path;
    if (unit >= 0) {
        const std::string desc = sndstat_line(unit);
        line += " = pcm" + std::to_string(unit);
        if (!desc.empty()) line += " " + desc;
        else line += " (no /dev/sndstat entry)";
    } else {
        line += " (not a pcm node this layer can name)";
    }
    if (overridden) {
        line += "; chosen by INTOXICATED_AUDIO_DEVICE";
    } else {
        line += "; chosen as /dev/dsp, which follows hw.snd.default_unit";
    }
    line += "; fixed for the life of each stream";
    return line;
}

FragmentPlan output_fragment_plan(int rate) {
    FragmentPlan plan;
    const char* env = getenv("INTOXICATED_AUDIO_FRAGMENT");
    if (env && *env && intoxicated_oss_parse_fragment(env, &plan.count, &plan.frames)) return plan;
    plan.count = 4;
    plan.frames = intoxicated_oss_default_fragment_frames(rate);
    return plan;
}

bool open_output(const std::string& path, int want_channels, int want_rate, const FragmentPlan& plan,
                 bool nonblocking, Negotiated* out, std::string* why) {
    *out = Negotiated{};
    const int fd = open(path.c_str(), O_WRONLY | O_CLOEXEC | (nonblocking ? O_NONBLOCK : 0));
    if (fd < 0) {
        *why = errno_text(("open " + path).c_str());
        return false;
    }
    const DeviceFormat want = choose_format(fd, unit_of(path));
    // Fragment size is in bytes, so it is computed for the format about to be
    // requested; the readback after negotiation is what counts.
    int frag = static_cast<int>(
        intoxicated_oss_fragment_arg(plan.count, plan.frames, sample_bytes(want) * want_channels));
    if (ioctl(fd, SNDCTL_DSP_SETFRAGMENT, &frag) != 0) {
        // Not fatal: the device then runs at its own block size, which the
        // readback below reports truthfully.
        fprintf(stderr, "[audio] %s: SNDCTL_DSP_SETFRAGMENT refused (%s); using the device's own blocks\n",
                path.c_str(), strerror(errno));
    }
    if (!negotiate(fd, want, want_channels, want_rate, SNDCTL_DSP_GETOSPACE, out, why)) {
        close(fd);
        return false;
    }
    out->fd = fd;
    return true;
}

bool open_input(const std::string& path, int want_channels, int want_rate, Negotiated* out,
                std::string* why) {
    *out = Negotiated{};
    const int fd = open(path.c_str(), O_RDONLY | O_CLOEXEC | O_NONBLOCK);
    if (fd < 0) {
        *why = errno_text(("open " + path).c_str());
        return false;
    }
    // 10 ms fragments at 48 kHz mono S16: 960 B, rounded to 1024.
    int frag = static_cast<int>(intoxicated_oss_fragment_arg(8, want_rate / 100, 2 * want_channels));
    (void)ioctl(fd, SNDCTL_DSP_SETFRAGMENT, &frag);
    if (!negotiate(fd, DeviceFormat::S16, want_channels, want_rate, SNDCTL_DSP_GETISPACE, out, why)) {
        close(fd);
        return false;
    }
    if (out->format != DeviceFormat::S16) {
        close(fd);
        *why = "capture device would not take S16_LE";
        return false;
    }
    out->fd = fd;
    return true;
}

bool open_output_exact_s16(const std::string& path, int channels, int rate, int frames_per_buffer,
                           int buffers, Negotiated* out, std::string* why) {
    *out = Negotiated{};
    const int fd = open(path.c_str(), O_WRONLY | O_CLOEXEC);
    if (fd < 0) {
        *why = errno_text(("open " + path).c_str());
        return false;
    }
    int frag = static_cast<int>(intoxicated_oss_fragment_arg(buffers, frames_per_buffer, 2 * channels));
    (void)ioctl(fd, SNDCTL_DSP_SETFRAGMENT, &frag);
    if (!negotiate(fd, DeviceFormat::S16, channels, rate, SNDCTL_DSP_GETOSPACE, out, why)) {
        close(fd);
        return false;
    }
    if (out->format != DeviceFormat::S16 || out->channels != channels || out->rate != rate) {
        char buf[160];
        snprintf(buf, sizeof(buf), "device granted %s %d ch %d Hz for a stream mixed as S16 %d ch %d Hz",
                 out->format == DeviceFormat::S16 ? "S16" : "S32", out->channels, out->rate, channels, rate);
        *why = buf;
        close(fd);
        return false;
    }
    out->fd = fd;
    return true;
}

bool output_available() {
    std::call_once(g_probe_once, run_probe);
    return g_probe_ok;
}

void announce() {
    static std::once_flag once;
    std::call_once(once, [] {
        const std::string path = device_path();
        const bool ok = output_available();
        fprintf(stderr, "[audio] output: %s\n", describe_device(path).c_str());
        if (ok) {
            fprintf(stderr, "[audio] probe passed (%s): FMOD gets AAudio over OSS\n", g_probe_detail.c_str());
        } else {
            fprintf(stderr,
                    "[audio] probe FAILED (%s): org.fmod.FMOD.supportsAAudio() answers false and FMOD "
                    "falls back to org.fmod.AudioDevice on the same node\n",
                    g_probe_detail.c_str());
        }
    });
}

bool trace_enabled() {
    static const bool on = [] {
        const char* v = getenv("INTOXICATED_AUDIO_TRACE");
        return v && *v && strcmp(v, "0") != 0;
    }();
    return on;
}

} // namespace intoxicated::audio

// ------------------------------------------------------------- pure helpers

extern "C" {

uint32_t intoxicated_oss_fragment_arg(int count, int frames, int bytes_per_frame) {
    if (count < 2) count = 2;
    if (count > 0x7fff) count = 0x7fff;
    if (frames < 1) frames = 1;
    if (bytes_per_frame < 1) bytes_per_frame = 1;
    const double bytes = static_cast<double>(frames) * bytes_per_frame;
    int shift = static_cast<int>(std::lround(std::log2(bytes)));
    if (shift < 4) shift = 4;
    if (shift > 16) shift = 16;
    return (static_cast<uint32_t>(count) << 16) | static_cast<uint32_t>(shift);
}

int intoxicated_oss_fragment_frames(int fragment_bytes, int bytes_per_frame) {
    if (fragment_bytes <= 0 || bytes_per_frame <= 0) return 0;
    return fragment_bytes / bytes_per_frame;
}

int64_t intoxicated_oss_frames_to_us(int64_t frames, int rate) {
    if (rate <= 0) return 0;
    return frames * 1000000 / rate;
}

int intoxicated_oss_default_fragment_frames(int rate) {
    if (rate <= 0) rate = 48000;
    const double target = rate * 0.032 / 12.0; // 2.67 ms
    int frames = 16;
    while (frames * 2 <= 16384 && std::fabs(frames * 2 - target) <= std::fabs(frames - target)) frames *= 2;
    return frames;
}

int intoxicated_oss_parse_fragment(const char* text, int* count, int* frames) {
    if (!text) return 0;
    char* end = nullptr;
    const long c = strtol(text, &end, 10);
    if (end == text || (*end != 'x' && *end != 'X')) return 0;
    const char* rest = end + 1;
    const long f = strtol(rest, &end, 10);
    if (end == rest || *end != '\0') return 0;
    if (c < 2 || c > 32 || f < 16 || f > 16384) return 0;
    *count = static_cast<int>(c);
    *frames = static_cast<int>(f);
    return 1;
}

void intoxicated_f32_to_s16(const float* in, int16_t* out, size_t samples) {
    for (size_t i = 0; i < samples; i++) {
        float x = in[i];
        if (!(x == x)) x = 0.0f; // NaN
        if (x > 1.0f) x = 1.0f;
        if (x < -1.0f) x = -1.0f;
        out[i] = static_cast<int16_t>(std::lrintf(x * 32767.0f));
    }
}

void intoxicated_f32_to_s32(const float* in, int32_t* out, size_t samples) {
    for (size_t i = 0; i < samples; i++) {
        double x = in[i];
        if (!(x == x)) x = 0.0;
        if (x > 1.0) x = 1.0;
        if (x < -1.0) x = -1.0;
        out[i] = static_cast<int32_t>(std::llrint(x * 2147483647.0));
    }
}

void intoxicated_s16_to_s32(const int16_t* in, int32_t* out, size_t samples) {
    for (size_t i = 0; i < samples; i++) out[i] = static_cast<int32_t>(static_cast<uint32_t>(in[i]) << 16);
}

void intoxicated_s32_to_s16(const int32_t* in, int16_t* out, size_t samples) {
    for (size_t i = 0; i < samples; i++) out[i] = static_cast<int16_t>(in[i] >> 16);
}

int intoxicated_audio_output_available(void) { return intoxicated::audio::output_available() ? 1 : 0; }

void intoxicated_audio_announce(void) { intoxicated::audio::announce(); }

}
