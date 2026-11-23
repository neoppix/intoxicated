// Intoxicated's libaaudio.so: the 26 AAudio entry points engine 2.748 resolves,
// implemented straight over FreeBSD sound(4).
//
// Written from the behavioural spec of the engine's use (open, read getters,
// start, callback-driven playback, close; blocking read for capture) and the
// public AAudio reference. The guest reaches these through dlopen/dlsym on a
// virtual library the runtime registers; nothing here is an ELF export.
//
// Shape of a playback stream:
//
//   openStream    opens the OSS node and negotiates (oss_device.cpp); the
//                 stream is OPEN and nothing is written yet.
//   requestStart  marks it STARTED and, the first time, starts one writer
//                 thread. That thread calls the data callback for exactly one
//                 fragment of frames, converts float to the device format and
//                 does a blocking write(). The blocking write is the clock.
//   requestPause  PAUSING; the writer finishes the callback in flight, halts
//   requestStop   the device (SNDCTL_DSP_HALT_OUTPUT) and moves the state on to
//                 PAUSED / STOPPED, then sleeps until the next start.
//   close         refused from the callback thread; otherwise joins the writer,
//                 closes the descriptor and leaves the handle CLOSED.
//
// Locking. The writer never takes a lock while it runs a callback or writes.
// Control calls only flip an atomic state and poke a condition variable the
// writer waits on when idle, so start, pause and stop are safe from any
// thread, the callback thread included, and nothing a control call holds can
// be waited on by the writer.
//
// Handles are never freed. close releases the descriptor, the thread and the
// buffers and leaves a small CLOSED record behind, so a late getState or a
// waitForStateChange racing a close reads valid memory rather than a freed
// block. FMOD opens a handful of streams per audio init, so this is bytes.
//
// Capture follows the microphone rule: openStream(INPUT) touches no device and
// answers its getters from declared values; requestStart opens the node for
// reading; pause, stop and close each close it.
#include "oss_device.h"

#include <algorithm>
#include <atomic>
#include <cerrno>
#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#include <poll.h>
#include <sys/ioctl.h>
#include <sys/soundcard.h>
#include <time.h>
#include <unistd.h>

