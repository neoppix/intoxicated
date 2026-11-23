// The values and state behind the Java world. See jni_values.h.

#include "jni_values.h"

#include <sys/stat.h>
#include <sys/sysctl.h>
#include <sys/types.h>

#include <atomic>
#include <cctype>
#include <cerrno>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <mutex>
#include <optional>
#include <xlocale.h>

namespace intoxicated {

namespace {

std::string trimmed_lower(const char* raw) {
    std::string s = raw ? raw : "";
    size_t start = 0;
    while (start < s.size() && std::isspace(static_cast<unsigned char>(s[start]))) {
        ++start;
    }
    size_t end = s.size();
    while (end > start && std::isspace(static_cast<unsigned char>(s[end - 1]))) {
        --end;
    }
    s = s.substr(start, end - start);
    for (auto& c : s) {
        if (c >= 'A' && c <= 'Z') {
            c = static_cast<char>(c - 'A' + 'a');
        }
    }
    return s;
}

/// The "C" locale, for number formatting and parsing that must not follow
/// whatever locale something else in the process selected.
locale_t c_locale() {
    static locale_t loc = newlocale(LC_ALL_MASK, "C", nullptr);
    return loc;
}

bool switch_on(const char* name) {
    return getenv(name) != nullptr;
}

} // namespace

// ------------------------------------------------------------ device identity

ProfileChoice parse_profile(const char* raw) {
    const std::string s = trimmed_lower(raw);
    if (s.empty() || s == "pc" || s == "pc-windows-11" || s == "windows" || s == "windows-11") {
        return {Profile::PcWindows11, true};
    }
    if (s == "android-tablet" || s == "android" || s == "tablet") {
        return {Profile::AndroidTablet, true};
    }
    if (s == "roblox-app" || s == "app" || s == "roblox") {
        return {Profile::RobloxApp, true};
    }
    return {Profile::PcWindows11, false};
}

const char* profile_label(Profile profile) {
    switch (profile) {
    case Profile::AndroidTablet:
        return "android-tablet";
    case Profile::RobloxApp:
        return "roblox-app";
    case Profile::PcWindows11:
        break;
    }
    return "pc-windows-11";
}

Profile device_profile() {
    static const Profile profile = [] {
        const char* raw = getenv("INTOXICATED_DEVICE_PROFILE");
        const ProfileChoice choice = parse_profile(raw);
        if (!choice.known) {
            fprintf(stderr,
                    "[intoxicated] INTOXICATED_DEVICE_PROFILE=\"%s\" is not a device profile; using "
                    "pc-windows-11. Known: roblox-app, android-tablet, pc-windows-11\n",
                    raw ? raw : "");
        }
        return choice.profile;
    }();
    return profile;
}

const DeviceRecord& device_record_for(Profile profile) {
    // brand, manufacturer, model, device, product, build id, build type,
    // build release, sdk version.
    static const DeviceRecord pc = {"Intoxicated", "Intoxicated", "Windows 11 PC", "intoxicated_pc",
                                    "intoxicated_pc", "intoxicated", "user", "11", "33"};
    static const DeviceRecord tablet = {"Intoxicated", "Intoxicated", "Intoxicated", "intoxicated",
                                        "intoxicated", "intoxicated", "user", "13", "33"};
    static const DeviceRecord app = {"Intoxicated", "Intoxicated", "Intoxicated", "intoxicated",
                                     "intoxicated", "intoxicated", "user", "13", "33"};
    switch (profile) {
    case Profile::AndroidTablet:
        return tablet;
    case Profile::RobloxApp:
        return app;
    case Profile::PcWindows11:
        break;
    }
    return pc;
}

const DeviceRecord& device_record() {
    return device_record_for(device_profile());
}

// ------------------------------------------------------------ User-Agent

std::string app_version(const char* engine_version) {
    if (!engine_version) {
        return "0.0.0";
    }
    std::string parts[4];
    size_t count = 0;
    std::string current;
    for (const char* p = engine_version;; ++p) {
        if (*p == '.' || *p == '\0') {
            if (count == 4 || current.empty()) {
                return "0.0.0";
            }
            parts[count++] = current;
            current.clear();
            if (*p == '\0') {
                break;
            }
        } else {
            current.push_back(*p);
        }
    }
    if (count != 4) {
        return "0.0.0";
    }
    return parts[0] + "." + parts[1] + "." + parts[3];
}

std::string user_agent_for(Profile profile, long ram_mb, int width, int height, const std::string& version) {
    const std::string size = std::to_string(width) + "x" + std::to_string(height);
    const std::string ram = std::to_string(ram_mb) + "MB";
    switch (profile) {
    case Profile::RobloxApp:
        return "RobloxApp/" + version + "(GlobalDist; Intoxicated)";
    case Profile::AndroidTablet:
        return "Mozilla/5.0 (" + ram + "; " + size + "; 160x160; " + size +
               "; Intoxicated; 33) AppleWebKit/537.36 (KHTML, like Gecko)  ROBLOX Android App " + version +
               " Tablet Hybrid()  GooglePlayStore RobloxApp/" + version + " (GlobalDist; GooglePlayStore)";
    case Profile::PcWindows11:
        break;
    }
    return "Mozilla/5.0 (" + ram + "; " + size + "; 160x160; " + size +
           "; Intoxicated) AppleWebKit/537.36 (KHTML, like Gecko)  ROBLOX Windows App " + version +
           " Desktop Hybrid()  Intoxicated RobloxApp/" + version + " (GlobalDist; Intoxicated)";
}

std::string user_agent() {
    const char* verbatim = getenv("INTOXICATED_UA");
    if (verbatim && *verbatim) {
        return verbatim;
    }
    int width = 0;
    int height = 0;
    display_size(&width, &height);
    return user_agent_for(device_profile(), user_agent_ram_mb(), width, height,
                          app_version(getenv("INTOXICATED_ENGINE_VERSION")));
}

// ------------------------------------------------------------ memory, density

long physmem_mb() {
    uint64_t bytes = 0;
    size_t len = sizeof(bytes);
    if (sysctlbyname("hw.physmem", &bytes, &len, nullptr, 0) != 0 || len == 0) {
        return -1;
    }
    if (len == sizeof(uint32_t)) {
        uint32_t narrow = 0;
        memcpy(&narrow, &bytes, sizeof(narrow));
        bytes = narrow;
    }
    return static_cast<long>(bytes / 1048576);
}

long user_agent_ram_mb() {
    std::ifstream meminfo("/proc/meminfo");
    std::string line;
    while (meminfo && std::getline(meminfo, line)) {
        if (line.compare(0, 9, "MemTotal:") == 0) {
            const long kb = strtol(line.c_str() + 9, nullptr, 10);
            if (kb > 0) {
                return kb / 1024;
            }
            break;
        }
    }
    const long mb = physmem_mb();
    return mb > 0 ? mb : 0;
}

int total_memory_mb(const char* switch_value, long measured_mb) {
    if (switch_value && strcmp(switch_value, "off") == 0) {
        return 8192;
    }
    if (switch_value) {
        long long value = 0;
        const char* p = switch_value;
        bool digits = false;
        while (*p >= '0' && *p <= '9') {
            digits = true;
            value = value * 10 + (*p - '0');
            if (value > 2147483647LL) {
                digits = false;
                break;
            }
            ++p;
        }
        if (digits && value > 0) {
            return static_cast<int>(value);
        }
    }
    if (measured_mb > 0) {
        return measured_mb > 2147483647L ? 2147483647 : static_cast<int>(measured_mb);
    }
    return 8192;
}

float dpi_scale(const char* switch_value, std::string* complaint) {
    if (complaint) {
        complaint->clear();
    }
    if (!switch_value || !*switch_value) {
        return 1.0f;
    }
    const std::string text = trimmed_lower(switch_value);
    char* end = nullptr;
    const float value = text.empty() ? NAN : strtof_l(text.c_str(), &end, c_locale());
    const bool whole = !text.empty() && end && *end == '\0';
    if (whole && std::isfinite(value) && value > 0.0f && value <= 8.0f) {
        return value;
    }
    if (complaint) {
        *complaint = std::string("[android] INTOXICATED_DPI_SCALE=") + switch_value +
                     " is not a scale between 0 and 8; using 1.0";
    }
    return 1.0f;
}

int millimetres_at_96dpi(int pixels) {
    // 25.4 mm per inch over 96 pixels per inch, in integers so the truncation
    // is exact: 25.4 / 96 == 254 / 960.
    return static_cast<int>(static_cast<long long>(pixels) * 254 / 960);
}

std::string locale_tag(const char* lc_all, const char* lc_messages, const char* lang) {
    const char* chosen = nullptr;
    for (const char* candidate : {lc_all, lc_messages, lang}) {
        if (candidate && *candidate) {
            chosen = candidate;
            break;
        }
    }
    if (!chosen) {
        return "en_us";
    }
    std::string tag = chosen;
    const size_t dot = tag.find('.');
    if (dot != std::string::npos) {
        tag.erase(dot);
    }
    if (tag.empty() || tag == "C" || tag == "POSIX") {
        return "en_us";
    }
    for (auto& c : tag) {
        if (c >= 'A' && c <= 'Z') {
            c = static_cast<char>(c - 'A' + 'a');
        }
    }
    return tag;
}

std::string env_or(const char* name, const char* fallback) {
    const char* value = getenv(name);
    return value && *value ? value : fallback;
}

// ------------------------------------------------------------ display state

namespace {

uint64_t pack(int a, int b) {
    return (static_cast<uint64_t>(static_cast<uint32_t>(a)) << 32) | static_cast<uint32_t>(b);
}
void unpack(uint64_t v, int* a, int* b) {
    if (a) {
        *a = static_cast<int>(static_cast<uint32_t>(v >> 32));
    }
    if (b) {
        *b = static_cast<int>(static_cast<uint32_t>(v));
    }
}

std::atomic<uint64_t> g_display{pack(1280, 720)};
std::atomic<uint64_t> g_physical{pack(0, 0)};
std::atomic<int> g_touchscreen{-1};
std::atomic<int> g_night{-1};

} // namespace

void set_display_size(int width, int height) {
    if (width <= 0 || height <= 0) {
        return;
    }
    g_display.store(pack(width, height), std::memory_order_release);
}

void display_size(int* width, int* height) {
    unpack(g_display.load(std::memory_order_acquire), width, height);
}

void set_physical_mm(int width_mm, int height_mm) {
    g_physical.store(pack(width_mm > 0 ? width_mm : 0, height_mm > 0 ? height_mm : 0), std::memory_order_release);
}

bool physical_mm(int* width_mm, int* height_mm) {
    int w = 0;
    int h = 0;
    unpack(g_physical.load(std::memory_order_acquire), &w, &h);
    if (w <= 0 || h <= 0) {
        return false;
    }
    if (width_mm) {
        *width_mm = w;
    }
    if (height_mm) {
        *height_mm = h;
    }
    return true;
}

void set_touchscreen(int present) {
    g_touchscreen.store(present < 0 ? -1 : (present ? 1 : 0), std::memory_order_release);
}

bool touchscreen() {
    static const bool answer = [] {
        const int flag = g_touchscreen.load(std::memory_order_acquire);
        const bool yes = flag > 0;
        printf("[android] host touchscreen: %s (%s)\n", yes ? "yes" : "no",
               flag < 0 ? "no display backend reported a seat" : "reported by the display backend");
        fflush(stdout);
        return yes;
    }();
    return answer;
}

void set_night(int night) {
    g_night.store(night, std::memory_order_release);
}

int night() {
    return g_night.load(std::memory_order_acquire);
}

// ------------------------------------------------------------ preferences

uint32_t prefs_digest(const std::string& name) {
    uint64_t hash = 1469598103934665603ULL;
    for (unsigned char byte : name) {
        hash ^= byte;
        hash *= 1099511628211ULL;
    }
    return static_cast<uint32_t>(hash);
}

std::string prefs_relative_path(const std::string& name) {
    const size_t slash = name.rfind('/');
    std::string safe = slash == std::string::npos ? name : name.substr(slash + 1);
    for (auto& c : safe) {
        const bool ok = (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '.' ||
                        c == '_' || c == '-';
        if (!ok) {
            c = '_';
        }
    }
    if (safe.empty()) {
        safe = "prefs";
    }
    char digest[9];
    snprintf(digest, sizeof(digest), "%08x", prefs_digest(name));
    return "shared_prefs/" + safe + "-" + digest + ".prefs";
}

std::string prefs_serialize(const std::map<std::string, std::string>& values) {
    std::string out;
    for (const auto& [key, value] : values) {
        out += "S\t";
        out += key;
        out += '\t';
        for (char c : value) {
            if (c == '\n') {
                out += "\\n";
            } else if (c == '\\') {
                out += "\\\\";
            } else {
                out += c;
            }
        }
        out += '\n';
    }
    return out;
}

std::map<std::string, std::string> prefs_parse(const std::string& text) {
    std::map<std::string, std::string> values;
    size_t start = 0;
    while (start < text.size()) {
        size_t end = text.find('\n', start);
        if (end == std::string::npos) {
            end = text.size();
        }
        const std::string line = text.substr(start, end - start);
        start = end + 1;
        const size_t first = line.find('\t');
        if (first == std::string::npos) {
            continue;
        }
        const size_t second = line.find('\t', first + 1);
        if (second == std::string::npos) {
            continue;
        }
        const std::string key = line.substr(first + 1, second - first - 1);
        std::string value;
        for (size_t i = second + 1; i < line.size(); ++i) {
            if (line[i] == '\\' && i + 1 < line.size()) {
                ++i;
                value += line[i] == 'n' ? '\n' : line[i];
            } else {
                value += line[i];
            }
        }
        values[key] = value;
    }
    return values;
}

bool leading_long(const std::string& text, long long* out) {
    const char* begin = text.c_str();
    char* end = nullptr;
    errno = 0;
    const long long value = strtoll_l(begin, &end, 10, c_locale());
    if (end == begin || errno == ERANGE) {
        return false;
    }
    if (out) {
        *out = value;
    }
    return true;
}

bool leading_int(const std::string& text, int* out) {
    long long value = 0;
    if (!leading_long(text, &value) || value < -2147483648LL || value > 2147483647LL) {
        return false;
    }
    if (out) {
        *out = static_cast<int>(value);
    }
    return true;
}

bool leading_float(const std::string& text, float* out) {
    const char* begin = text.c_str();
    char* end = nullptr;
    errno = 0;
    const float value = strtof_l(begin, &end, c_locale());
    if (end == begin || errno == ERANGE) {
        return false;
    }
    if (out) {
        *out = value;
    }
    return true;
}

std::string float_text(float value) {
    char buf[64];
    snprintf_l(buf, sizeof(buf), c_locale(), "%.6f", static_cast<double>(value));
    return buf;
}

// ------------------------------------------------------------ files directory

namespace {

std::mutex g_files_mutex;
bool g_files_latched = false;
bool g_files_host_set = false;
std::string g_files_host;
std::string g_files_dir;

/// `mkdir -p` with mode 0700 for whatever it creates.
void make_dirs(const std::string& path) {
    for (size_t i = 1; i <= path.size(); ++i) {
        if (i == path.size() || path[i] == '/') {
            const std::string prefix = path.substr(0, i);
            if (!prefix.empty()) {
                mkdir(prefix.c_str(), 0700);
            }
        }
    }
}

} // namespace

void set_files_dir(const char* dir) {
    std::lock_guard<std::mutex> lock(g_files_mutex);
    if (g_files_latched) {
        return;
    }
    g_files_host = dir ? dir : "";
    g_files_host_set = true;
}

std::string files_dir() {
    std::lock_guard<std::mutex> lock(g_files_mutex);
    if (!g_files_latched) {
        g_files_latched = true;
        if (const char* env = getenv("INTOXICATED_FILES_DIR")) {
            g_files_dir = env;
        } else if (g_files_host_set) {
            g_files_dir = g_files_host;
        } else {
            const char* xdg = getenv("XDG_DATA_HOME");
            const char* home = getenv("HOME");
            std::string base;
            if (xdg && *xdg) {
                base = xdg;
            } else if (home && *home) {
                base = std::string(home) + "/.local/share";
            } else {
                base = "/tmp";
            }
            g_files_dir = base + "/intoxicated/instances/default/data";
            make_dirs(g_files_dir);
        }
    }
    return g_files_dir;
}

// ------------------------------------------------------------ identity

namespace {
std::mutex g_identity_mutex;
Identity g_identity;
} // namespace

void identity_publish(int64_t user_id, const char* username, const char* display_name, int64_t membership_type,
                      bool under13, bool subscription) {
    std::lock_guard<std::mutex> lock(g_identity_mutex);
    g_identity.known = true;
    g_identity.user_id = user_id;
    g_identity.username = username ? username : "";
    g_identity.display_name = display_name ? display_name : "";
    g_identity.membership_type = membership_type;
    g_identity.under13 = under13;
    g_identity.subscription = subscription;
}

void identity_clear() {
    std::lock_guard<std::mutex> lock(g_identity_mutex);
    g_identity = Identity{};
}

Identity identity() {
    std::lock_guard<std::mutex> lock(g_identity_mutex);
    return g_identity;
}

// ------------------------------------------------------------ text box

namespace {

std::mutex g_textbox_mutex;
std::atomic<int64_t> g_textbox_handle{0};
std::atomic<uint32_t> g_textbox_generation{0};
std::optional<TextBoxSpec> g_textbox_spec;
std::optional<TextBoxSpec> g_textbox_last_built;
std::string g_textbox_text;

void trace_spec(const TextBoxSpec& s, const char* source) {
    fprintf(stderr,
            "[text] spec (%s): x=%.1f y=%.1f w=%.1f h=%.1f fontSize=%.1f multiline=%d xAlign=%d yAlign=%d "
            "textColor=0x%08x font=%d inputType=%d returnKey=%d manualFocusRelease=%d wrapped=%d slot14=%d\n",
            source, static_cast<double>(s.x), static_cast<double>(s.y), static_cast<double>(s.width),
            static_cast<double>(s.height), static_cast<double>(s.font_size), s.multiline, s.x_alignment,
            s.y_alignment, static_cast<unsigned>(s.text_color), s.font, s.text_input_type, s.return_key_type,
            s.manual_focus_release, s.text_wrapped, s.fifteenth);
}

} // namespace

SpecSource textbox_focus(int64_t handle, const std::string& text, const TextBoxSpec* spec) {
    SpecSource source = SpecSource::None;
    {
        std::lock_guard<std::mutex> lock(g_textbox_mutex);
        g_textbox_text = text;
        if (spec) {
            g_textbox_spec = *spec;
            source = SpecSource::Given;
        } else if (g_textbox_last_built) {
            g_textbox_spec = g_textbox_last_built;
            source = SpecSource::LastBuilt;
        } else {
            g_textbox_spec.reset();
        }
        g_textbox_handle.store(handle, std::memory_order_release);
        g_textbox_generation.fetch_add(1, std::memory_order_acq_rel);
        if (trace_text()) {
            fprintf(stderr, "[text] focus: handle %lld, %zu bytes\n", static_cast<long long>(handle), text.size());
            if (g_textbox_spec) {
                trace_spec(*g_textbox_spec, source == SpecSource::Given ? "from showKeyboard" : "from the last constructor");
            } else {
                fprintf(stderr, "[text] spec unavailable\n");
            }
        }
    }
    return source;
}

void textbox_blur() {
    std::lock_guard<std::mutex> lock(g_textbox_mutex);
    g_textbox_spec.reset();
    g_textbox_handle.store(0, std::memory_order_release);
    g_textbox_generation.fetch_add(1, std::memory_order_acq_rel);
    if (trace_text()) {
        fprintf(stderr, "[text] blur\n");
    }
}

void textbox_record_built(const TextBoxSpec& spec) {
    std::lock_guard<std::mutex> lock(g_textbox_mutex);
    g_textbox_last_built = spec;
}

int64_t textbox_handle() {
    return g_textbox_handle.load(std::memory_order_acquire);
}

uint32_t textbox_generation() {
    return g_textbox_generation.load(std::memory_order_acquire);
}

bool textbox_spec(TextBoxSpec* out) {
    std::lock_guard<std::mutex> lock(g_textbox_mutex);
    if (!g_textbox_spec) {
        return false;
    }
    if (out) {
        *out = *g_textbox_spec;
    }
    return true;
}

std::string textbox_text() {
    std::lock_guard<std::mutex> lock(g_textbox_mutex);
    return g_textbox_text;
}

// ------------------------------------------------------------ IME

namespace {
std::mutex g_ime_mutex;
std::string g_ime_text;
int g_ime_start = 0;
int g_ime_end = 0;
int g_ime_composing_start = -1;
int g_ime_composing_end = -1;
std::atomic<uint32_t> g_ime_generation{0};
std::atomic<bool> g_ime_active{false};
} // namespace

void ime_set_state(const std::string& text, int selection_start, int selection_end, int composing_start,
                   int composing_end) {
    {
        std::lock_guard<std::mutex> lock(g_ime_mutex);
        g_ime_text = text;
        g_ime_start = selection_start;
        g_ime_end = selection_end;
        g_ime_composing_start = composing_start;
        g_ime_composing_end = composing_end;
    }
    g_ime_generation.fetch_add(1, std::memory_order_acq_rel);
}

void ime_restart() {
    g_ime_generation.fetch_add(1, std::memory_order_acq_rel);
}

void ime_set_active(bool active) {
    g_ime_active.store(active, std::memory_order_release);
}

uint32_t ime_generation() {
    return g_ime_generation.load(std::memory_order_acquire);
}

std::string ime_text() {
    std::lock_guard<std::mutex> lock(g_ime_mutex);
    return g_ime_text;
}

void ime_selection(int* start, int* end) {
    std::lock_guard<std::mutex> lock(g_ime_mutex);
    if (start) {
        *start = g_ime_start;
    }
    if (end) {
        *end = g_ime_end;
    }
}

// ------------------------------------------------------------ host callbacks

namespace {
std::atomic<TextSink> g_login_sink{nullptr};
std::atomic<BareSink> g_logout_sink{nullptr};
std::atomic<TextSink> g_app_ready_sink{nullptr};
std::atomic<BareSink> g_home_sink{nullptr};
std::atomic<BareSink> g_bootstrap{nullptr};
std::atomic<uint32_t> g_games_loaded{0};
std::atomic<int64_t> g_last_place{0};
} // namespace

TextSink login_sink() {
    return g_login_sink.load(std::memory_order_acquire);
}
BareSink logout_sink() {
    return g_logout_sink.load(std::memory_order_acquire);
}
TextSink app_ready_sink() {
    return g_app_ready_sink.load(std::memory_order_acquire);
}
BareSink home_interactive_sink() {
    return g_home_sink.load(std::memory_order_acquire);
}
BareSink bootstrap_hook() {
    return g_bootstrap.load(std::memory_order_acquire);
}

void note_game_loaded(int64_t place) {
    g_last_place.store(place, std::memory_order_release);
    g_games_loaded.fetch_add(1, std::memory_order_acq_rel);
}

uint32_t games_loaded() {
    return g_games_loaded.load(std::memory_order_acquire);
}

// ------------------------------------------------------------ diagnostics

bool trace_param_reads() {
    static const bool on = switch_on("INTOXICATED_TRACE_PARAM_READS");
    return on;
}
bool trace_identity() {
    static const bool on = switch_on("INTOXICATED_TRACE_IDENTITY");
    return on;
}
bool trace_base_url() {
    static const bool on = switch_on("INTOXICATED_TRACE_BASEURL");
    return on;
}
bool trace_class_loader() {
    static const bool on = switch_on("INTOXICATED_TRACE_CLASSLOADER");
    return on;
}
bool trace_text() {
    // Shared with the runtime's input module, which reads the same variable.
    static const bool on = switch_on("INTOXICATED_TRACE_TEXT");
    return on;
}

std::string url_without_query(const char* url) {
    if (!url) {
        return "(null)";
    }
    std::string text = url;
    const size_t cut = text.find_first_of("?#");
    if (cut == std::string::npos) {
        return text;
    }
    const bool query = text[cut] == '?';
    text.erase(cut);
    return text + (query ? "?<query elided>" : "#<fragment elided>");
}

int copy_out(const std::string& text, char* buf, int len) {
    if (!buf || len <= 0) {
        return 0;
    }
    const size_t n = text.size() < static_cast<size_t>(len - 1) ? text.size() : static_cast<size_t>(len - 1);
    memcpy(buf, text.data(), n);
    buf[n] = '\0';
    return static_cast<int>(n);
}

} // namespace intoxicated

// ------------------------------------------------------------ C ABI

using namespace intoxicated;

extern "C" {

void intoxicated_set_display_size(int width, int height) {
    set_display_size(width, height);
}

void intoxicated_set_display_physical_mm(int width_mm, int height_mm) {
    set_physical_mm(width_mm, height_mm);
}

void intoxicated_set_touchscreen_present(int present) {
    set_touchscreen(present);
}

void intoxicated_set_ui_mode_night(int night_mode) {
    set_night(night_mode);
}

void intoxicated_set_files_dir(const char* dir) {
    set_files_dir(dir);
}

/// The User-Agent into `buf`, truncated to `n - 1` bytes and NUL-terminated.
/// Returns the full length, so a short buffer can be told from a short value.
size_t intoxicated_build_user_agent(char* buf, size_t n) {
    const std::string ua = user_agent();
    if (buf && n > 0) {
        const size_t copied = ua.size() < n - 1 ? ua.size() : n - 1;
        memcpy(buf, ua.data(), copied);
        buf[copied] = '\0';
    }
    return ua.size();
}

void intoxicated_set_bootstrap(void (*hook)()) {
    g_bootstrap.store(hook, std::memory_order_release);
}

void intoxicated_identity_set_sinks(void (*on_login)(const char*), void (*on_logout)()) {
    g_login_sink.store(on_login, std::memory_order_release);
    g_logout_sink.store(on_logout, std::memory_order_release);
}

void intoxicated_app_ready_set_sink(void (*sink)(const char*)) {
    g_app_ready_sink.store(sink, std::memory_order_release);
}

void intoxicated_home_interactive_set_sink(void (*sink)()) {
    g_home_sink.store(sink, std::memory_order_release);
}

void intoxicated_identity_publish(int64_t user_id, const char* username, const char* display_name,
                                  int64_t membership_type, int is_under13, int has_subscription) {
    identity_publish(user_id, username, display_name, membership_type, is_under13 != 0, has_subscription != 0);
}

void intoxicated_identity_clear() {
    identity_clear();
}

int64_t intoxicated_textbox_handle() {
    return textbox_handle();
}

uint32_t intoxicated_textbox_generation() {
    return textbox_generation();
}

/// 1 with the focused box's spec in `*out`, 0 with `*out` untouched when no
/// spec is known.
int intoxicated_textbox_info(TextBoxSpec* out) {
    TextBoxSpec spec{};
    if (!textbox_spec(&spec)) {
        return 0;
    }
    if (out) {
        *out = spec;
    }
    return 1;
}

int intoxicated_textbox_text(char* buf, int len) {
    return copy_out(textbox_text(), buf, len);
}

/// Test seam: focus `handle` with a spec built from fifteen separate scalars,
/// in `NativeTextBoxInfo`'s argument order.
void intoxicated_textbox_test_focus(int64_t handle, const char* text, float s0, float s1, float s2, float s3, float s4,
                                    int s5, int s6, int s7, int s8, int s9, int s10, int s11, int s12, int s13,
                                    int s14) {
    TextBoxSpec spec{};
    spec.x = s0;
    spec.y = s1;
    spec.width = s2;
    spec.height = s3;
    spec.font_size = s4;
    spec.multiline = s5;
    spec.x_alignment = s6;
    spec.y_alignment = s7;
    spec.text_color = s8;
    spec.font = s9;
    spec.text_input_type = s10;
    spec.return_key_type = s11;
    spec.manual_focus_release = s12;
    spec.text_wrapped = s13;
    spec.fifteenth = s14;
    textbox_focus(handle, text ? text : "", &spec);
}

/// Test seams: focus and blur as `showKeyboard` and `hideKeyboard` would.
void intoxicated_textbox_focused(int64_t handle, const char* text, const TextBoxSpec* spec) {
    textbox_focus(handle, text ? text : "", spec);
}

void intoxicated_textbox_blurred() {
    textbox_blur();
}

uint32_t intoxicated_ime_state_generation() {
    return ime_generation();
}

int intoxicated_ime_state_text(char* buf, int len) {
    return copy_out(ime_text(), buf, len);
}

void intoxicated_ime_state_selection(int* start, int* end) {
    ime_selection(start, end);
}

uint32_t intoxicated_games_loaded() {
    return games_loaded();
}

} // extern "C"
