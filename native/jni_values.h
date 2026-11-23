#pragma once

// The values the Java world answers the engine with, and the process-wide
// state behind them, with no libjnivm in sight.
//
// Everything here is plain C++ so `jni_values_test.cpp` can check it without
// a JavaVM: the device identity and its profiles, the User-Agent, memory,
// density, locale, the preference store's file name and format, the files
// directory, the signed-in identity, the focused text box, the IME text and
// the host's callbacks. The Java classes in jni_framework.cpp, jni_params.cpp
// and activity.cpp read from here; the host writes through the extern "C"
// setters at the end of jni_values.cpp.
//
// Thread safety: the engine calls in from its own threads (bootstrapTheApp
// runs on one of them while the host is still bringing the window up), so
// every piece of state below is either atomic or behind a mutex.

#include <cstddef>
#include <cstdint>
#include <map>
#include <string>

namespace intoxicated {

// ------------------------------------------------------------ device identity

/// The three device identities a launch can claim.
enum class Profile { PcWindows11, AndroidTablet, RobloxApp };

/// What `INTOXICATED_DEVICE_PROFILE` asked for. `known` is false for a value
/// that names no profile, which falls back to the PC one.
struct ProfileChoice {
    Profile profile;
    bool known;
};

/// The profile a raw switch value selects: trimmed, lower-cased, matched
/// against the names and their aliases. Null and empty select the PC.
ProfileChoice parse_profile(const char* raw);

/// `pc-windows-11`, `android-tablet` or `roblox-app`.
const char* profile_label(Profile profile);

/// This process's profile, read from the environment once and latched. An
/// unknown value is reported to stderr the first time.
Profile device_profile();

/// The device record WebRTC's `BuildInfo` answers from (android_platform.cpp).
/// One record per profile; the only copy of these strings.
struct DeviceRecord {
    const char* brand;
    const char* manufacturer;
    const char* model;
    const char* device;
    const char* product;
    const char* build_id;
    const char* build_type;
    const char* build_release;
    const char* sdk_version;
};
const DeviceRecord& device_record_for(Profile profile);
/// The record for this process's profile.
const DeviceRecord& device_record();

/// The Android API level every profile reports, as `DeviceParams.osVersion`
/// and `Build$VERSION.SDK_INT`. The engine refuses Vulkan below its minimum,
/// and 33 is what the real client on Android 13 sends.
constexpr int kApiLevel = 33;

// ------------------------------------------------------------ User-Agent

/// `a.b.c.d` becomes `a.b.d`; anything else, null included, `0.0.0`.
std::string app_version(const char* engine_version);

/// The User-Agent for a profile from its parts. Pure: no environment.
std::string user_agent_for(Profile profile, long ram_mb, int width, int height, const std::string& version);

/// The User-Agent this process sends, computed now: `INTOXICATED_UA` verbatim
/// when set, else the profile's shape with the stored display size, the
/// memory token and `INTOXICATED_ENGINE_VERSION`.
std::string user_agent();

// ------------------------------------------------------------ memory, density

/// `hw.physmem` in MiB, truncated; -1 when the sysctl fails.
long physmem_mb();

/// The memory token of the User-Agent: `/proc/meminfo`'s MemTotal when that
/// is readable and positive, else `hw.physmem`, else 0.
long user_agent_ram_mb();

/// `DeviceParams.deviceTotalMemoryMB` for a switch value and a measured size
/// (-1 for a failed measurement): `off` 8192, a leading positive integer that
/// integer, otherwise the measurement, otherwise 8192.
int total_memory_mb(const char* switch_value, long measured_mb);

/// `PlatformParams.dpiScale` for a switch value. `*complaint` is set to a line
/// for stderr when the value is present but unusable, and cleared otherwise.
float dpi_scale(const char* switch_value, std::string* complaint);

/// Millimetres for a pixel count at 96 dpi, truncated toward zero.
int millimetres_at_96dpi(int pixels);

/// The locale tag `NativeLocaleJavaInterface` answers, from the values of
/// LC_ALL, LC_MESSAGES and LANG (any may be null).
std::string locale_tag(const char* lc_all, const char* lc_messages, const char* lang);

/// The value of an environment variable when it is set and not empty, else
/// the fallback.
std::string env_or(const char* name, const char* fallback);

// ------------------------------------------------------------ display state

/// The display size the Java world reports. 1280x720 until the host sets it;
/// a size with a side of zero or less is ignored.
void set_display_size(int width, int height);
void display_size(int* width, int* height);

/// The physical size in millimetres; a side of zero or less is stored as 0,
/// which means unknown.
void set_physical_mm(int width_mm, int height_mm);
/// True, with the size, only when both sides are known.
bool physical_mm(int* width_mm, int* height_mm);

/// The host's touchscreen flag: -1 unset (answers no), 0 or 1.
void set_touchscreen(int present);
/// The answer, latched at the first query, which prints one line to stdout.
bool touchscreen();

/// Night mode: -1 unset, above 0 night.
void set_night(int night);
int night();

// ------------------------------------------------------------ preferences

/// The low 32 bits of FNV-1a 64 over the name's bytes, with the offset basis
/// 1469598103934665603. **Not the standard basis**: files already on disk
/// carry digests made with this one, so it may never change.
uint32_t prefs_digest(const std::string& name);

/// `shared_prefs/<safe>-<digest>.prefs`, relative to the working directory.
std::string prefs_relative_path(const std::string& name);

/// One store's file contents and back. Records are `S`, TAB, key, TAB, value
/// with newline and backslash escaped, sorted by key.
std::string prefs_serialize(const std::map<std::string, std::string>& values);
std::map<std::string, std::string> prefs_parse(const std::string& text);

/// The leading number of a stored text, as `getInt`, `getLong` and
/// `getFloat` read it. False when there is none or it is out of range.
bool leading_long(const std::string& text, long long* out);
bool leading_int(const std::string& text, int* out);
bool leading_float(const std::string& text, float* out);

/// `putFloat`'s text: fixed notation with six decimals.
std::string float_text(float value);

// ------------------------------------------------------------ files directory

/// What the host says the files directory is. Ignored once it has been used.
void set_files_dir(const char* dir);
/// The files directory, latched on first use.
std::string files_dir();

// ------------------------------------------------------------ identity

/// The signed-in user as the host last published it. "Signed in" is `known`,
/// not a non-zero id. Nothing in here is ever printed.
struct Identity {
    bool known = false;
    int64_t user_id = 0;
    std::string username;
    std::string display_name;
    int64_t membership_type = 0;
    bool under13 = false;
    bool subscription = false;
};
void identity_publish(int64_t user_id, const char* username, const char* display_name, int64_t membership_type,
                      bool under13, bool subscription);
void identity_clear();
Identity identity();

// ------------------------------------------------------------ text box

/// `NativeTextBoxInfo`'s fifteen constructor values, in argument order. Laid
/// out as the host's `RawTextBoxInfo` mirror: five floats, then ten 32-bit
/// ints (booleans as 0 or 1).
struct TextBoxSpec {
    float x, y, width, height, font_size;
    int32_t multiline, x_alignment, y_alignment, text_color, font, text_input_type, return_key_type,
        manual_focus_release, text_wrapped, fifteenth;
};
static_assert(sizeof(TextBoxSpec) == 60, "TextBoxSpec is fifteen 32-bit slots");
static_assert(alignof(TextBoxSpec) == 4, "TextBoxSpec is 4-byte aligned");

/// Where a focus's spec came from, for the trace.
enum class SpecSource { Given, LastBuilt, None };

/// A box took focus: its handle, its text and the spec it came with (null for
/// none, which falls back to the last built spec). Bumps the generation.
SpecSource textbox_focus(int64_t handle, const std::string& text, const TextBoxSpec* spec);
/// Focus left: handle 0, spec unknown, text kept. Bumps the generation.
void textbox_blur();
/// Every `NativeTextBoxInfo` construction.
void textbox_record_built(const TextBoxSpec& spec);
int64_t textbox_handle();
uint32_t textbox_generation();
/// True and `*out` filled only when the focused box's spec is known.
bool textbox_spec(TextBoxSpec* out);
std::string textbox_text();

// ------------------------------------------------------------ IME

/// What the engine last reported through `InputConnection`.
void ime_set_state(const std::string& text, int selection_start, int selection_end, int composing_start,
                   int composing_end);
void ime_restart();
void ime_set_active(bool active);
uint32_t ime_generation();
std::string ime_text();
void ime_selection(int* start, int* end);

// ------------------------------------------------------------ host callbacks

/// The host's callbacks, each replaceable at any time and null when nobody
/// listens. Read without holding any lock.
using TextSink = void (*)(const char*);
using BareSink = void (*)();
TextSink login_sink();
BareSink logout_sink();
TextSink app_ready_sink();
BareSink home_interactive_sink();
BareSink bootstrap_hook();

/// A place finished loading (either engine callback).
void note_game_loaded(int64_t place);
uint32_t games_loaded();

// ------------------------------------------------------------ diagnostics

/// A switch that is on when the variable is set at all, read once.
bool trace_param_reads();
bool trace_identity();
bool trace_base_url();
bool trace_class_loader();
bool trace_text();

/// `text` cut at the first `?` or `#`, with what was cut named as elided.
/// Null prints as `(null)`.
std::string url_without_query(const char* url);

/// Copy `text` into a caller's buffer of `len` bytes, truncated to `len - 1`
/// and NUL-terminated. Returns the bytes copied, not counting the NUL.
int copy_out(const std::string& text, char* buf, int len);

} // namespace intoxicated