namespace {

using namespace intoxicated::audio;

// ---------------------------------------------------------------- AAudio ABI
// Values from the public aaudio/AAudio.h.

using aaudio_result_t = int32_t;
using aaudio_stream_state_t = int32_t;
using aaudio_format_t = int32_t;
using aaudio_direction_t = int32_t;

constexpr aaudio_result_t kOk = 0;
constexpr aaudio_result_t kErrDisconnected = -899;
constexpr aaudio_result_t kErrIllegalArgument = -898;
constexpr aaudio_result_t kErrInvalidState = -895;
constexpr aaudio_result_t kErrUnimplemented = -890;
constexpr aaudio_result_t kErrUnavailable = -889;
constexpr aaudio_result_t kErrNoMemory = -887;
constexpr aaudio_result_t kErrNull = -886;
constexpr aaudio_result_t kErrTimeout = -885;
constexpr aaudio_result_t kErrInvalidFormat = -883;

constexpr aaudio_direction_t kOutput = 0;
constexpr aaudio_direction_t kInput = 1;

constexpr aaudio_format_t kFmtUnspecified = 0;
constexpr aaudio_format_t kFmtI16 = 1;
constexpr aaudio_format_t kFmtFloat = 2;
constexpr aaudio_format_t kFmtI32 = 4;

constexpr aaudio_stream_state_t kOpen = 2;
constexpr aaudio_stream_state_t kStarting = 3;
constexpr aaudio_stream_state_t kStarted = 4;
constexpr aaudio_stream_state_t kPausing = 5;
constexpr aaudio_stream_state_t kPaused = 6;
constexpr aaudio_stream_state_t kStopping = 9;
constexpr aaudio_stream_state_t kStopped = 10;
constexpr aaudio_stream_state_t kClosing = 11;
constexpr aaudio_stream_state_t kClosed = 12;
constexpr aaudio_stream_state_t kDisconnected = 13;

constexpr int32_t kCallbackContinue = 0;

struct Stream;
using DataCallback = int32_t (*)(Stream*, void*, void*, int32_t);
using ErrorCallback = void (*)(Stream*, void*, aaudio_result_t);

struct Builder {
    aaudio_direction_t direction = kOutput;
    aaudio_format_t format = kFmtUnspecified;
    int32_t capacity = 0;
    int32_t performance_mode = 10;
    int32_t usage = 0;
    int32_t input_preset = 0;
    DataCallback data_cb = nullptr;
    void* data_user = nullptr;
    ErrorCallback error_cb = nullptr;
    void* error_user = nullptr;
};

// Declared capture shape: what an INPUT stream reports before any device is
// opened, and what read() then delivers.
constexpr int kInputRate = 48000;
constexpr int kInputChannels = 1;
constexpr int kInputBurst = 480;
constexpr int kInputCapacity = 24000;

// ------------------------------------------------------------------ sinks
//
// The writer thread speaks to an abstract sink so the state machine can be
// tested without a sound card. The OSS sink is the only one production uses.

struct Sink {
    virtual ~Sink() = default;
    /// Writes all of `bytes` or fails with an errno value. Blocks for pacing.
    virtual int write_all(const void* data, size_t bytes) = 0;
    /// Drops whatever is queued and stops the device until the next write.
    virtual void halt() = 0;
    /// Waits until everything written has been played.
    virtual void drain() = 0;
    /// Underruns since the last call.
    virtual int take_underruns() = 0;
    /// Frames queued but not yet played, or -1.
    virtual int queued_frames() = 0;
};

struct OssSink final : Sink {
    int fd;
    int bytes_per_frame;
    OssSink(int f, int bpf) : fd(f), bytes_per_frame(bpf) {}
    ~OssSink() override {
        if (fd >= 0) {
            (void)ioctl(fd, SNDCTL_DSP_HALT_OUTPUT, nullptr);
            close(fd);
        }
    }
    int write_all(const void* data, size_t bytes) override {
        const char* p = static_cast<const char*>(data);
        while (bytes > 0) {
            const ssize_t n = write(fd, p, bytes);
            if (n < 0) {
                if (errno == EINTR) continue;
                return errno;
            }
            p += n;
            bytes -= static_cast<size_t>(n);
        }
        return 0;
    }
    void halt() override { (void)ioctl(fd, SNDCTL_DSP_HALT_OUTPUT, nullptr); }
    void drain() override { (void)ioctl(fd, SNDCTL_DSP_SYNC, nullptr); }
    int take_underruns() override {
        audio_errinfo ei = {};
        if (ioctl(fd, SNDCTL_DSP_GETERROR, &ei) != 0) return 0;
        return ei.play_underruns > 0 ? ei.play_underruns : 0;
    }
    int queued_frames() override {
        int bytes = 0;
        if (ioctl(fd, SNDCTL_DSP_GETODELAY, &bytes) != 0) return -1;
        return bytes / bytes_per_frame;
    }
};

/// Test sink: paces writes at real time for the stream's rate and can be told
/// to fail, which is how the disconnect path is exercised.
struct PacedNullSink final : Sink {
    int bytes_per_frame;
    int rate;
    std::atomic<int> fail_with{0};
    std::chrono::steady_clock::time_point next = std::chrono::steady_clock::now();
    PacedNullSink(int bpf, int r) : bytes_per_frame(bpf), rate(r) {}
    int write_all(const void*, size_t bytes) override {
        if (int e = fail_with.load()) return e;
        const auto frames = static_cast<int64_t>(bytes / bytes_per_frame);
        const auto now = std::chrono::steady_clock::now();
        if (next < now) next = now;
        next += std::chrono::microseconds(frames * 1000000 / rate);
        std::this_thread::sleep_until(next);
        return 0;
    }
    void halt() override { next = std::chrono::steady_clock::now(); }
    void drain() override { std::this_thread::sleep_until(next); }
    int take_underruns() override { return 0; }
    int queued_frames() override { return 0; }
};

std::atomic<bool> g_test_sink{false};
std::atomic<PacedNullSink*> g_last_test_sink{nullptr};

// ----------------------------------------------------------------- stream

struct Stream {
    // Fixed at open.
    aaudio_direction_t direction = kOutput;
    aaudio_format_t app_format = kFmtFloat;
    DeviceFormat dev_format = DeviceFormat::S16;
    int channels = 0;
    int rate = 0;
    int burst = 0;
    int capacity = 0;
    DataCallback data_cb = nullptr;
    void* data_user = nullptr;
    ErrorCallback error_cb = nullptr;
    void* error_user = nullptr;
    int id = 0;
    std::string path;

    std::atomic<aaudio_stream_state_t> state{kOpen};
    std::atomic<bool> closing{false};
    std::atomic<int32_t> xruns{0};
    std::atomic<int> waiters{0};

    // Output.
    std::unique_ptr<Sink> sink;
    // `ctl_m` guards creating and reaping the writer thread and nothing else.
    // The writer never takes it, and a control call made from the writer
    // (start, pause or stop inside the callback) never needs to.
    std::mutex ctl_m;
    std::thread writer;
    std::atomic<bool> has_writer{false};
    std::mutex wake_m;
    std::condition_variable wake_cv;
    std::vector<unsigned char> app_buf;
    std::vector<unsigned char> dev_buf;

    // Input. `io_m` serialises read() against the device being opened or
    // closed underneath it; nothing on the output path takes it.
    std::mutex io_m;
    int in_fd = -1;
    int in_dev_channels = 0;
    PacedNullSink* in_test = nullptr;

