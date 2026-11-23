// The FreeBSD sound(4) layer under Intoxicated's audio: which /dev/dsp node the
// client plays to, how a stream is negotiated on it, and the sample format
// conversion between what FMOD hands over and what the device takes.
//
// Everything here talks to OSS directly through <sys/soundcard.h>. There is no
// sound server in the path and no library to load, so the only thing audio needs
// on a FreeBSD machine is the kernel's own sound driver.
//
// Two users: `aaudio_oss.cpp` (the engine's libaaudio.so) and `fmod_java.cpp`
// (org.fmod.FMOD's gate and the org.fmod.AudioDevice fallback). They sit in
// different static libraries, which is why this header carries no C++ types
// beyond plain structs.
#pragma once

#include <cstddef>
#include <cstdint>
#include <string>

namespace intoxicated::audio {

/// Device sample formats this layer writes. Both are little-endian integer PCM;
/// FreeBSD's feeder chain converts from either to the hardware format.
enum class DeviceFormat { S16, S32 };

/// Bytes per sample of a device format.
inline int sample_bytes(DeviceFormat f) { return f == DeviceFormat::S16 ? 2 : 4; }

/// What one open() and negotiation produced. Every number is the value the
/// kernel wrote back, never the value asked for.
struct Negotiated {
    int fd = -1;
    DeviceFormat format = DeviceFormat::S16;
    int channels = 0;
    int rate = 0;
    int fragment_bytes = 0;   ///< GETOSPACE / GETISPACE fragsize
    int fragment_count = 0;   ///< fragstotal
    int burst_frames = 0;     ///< whole frames per fragment
    int buffer_frames = 0;    ///< fragment_count * fragment_bytes in frames
};

/// The fragment request a stream makes, as count x frames.
struct FragmentPlan {
    int count = 0;
    int frames = 0;
};

/// The node streams open: `INTOXICATED_AUDIO_DEVICE` when set and non-empty,
/// otherwise `/dev/dsp` (which sound(4) aliases to hw.snd.default_unit).
std::string device_path();

/// One human line describing `path`: the unit, its /dev/sndstat description,
/// and why it was chosen. Never empty.
std::string describe_device(const std::string& path);

/// The output fragment plan: `INTOXICATED_AUDIO_FRAGMENT=COUNTxFRAMES` when it
/// parses, otherwise the default for `rate`.
FragmentPlan output_fragment_plan(int rate);

/// Opens `path` for playback and negotiates a stream: fragment plan first,
/// then format, channels and rate, as the OSS API requires. `nonblocking`
/// opens with O_NONBLOCK (the probe). On failure returns false, closes
/// anything it opened, and puts the reason in `why`.
bool open_output(const std::string& path, int want_channels, int want_rate, const FragmentPlan& plan,
                 bool nonblocking, Negotiated* out, std::string* why);

/// Opens `path` for capture: S16, `want_channels` (falling back to whatever the
/// device grants), `want_rate`. The descriptor is non-blocking; readers wait
/// with poll().
bool open_input(const std::string& path, int want_channels, int want_rate, Negotiated* out,
                std::string* why);

/// Opens playback with the exact parameters the S16 Java fallback dictates
/// (FMOD's AudioDevice.init). Refuses unless channels and rate come back as
/// asked, because FMOD has already mixed for them.
bool open_output_exact_s16(const std::string& path, int channels, int rate, int frames_per_buffer,
                           int buffers, Negotiated* out, std::string* why);

/// Whether a playback stream can be opened on the chosen node right now, with
/// the format and plan real streams use. Probed once and cached, so the
/// answer the engine gets from org.fmod.FMOD.supportsAAudio() never changes
/// within a process.
bool output_available();

/// Logs the startup line: node, description, probe result. Idempotent.
void announce();

/// Whether `INTOXICATED_AUDIO_TRACE` asked for per-second stream statistics.
bool trace_enabled();

} // namespace intoxicated::audio

extern "C" {

/// SETFRAGMENT argument for `count` fragments of about `frames` frames each at
/// `bytes_per_frame`: high half the count, low half log2 of the byte size,
/// rounded to the nearest power of two and clamped to what sound(4) accepts
/// (16 bytes to 64 KiB).
uint32_t intoxicated_oss_fragment_arg(int count, int frames, int bytes_per_frame);

/// Whole frames in one fragment of `fragment_bytes`.
int intoxicated_oss_fragment_frames(int fragment_bytes, int bytes_per_frame);

/// `frames` at `rate` in microseconds (latency reporting).
int64_t intoxicated_oss_frames_to_us(int64_t frames, int rate);

/// Default fragment size in frames for `rate`: the power of two closest to
/// 2.67 ms (128 frames at 48 kHz). The default plan is four of them.
int intoxicated_oss_default_fragment_frames(int rate);

/// Parses `COUNTxFRAMES`. Returns 1 and fills both on success, 0 otherwise
/// (count 2..32, frames 16..16384).
int intoxicated_oss_parse_fragment(const char* text, int* count, int* frames);

/// Float to S16 with clamping; NaN becomes silence.
void intoxicated_f32_to_s16(const float* in, int16_t* out, size_t samples);

/// Float to S32 with clamping; NaN becomes silence.
void intoxicated_f32_to_s32(const float* in, int32_t* out, size_t samples);

/// S16 to S32 (left-justified).
void intoxicated_s16_to_s32(const int16_t* in, int32_t* out, size_t samples);

/// S32 to S16 (keeps the top 16 bits).
void intoxicated_s32_to_s16(const int32_t* in, int16_t* out, size_t samples);

/// The cached probe, for the Rust side and the Java gate. 1 available, 0 not.
int intoxicated_audio_output_available(void);

/// Startup announcement, called by the loader before the symbol table is built.
void intoxicated_audio_announce(void);

}