    // Statistics, written by the writer only.
    uint64_t callbacks = 0;
    uint64_t frames = 0;
    int odelay_min = -1;
    int odelay_max = -1;
    std::chrono::steady_clock::time_point started_at{};
};

thread_local Stream* t_writer_of = nullptr;
std::atomic<int> g_next_id{1};

const char* state_name(aaudio_stream_state_t s) {
    switch (s) {
    case kOpen: return "OPEN";
    case kStarting: return "STARTING";
    case kStarted: return "STARTED";
    case kPausing: return "PAUSING";
    case kPaused: return "PAUSED";
    case kStopping: return "STOPPING";
    case kStopped: return "STOPPED";
    case kClosing: return "CLOSING";
    case kClosed: return "CLOSED";
    case kDisconnected: return "DISCONNECTED";
    default: return "?";
    }
}

const char* format_name(aaudio_format_t f) {
    switch (f) {
    case kFmtUnspecified: return "UNSPECIFIED";
    case kFmtI16: return "PCM_I16";
    case kFmtFloat: return "PCM_FLOAT";
    case 3: return "PCM_I24_PACKED";
    case kFmtI32: return "PCM_I32";
    default: return "INVALID";
    }
}

int app_sample_bytes(aaudio_format_t f) { return f == kFmtI16 ? 2 : 4; }

void wake(Stream* s) {
    std::lock_guard<std::mutex> lock(s->wake_m);
    s->wake_cv.notify_all();
}

/// Converts one callback's worth of app samples into the device buffer.
void convert(Stream* s, size_t samples) {
    const void* in = s->app_buf.data();
    void* out = s->dev_buf.data();
    if (s->dev_format == DeviceFormat::S16) {
        auto* o = static_cast<int16_t*>(out);
        if (s->app_format == kFmtFloat) intoxicated_f32_to_s16(static_cast<const float*>(in), o, samples);
        else if (s->app_format == kFmtI16) memcpy(o, in, samples * 2);
        else intoxicated_s32_to_s16(static_cast<const int32_t*>(in), o, samples);
    } else {
        auto* o = static_cast<int32_t*>(out);
        if (s->app_format == kFmtFloat) intoxicated_f32_to_s32(static_cast<const float*>(in), o, samples);
        else if (s->app_format == kFmtI16) intoxicated_s16_to_s32(static_cast<const int16_t*>(in), o, samples);
        else memcpy(o, in, samples * 4);
    }
}

float peak_of(Stream* s, size_t samples) {
    if (s->app_format != kFmtFloat) return -1.0f;
    const float* p = reinterpret_cast<const float*>(s->app_buf.data());
    float peak = 0.0f;
    for (size_t i = 0; i < samples; i++) {
        const float a = p[i] < 0 ? -p[i] : p[i];
        if (a > peak) peak = a;
    }
    return peak;
}

void log_summary(Stream* s, const char* why) {
    const double secs =
        s->callbacks ? std::chrono::duration<double>(std::chrono::steady_clock::now() - s->started_at).count() : 0.0;
    fprintf(stderr,
            "[audio] stream %d %s: %llu callbacks, %llu frames in %.1f s (%.0f frames/s), "
            "queued %d..%d frames (%.1f..%.1f ms), %d underruns\n",
            s->id, why, static_cast<unsigned long long>(s->callbacks), static_cast<unsigned long long>(s->frames),
            secs, secs > 0 ? s->frames / secs : 0.0, s->odelay_min, s->odelay_max,
            intoxicated_oss_frames_to_us(s->odelay_min < 0 ? 0 : s->odelay_min, s->rate) / 1000.0,
            intoxicated_oss_frames_to_us(s->odelay_max < 0 ? 0 : s->odelay_max, s->rate) / 1000.0,
            s->xruns.load());
}

/// Delivers ERROR_DISCONNECTED once, from a thread of its own, so the
/// application may stop and close the stream from inside its error callback.
void report_disconnect(Stream* s, int err) {
    fprintf(stderr, "[audio] stream %d: %s write failed (%s); DISCONNECTED\n", s->id, s->path.c_str(),
            strerror(err));
    s->state.store(kDisconnected);
    if (!s->error_cb) return;
    ErrorCallback cb = s->error_cb;
    void* user = s->error_user;
    std::thread([s, cb, user] { cb(s, user, kErrDisconnected); }).detach();
}

void writer_main(Stream* s) {
    t_writer_of = s;
    const size_t samples = static_cast<size_t>(s->burst) * s->channels;
    const size_t dev_bytes = samples * sample_bytes(s->dev_format);
    const bool trace = trace_enabled();
    auto window_start = std::chrono::steady_clock::now();
    uint64_t window_callbacks = 0, window_frames = 0;
    int window_odelay_min = -1, window_odelay_max = -1;
    float window_peak = 0.0f;
    int64_t window_cb_max_us = 0;
    bool halted = true;

    while (!s->closing.load()) {
        const aaudio_stream_state_t st = s->state.load();
        if (st == kStarted) {
            if (s->callbacks == 0) s->started_at = std::chrono::steady_clock::now();
            const auto cb_start = trace ? std::chrono::steady_clock::now() : std::chrono::steady_clock::time_point{};
            const int32_t r = s->data_cb(s, s->data_user, s->app_buf.data(), s->burst);
            if (trace) {
                const int64_t us = std::chrono::duration_cast<std::chrono::microseconds>(
                                       std::chrono::steady_clock::now() - cb_start).count();
                if (us > window_cb_max_us) window_cb_max_us = us;
            }
            s->callbacks++;
            window_callbacks++;
            if (r != kCallbackContinue) {
                // STOP from the callback: this buffer still plays, then no more
                // calls and the stream stays open (STOPPING drains it).
                aaudio_stream_state_t expect = kStarted;
                s->state.compare_exchange_strong(expect, kStopping);
            } else if (s->state.load() == kPausing) {
                continue; // paused from inside the callback: the buffer is not played
            }
            convert(s, samples);
            if (trace) {
                const float p = peak_of(s, samples);
                if (p > window_peak) window_peak = p;
            }
            const int err = s->sink->write_all(s->dev_buf.data(), dev_bytes);
            if (err != 0) {
                report_disconnect(s, err);
                continue;
            }
            halted = false;
            s->frames += static_cast<uint64_t>(s->burst);
            window_frames += static_cast<uint64_t>(s->burst);
            const int q = s->sink->queued_frames();
            if (q >= 0 && s->callbacks > 8) {
                if (s->odelay_min < 0 || q < s->odelay_min) s->odelay_min = q;
                if (q > s->odelay_max) s->odelay_max = q;
                if (window_odelay_min < 0 || q < window_odelay_min) window_odelay_min = q;
                if (q > window_odelay_max) window_odelay_max = q;
            }
            const auto now = std::chrono::steady_clock::now();
            if (now - window_start >= std::chrono::seconds(1)) {
                const int u = s->sink->take_underruns();
                if (u > 0) s->xruns.fetch_add(u);
                if (trace) {
                    const double secs = std::chrono::duration<double>(now - window_start).count();
                    fprintf(stderr,
                            "[audio] stream %d: %llu cb/s, %.0f frames/s, %d frames per cb, queued %d..%d "
                            "frames (%.1f..%.1f ms), underruns %d (total %d), peak %.4f, longest callback %lld us\n",
                            s->id, static_cast<unsigned long long>(window_callbacks), window_frames / secs, s->burst,
                            window_odelay_min, window_odelay_max,
                            intoxicated_oss_frames_to_us(window_odelay_min < 0 ? 0 : window_odelay_min, s->rate) / 1000.0,
                            intoxicated_oss_frames_to_us(window_odelay_max < 0 ? 0 : window_odelay_max, s->rate) / 1000.0,
                            u, s->xruns.load(), window_peak, static_cast<long long>(window_cb_max_us));
                }
                window_start = now;
                window_callbacks = window_frames = 0;
                window_odelay_min = window_odelay_max = -1;
                window_peak = 0.0f;
                window_cb_max_us = 0;
            }
            continue;
        }
        if (st == kPausing || st == kStopping) {
            if (!halted) {
                // Count what happened up to here, then stop the device so the
                // silence after it is not reported as underruns. Stop plays out
                // what is queued (AAudio stop semantics, at most one device
                // buffer); pause drops it.
                const int u = s->sink->take_underruns();
                if (u > 0) s->xruns.fetch_add(u);
                if (st == kStopping) s->sink->drain();
                s->sink->halt();
                (void)s->sink->take_underruns();
                halted = true;
            }
            aaudio_stream_state_t expect = st;
            s->state.compare_exchange_strong(expect, st == kPausing ? kPaused : kStopped);
            continue;
        }
        // OPEN, PAUSED, STOPPED, DISCONNECTED: sleep until something changes.
        std::unique_lock<std::mutex> lock(s->wake_m);
        s->wake_cv.wait_for(lock, std::chrono::milliseconds(200),
                            [&] { return s->closing.load() || s->state.load() != st; });
    }
    if (!halted) s->sink->halt();
}

// --------------------------------------------------------------- open

aaudio_result_t open_output_stream(const Builder& b, Stream** out) {
    aaudio_format_t app = b.format == kFmtUnspecified ? kFmtFloat : b.format;
    if (app != kFmtFloat && app != kFmtI16 && app != kFmtI32) return kErrInvalidFormat;

    auto* s = new (std::nothrow) Stream;
    if (!s) return kErrNoMemory;
    s->direction = kOutput;
    s->app_format = app;
    s->id = g_next_id.fetch_add(1);
    s->path = device_path();

    if (g_test_sink.load()) {
        s->dev_format = DeviceFormat::S16;
        s->channels = 2;
        s->rate = 48000;
        s->burst = 256;
        s->capacity = 768;
        auto sink = std::make_unique<PacedNullSink>(4, 48000);
        g_last_test_sink.store(sink.get());
        s->sink = std::move(sink);
    } else {
        Negotiated n;
        std::string why;
        if (!open_output(s->path, 2, 48000, output_fragment_plan(48000), false, &n, &why)) {
            fprintf(stderr, "[audio] openStream(OUTPUT) refused: %s -> ERROR_UNAVAILABLE\n", why.c_str());
            delete s;
            return kErrUnavailable;
        }
        s->dev_format = n.format;
        s->channels = n.channels;
        s->rate = n.rate;
        s->burst = n.burst_frames;
        s->capacity = n.buffer_frames;
        s->sink = std::make_unique<OssSink>(n.fd, sample_bytes(n.format) * n.channels);
    }
    s->data_cb = b.data_cb;
    s->data_user = b.data_user;
    s->error_cb = b.error_cb;
    s->error_user = b.error_user;
    s->app_buf.assign(static_cast<size_t>(s->burst) * s->channels * app_sample_bytes(app), 0);
    s->dev_buf.assign(static_cast<size_t>(s->burst) * s->channels * sample_bytes(s->dev_format), 0);
    *out = s;
    return kOk;
}

aaudio_result_t open_input_stream(const Builder& b, Stream** out) {
    if (b.data_cb) {
        // Not a shape the engine uses; a callback-driven microphone nobody
        // pulls would be a capture handle held for nothing.
        fprintf(stderr, "[audio] openStream(INPUT) with a data callback refused -> ERROR_UNIMPLEMENTED\n");
        return kErrUnimplemented;
    }
    if (b.format != kFmtUnspecified && b.format != kFmtI16) return kErrInvalidFormat;
    auto* s = new (std::nothrow) Stream;
    if (!s) return kErrNoMemory;
    s->direction = kInput;
    s->app_format = kFmtI16;
    s->channels = kInputChannels;
    s->rate = kInputRate;
    s->burst = kInputBurst;
    s->capacity = kInputCapacity;
    s->id = g_next_id.fetch_add(1);
    s->path = device_path();
    *out = s;
    return kOk;
}

// Capture device lifetime. Called with io_m held.
aaudio_result_t input_open_device(Stream* s) {
    if (s->in_fd >= 0 || s->in_test) return kOk;
    if (g_test_sink.load()) {
        s->in_test = new PacedNullSink(2, kInputRate);
        return kOk;
    }
    Negotiated n;
    std::string why;
    if (!open_input(s->path, kInputChannels, kInputRate, &n, &why)) {
        fprintf(stderr, "[audio] stream %d: capture open failed: %s\n", s->id, why.c_str());
        return kErrUnavailable;
    }
    if (n.rate != kInputRate || n.channels > 2) {
        fprintf(stderr, "[audio] stream %d: capture granted %d ch %d Hz, not %d ch %d Hz; refused\n", s->id,
                n.channels, n.rate, kInputChannels, kInputRate);
        close(n.fd);
        return kErrUnavailable;
    }
    s->in_fd = n.fd;
    s->in_dev_channels = n.channels;
    fprintf(stderr, "[audio] stream %d: capture opened on %s (%d ch device, delivered as mono)\n", s->id,
            s->path.c_str(), n.channels);
    return kOk;
}

void input_close_device(Stream* s) {
    if (s->in_fd >= 0) {
        (void)ioctl(s->in_fd, SNDCTL_DSP_HALT_INPUT, nullptr);
        close(s->in_fd);
        s->in_fd = -1;
        fprintf(stderr, "[audio] stream %d: capture closed\n", s->id);
    }
    delete s->in_test;
    s->in_test = nullptr;
}

bool valid(Stream* s) { return s && s->state.load() != kClosed && s->state.load() != kClosing; }

// ------------------------------------------------------------ API: builder

extern "C" aaudio_result_t ix_createStreamBuilder(Builder** out) {
    if (!out) return kErrNull;
    *out = new (std::nothrow) Builder;
    return *out ? kOk : kErrNoMemory;
}

extern "C" aaudio_result_t ix_builderDelete(Builder* b) {
    if (!b) return kErrNull;
    delete b;
    return kOk;
}

extern "C" void ix_setDirection(Builder* b, aaudio_direction_t d) { if (b) b->direction = d; }
extern "C" void ix_setFormat(Builder* b, aaudio_format_t f) { if (b) b->format = f; }
extern "C" void ix_setBufferCapacityInFrames(Builder* b, int32_t n) { if (b) b->capacity = n; }
extern "C" void ix_setPerformanceMode(Builder* b, int32_t m) { if (b) b->performance_mode = m; }
extern "C" void ix_setUsage(Builder* b, int32_t u) { if (b) b->usage = u; }
extern "C" void ix_setInputPreset(Builder* b, int32_t p) { if (b) b->input_preset = p; }
extern "C" void ix_setDataCallback(Builder* b, DataCallback cb, void* user) {
    if (b) { b->data_cb = cb; b->data_user = user; }
}
extern "C" void ix_setErrorCallback(Builder* b, ErrorCallback cb, void* user) {
    if (b) { b->error_cb = cb; b->error_user = user; }
}

extern "C" aaudio_result_t ix_openStream(Builder* b, Stream** out) {
    if (!b || !out) return kErrNull;
    *out = nullptr;
    aaudio_result_t r;
    if (b->direction == kOutput) r = open_output_stream(*b, out);
    else if (b->direction == kInput) r = open_input_stream(*b, out);
    else r = kErrIllegalArgument;
    if (r == kOk) {
        Stream* s = *out;
        fprintf(stderr,
                "[audio] openStream #%d %s format=%s capacity=%d performanceMode=%d usage=%d inputPreset=%d "
                "dataCallback=%s errorCallback=%s -> OK: %d Hz %d ch %s burst %d capacity %d%s%s\n",
                s->id, b->direction == kOutput ? "OUTPUT" : "INPUT", format_name(b->format), b->capacity,
                b->performance_mode, b->usage, b->input_preset, b->data_cb ? "yes" : "no",
                b->error_cb ? "yes" : "no", s->rate, s->channels, format_name(s->app_format), s->burst, s->capacity,
                s->direction == kOutput ? (s->dev_format == DeviceFormat::S16 ? ", device S16_LE on " : ", device S32_LE on ")
                                        : ", no device open until requestStart on ",
                s->path.c_str());
    } else {
        fprintf(stderr,
                "[audio] openStream %s format=%s capacity=%d performanceMode=%d usage=%d dataCallback=%s -> %d\n",
                b->direction == kOutput ? "OUTPUT" : b->direction == kInput ? "INPUT" : "?", format_name(b->format),
                b->capacity, b->performance_mode, b->usage, b->data_cb ? "yes" : "no", r);
    }
    return r;
}

// ------------------------------------------------------------- API: stream

extern "C" aaudio_result_t ix_requestStart(Stream* s) {
    if (!valid(s)) return kErrInvalidState;
    if (s->state.load() == kDisconnected) return kErrDisconnected;
    if (s->direction == kInput) {
        std::lock_guard<std::mutex> lock(s->io_m);
        const aaudio_result_t r = input_open_device(s);
        if (r != kOk) return r;
        s->state.store(kStarted);
        return kOk;
    }
    for (;;) {
        aaudio_stream_state_t st = s->state.load();
        if (st == kStarted) return kOk;
        if (st == kDisconnected) return kErrDisconnected;
        if (st == kClosing || st == kClosed) return kErrInvalidState;
        if (s->state.compare_exchange_weak(st, kStarted)) break;
    }
    if (s->data_cb && t_writer_of != s && !s->has_writer.load()) {
        std::lock_guard<std::mutex> lock(s->ctl_m);
        if (!s->has_writer.load() && !s->closing.load()) {
            s->writer = std::thread(writer_main, s);
            s->has_writer.store(true);
        }
    }
    wake(s);
    return kOk;
}

/// Shared by pause and stop. `transient` is PAUSING or STOPPING, `final_state`
/// PAUSED or STOPPED.
aaudio_result_t request_halt(Stream* s, aaudio_stream_state_t transient, aaudio_stream_state_t final_state) {
    if (!valid(s)) return kErrInvalidState;
    if (s->direction == kInput) {
        // The microphone is released on pause as well as stop: a held but
        // paused capture is indistinguishable from recording to the user.
        if (s->state.load() == kDisconnected) return kOk;
        s->state.store(final_state);
        std::lock_guard<std::mutex> lock(s->io_m);
        input_close_device(s);
        return kOk;
    }
    for (;;) {
        aaudio_stream_state_t st = s->state.load();
        // Nothing plays on a lost device; stopping it is already true.
        if (st == kDisconnected) return kOk;
        if (st == final_state || st == transient) return kOk;
        // No writer running means nothing to drain: go straight to the end.
        const bool writer_running = s->has_writer.load() || t_writer_of == s;
        const aaudio_stream_state_t next = (st == kStarted && writer_running) ? transient : final_state;
        if (st == kPausing || st == kStopping) {
            // Pause racing stop, or the reverse: let the later request win.
            if (s->state.compare_exchange_weak(st, transient)) break;
            continue;
        }
        if (s->state.compare_exchange_weak(st, next)) break;
    }
    wake(s);
    return kOk;
}

extern "C" aaudio_result_t ix_requestPause(Stream* s) {
    if (s && s->direction == kInput) return request_halt(s, kPaused, kPaused);
    return request_halt(s, kPausing, kPaused);
}

extern "C" aaudio_result_t ix_requestStop(Stream* s) {
    if (s && s->direction == kInput) return request_halt(s, kStopped, kStopped);
    return request_halt(s, kStopping, kStopped);
}

extern "C" aaudio_result_t ix_close(Stream* s) {
    if (!s) return kErrNull;
    if (t_writer_of == s) {
        // AAudio forbids close from the data callback; joining ourselves would hang.
        return kErrInvalidState;
    }
    aaudio_stream_state_t st = s->state.load();
    if (st == kClosed || st == kClosing) return kErrInvalidState;
    std::thread writer;
    {
        std::lock_guard<std::mutex> lock(s->ctl_m);
        s->closing.store(true);
        s->state.store(kClosing);
        writer = std::move(s->writer);
    }
    wake(s);
    if (writer.joinable()) writer.join();
    if (s->direction == kInput) {
        std::lock_guard<std::mutex> lock(s->io_m);
        input_close_device(s);
    } else {
        log_summary(s, "closed");
        if (g_last_test_sink.load() == s->sink.get()) g_last_test_sink.store(nullptr);
        s->sink.reset();
    }
    std::vector<unsigned char>().swap(s->app_buf);
    std::vector<unsigned char>().swap(s->dev_buf);
    s->state.store(kClosed);
    // Waiters see CLOSED and return; give them a moment so none is left
    // mid-poll when the caller tears down whatever owns its own state.
    for (int i = 0; i < 200 && s->waiters.load() > 0; i++) std::this_thread::sleep_for(std::chrono::milliseconds(1));
    return kOk;
}

extern "C" aaudio_stream_state_t ix_getState(Stream* s) { return s ? s->state.load() : kClosed; }
extern "C" int32_t ix_getSampleRate(Stream* s) { return s ? s->rate : 0; }
extern "C" int32_t ix_getChannelCount(Stream* s) { return s ? s->channels : 0; }
extern "C" aaudio_format_t ix_getFormat(Stream* s) { return s ? s->app_format : -1; }
extern "C" int32_t ix_getFramesPerBurst(Stream* s) { return s ? s->burst : 0; }
extern "C" int32_t ix_getBufferCapacityInFrames(Stream* s) { return s ? s->capacity : 0; }
extern "C" int32_t ix_getBufferSizeInFrames(Stream* s) { return s ? s->capacity : 0; }
extern "C" int32_t ix_getXRunCount(Stream* s) { return s ? s->xruns.load() : 0; }

/// The device buffer is fixed once negotiated, so the size in force is always
/// the whole of it. AAudio's contract is to return the size actually applied.
extern "C" int32_t ix_setBufferSizeInFrames(Stream* s, int32_t) {
    if (!valid(s)) return kErrInvalidState;
    return s->capacity;
}

extern "C" aaudio_result_t ix_waitForStateChange(Stream* s, aaudio_stream_state_t input,
                                                  aaudio_stream_state_t* next, int64_t timeout_ns) {
    if (!s) return kErrNull;
    s->waiters.fetch_add(1);
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::nanoseconds(timeout_ns > 0 ? timeout_ns : 0);
    aaudio_result_t r = kOk;
    for (;;) {
        const aaudio_stream_state_t st = s->state.load();
        if (st != input || st == kClosed || st == kDisconnected) {
            if (next) *next = st;
            break;
        }
        if (std::chrono::steady_clock::now() >= deadline) {
            if (next) *next = st;
            r = kErrTimeout;
            break;
        }
        std::this_thread::sleep_for(std::chrono::milliseconds(1));
    }
    s->waiters.fetch_sub(1);
    return r;
}

/// Blocking capture. Reads S16 from the device, downmixing a stereo device to
/// the declared mono. timeout <= 0 returns what is there now.
extern "C" int32_t ix_read(Stream* s, void* buffer, int32_t num_frames, int64_t timeout_ns) {
    if (!s || !buffer) return kErrNull;
    if (s->direction != kInput) return kErrUnimplemented;
    if (num_frames < 0) return kErrIllegalArgument;
    if (s->state.load() != kStarted) return kErrInvalidState;
    std::lock_guard<std::mutex> lock(s->io_m);
    if (s->state.load() != kStarted) return kErrInvalidState;
    auto* out = static_cast<int16_t*>(buffer);
    if (s->in_test) {
        // Test source: real-time paced silence.
        memset(out, 0, static_cast<size_t>(num_frames) * 2);
        (void)s->in_test->write_all(out, static_cast<size_t>(num_frames) * 2);
        return num_frames;
    }
    const int dev_ch = s->in_dev_channels;
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::nanoseconds(timeout_ns > 0 ? timeout_ns : 0);
    int32_t got = 0;
    int16_t scratch[2048];
    while (got < num_frames) {
        const int want_frames = std::min<int>(num_frames - got, static_cast<int>(sizeof(scratch) / 2 / dev_ch));
        const ssize_t n = ::read(s->in_fd, scratch, static_cast<size_t>(want_frames) * 2 * dev_ch);
        if (n > 0) {
            const int frames = static_cast<int>(n / (2 * dev_ch));
            for (int i = 0; i < frames; i++) {
                out[got + i] = dev_ch == 1 ? scratch[i]
                                           : static_cast<int16_t>((scratch[2 * i] + scratch[2 * i + 1]) / 2);
            }
            got += frames;
            continue;
        }
        if (n < 0 && errno != EAGAIN && errno != EINTR) {
            s->state.store(kDisconnected);
            return got > 0 ? got : kErrDisconnected;
        }
        if (timeout_ns <= 0) break;
        const auto left = std::chrono::duration_cast<std::chrono::milliseconds>(deadline - std::chrono::steady_clock::now());
        if (left.count() <= 0) break;
        struct pollfd p = {s->in_fd, POLLIN, 0};
        (void)poll(&p, 1, static_cast<int>(std::min<long long>(left.count(), 10)));
        if (s->state.load() != kStarted) break;
    }
    return got;
}

// -------------------------------------------------------------- table

struct SymbolEntry {
    const char* name;
    void* addr;
};

#define IX_SYM(name, fn) {name, reinterpret_cast<void*>(&fn)}
const SymbolEntry kSymbols[] = {
    IX_SYM("AAudio_createStreamBuilder", ix_createStreamBuilder),
    IX_SYM("AAudioStreamBuilder_delete", ix_builderDelete),
    IX_SYM("AAudioStreamBuilder_openStream", ix_openStream),
    IX_SYM("AAudioStreamBuilder_setBufferCapacityInFrames", ix_setBufferCapacityInFrames),
    IX_SYM("AAudioStreamBuilder_setDataCallback", ix_setDataCallback),
    IX_SYM("AAudioStreamBuilder_setDirection", ix_setDirection),
    IX_SYM("AAudioStreamBuilder_setErrorCallback", ix_setErrorCallback),
    IX_SYM("AAudioStreamBuilder_setFormat", ix_setFormat),
    IX_SYM("AAudioStreamBuilder_setInputPreset", ix_setInputPreset),
    IX_SYM("AAudioStreamBuilder_setPerformanceMode", ix_setPerformanceMode),
    IX_SYM("AAudioStreamBuilder_setUsage", ix_setUsage),
    IX_SYM("AAudioStream_close", ix_close),
    IX_SYM("AAudioStream_getBufferCapacityInFrames", ix_getBufferCapacityInFrames),
    IX_SYM("AAudioStream_getBufferSizeInFrames", ix_getBufferSizeInFrames),
    IX_SYM("AAudioStream_getChannelCount", ix_getChannelCount),
    IX_SYM("AAudioStream_getFormat", ix_getFormat),
    IX_SYM("AAudioStream_getFramesPerBurst", ix_getFramesPerBurst),
    IX_SYM("AAudioStream_getSampleRate", ix_getSampleRate),
    IX_SYM("AAudioStream_getState", ix_getState),
    IX_SYM("AAudioStream_getXRunCount", ix_getXRunCount),
    IX_SYM("AAudioStream_read", ix_read),
    IX_SYM("AAudioStream_requestPause", ix_requestPause),
    IX_SYM("AAudioStream_requestStart", ix_requestStart),
    IX_SYM("AAudioStream_requestStop", ix_requestStop),
    IX_SYM("AAudioStream_setBufferSizeInFrames", ix_setBufferSizeInFrames),
    IX_SYM("AAudioStream_waitForStateChange", ix_waitForStateChange),
};
#undef IX_SYM

} // namespace

extern "C" {

/// The libaaudio.so table the runtime registers as a virtual library.
const SymbolEntry* intoxicated_aaudio_symbols(size_t* count) {
    *count = sizeof(kSymbols) / sizeof(kSymbols[0]);
    return kSymbols;
}

/// Tests only: streams opened after this use a real-time paced null sink and
/// a silent capture source instead of /dev/dsp.
void intoxicated_aaudio_use_test_sink(int on) { g_test_sink.store(on != 0); }

/// Tests only: makes the most recently opened test sink fail its next write
/// with `err`, which is how a device unplug looks to the writer.
int intoxicated_aaudio_fail_test_sink(int err) {
    PacedNullSink* s = g_last_test_sink.load();
    if (!s) return -1;
    s->fail_with.store(err);
    return 0;
}

}
