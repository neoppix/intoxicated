// Checks for jni_values.cpp, run on every native build (CMakeLists.txt): the
// preference file names the files on disk depend on, the identity each
// device profile claims, the User-Agent, and the small rules around them.
// The vectors are the spec's (section 5.2) with the branding of this client.

#include "jni_values.h"

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>

using namespace intoxicated;

namespace {

int g_failures = 0;
int g_checks = 0;

void check(bool ok, const char* what, int line) {
    ++g_checks;
    if (!ok) {
        ++g_failures;
        fprintf(stderr, "jni_values_test:%d: FAILED: %s\n", line, what);
    }
}

#define CHECK(cond) check((cond), #cond, __LINE__)
#define CHECK_EQ(a, b) check((a) == (b), #a " == " #b, __LINE__)

void preference_files() {
    CHECK_EQ(prefs_digest(""), 0x739d0383u);
    CHECK_EQ(prefs_relative_path(""), std::string("shared_prefs/prefs-739d0383.prefs"));
    CHECK_EQ(prefs_relative_path("default"), std::string("shared_prefs/default-f2e375f4.prefs"));
    CHECK_EQ(prefs_relative_path("GlobalBasicSettings_13.xml"),
             std::string("shared_prefs/GlobalBasicSettings_13.xml-d42b83cb.prefs"));
    CHECK_EQ(prefs_relative_path("/x/GlobalBasicSettings_13.xml"),
             std::string("shared_prefs/GlobalBasicSettings_13.xml-c9c12e15.prefs"));
    // The engine asks by full path: only the last part names the file, and
    // the digest covers the whole name.
    CHECK_EQ(prefs_relative_path("/data/files/appData/GlobalBasicSettings_13.xml"),
             std::string("shared_prefs/GlobalBasicSettings_13.xml-04389411.prefs"));
    // Characters outside the safe set become underscores; an empty last part
    // is `prefs`.
    CHECK_EQ(prefs_relative_path("a b:c"), std::string("shared_prefs/a_b_c-da44dce7.prefs"));
    CHECK_EQ(prefs_relative_path("dir/"), std::string("shared_prefs/prefs-2e40c1f7.prefs"));

    std::map<std::string, std::string> values{{"k", "a\nb\\c"}, {"channel", ""}};
    const std::string text = prefs_serialize(values);
    CHECK_EQ(text, std::string("S\tchannel\t\nS\tk\ta\\nb\\\\c\n"));
    CHECK(prefs_parse(text) == values);
    // The measured file: one channel record, 11 bytes.
    CHECK_EQ(prefs_serialize({{"channel", ""}}).size(), size_t{11});
    // Lines without two tabs are skipped; the tag is not checked; a backslash
    // before anything but n is that character.
    const auto parsed = prefs_parse("junk\nX\tkey\tv\\tx\nS\tonly-one-tab\n");
    CHECK_EQ(parsed.size(), size_t{1});
    CHECK_EQ(parsed.at("key"), std::string("vtx"));

    int i = 0;
    CHECK(leading_int("12x", &i) && i == 12);
    CHECK(leading_int("  -7", &i) && i == -7);
    CHECK(!leading_int("x12", &i));
    CHECK(!leading_int("99999999999", &i));
    long long l = 0;
    CHECK(leading_long("99999999999", &l) && l == 99999999999LL);
    float f = 0;
    CHECK(leading_float("1.5abc", &f) && f == 1.5f);
    CHECK(!leading_float("abc", &f));
    CHECK_EQ(float_text(1.5f), std::string("1.500000"));
}

void profiles() {
    CHECK(parse_profile(nullptr).profile == Profile::PcWindows11 && parse_profile(nullptr).known);
    CHECK(parse_profile("").profile == Profile::PcWindows11 && parse_profile("  ").known);
    CHECK(parse_profile("windows").profile == Profile::PcWindows11);
    CHECK(parse_profile("pc").profile == Profile::PcWindows11);
    CHECK(parse_profile("windows-11").profile == Profile::PcWindows11);
    CHECK(parse_profile(" Android-Tablet ").profile == Profile::AndroidTablet);
    CHECK(parse_profile("tablet").profile == Profile::AndroidTablet);
    CHECK(parse_profile("android").profile == Profile::AndroidTablet);
    CHECK(parse_profile("roblox-app").profile == Profile::RobloxApp);
    CHECK(parse_profile("app").profile == Profile::RobloxApp);
    CHECK(parse_profile("roblox").profile == Profile::RobloxApp);
    CHECK(parse_profile("tablett").profile == Profile::PcWindows11 && !parse_profile("tablett").known);
    CHECK_EQ(std::string(profile_label(Profile::PcWindows11)), std::string("pc-windows-11"));
    CHECK_EQ(std::string(profile_label(Profile::AndroidTablet)), std::string("android-tablet"));
    CHECK_EQ(std::string(profile_label(Profile::RobloxApp)), std::string("roblox-app"));

    // The device record each profile claims.
    const DeviceRecord& pc = device_record_for(Profile::PcWindows11);
    const DeviceRecord& tablet = device_record_for(Profile::AndroidTablet);
    const DeviceRecord& app = device_record_for(Profile::RobloxApp);
    for (const DeviceRecord* r : {&pc, &tablet, &app}) {
        CHECK_EQ(std::string(r->brand), std::string("Intoxicated"));
        CHECK_EQ(std::string(r->manufacturer), std::string("Intoxicated"));
        CHECK_EQ(std::string(r->build_id), std::string("intoxicated"));
        CHECK_EQ(std::string(r->build_type), std::string("user"));
        CHECK_EQ(std::string(r->sdk_version), std::string("33"));
    }
    CHECK_EQ(std::string(pc.model), std::string("Windows 11 PC"));
    CHECK_EQ(std::string(pc.device), std::string("intoxicated_pc"));
    CHECK_EQ(std::string(pc.product), std::string("intoxicated_pc"));
    CHECK_EQ(std::string(pc.build_release), std::string("11"));
    CHECK_EQ(std::string(tablet.model), std::string("Intoxicated"));
    CHECK_EQ(std::string(tablet.device), std::string("intoxicated"));
    CHECK_EQ(std::string(tablet.build_release), std::string("13"));
    CHECK_EQ(std::string(app.product), std::string("intoxicated"));
    CHECK_EQ(std::string(app.build_release), std::string("13"));
    // The API level is a measured engine gate and is the same everywhere.
    CHECK_EQ(kApiLevel, 33);
}

void user_agents() {
    CHECK_EQ(app_version("2.748.0.1527"), std::string("2.748.1527"));
    CHECK_EQ(app_version("1.2.3.4"), std::string("1.2.4"));
    CHECK_EQ(app_version("2.748.1527"), std::string("0.0.0"));
    CHECK_EQ(app_version("1.2.3.4.5"), std::string("0.0.0"));
    CHECK_EQ(app_version(nullptr), std::string("0.0.0"));

    const std::string v = app_version("2.748.0.1527");
    CHECK_EQ(user_agent_for(Profile::PcWindows11, 32003, 1280, 720, v),
             std::string("Mozilla/5.0 (32003MB; 1280x720; 160x160; 1280x720; Intoxicated) AppleWebKit/537.36 "
                         "(KHTML, like Gecko)  ROBLOX Windows App 2.748.1527 Desktop Hybrid()  Intoxicated "
                         "RobloxApp/2.748.1527 (GlobalDist; Intoxicated)"));
    CHECK_EQ(user_agent_for(Profile::AndroidTablet, 32003, 1280, 720, v),
             std::string("Mozilla/5.0 (32003MB; 1280x720; 160x160; 1280x720; Intoxicated; 33) AppleWebKit/537.36 "
                         "(KHTML, like Gecko)  ROBLOX Android App 2.748.1527 Tablet Hybrid()  GooglePlayStore "
                         "RobloxApp/2.748.1527 (GlobalDist; GooglePlayStore)"));
    CHECK_EQ(user_agent_for(Profile::RobloxApp, 32003, 1280, 720, v),
             std::string("RobloxApp/2.748.1527(GlobalDist; Intoxicated)"));
    CHECK_EQ(user_agent_for(Profile::RobloxApp, 1, 1, 1, app_version("2.748.1527")),
             std::string("RobloxApp/0.0.0(GlobalDist; Intoxicated)"));
}

void memory_density_locale() {
    CHECK_EQ(total_memory_mb(nullptr, 32003), 32003);
    CHECK_EQ(total_memory_mb("off", 32003), 8192);
    CHECK_EQ(total_memory_mb("16384", 32003), 16384);
    CHECK_EQ(total_memory_mb("16384MB", 32003), 16384);
    CHECK_EQ(total_memory_mb("0", 32003), 32003);
    CHECK_EQ(total_memory_mb("junk", 32003), 32003);
    CHECK_EQ(total_memory_mb(nullptr, -1), 8192);
    CHECK(physmem_mb() > 0);

    std::string complaint;
    CHECK(dpi_scale(nullptr, &complaint) == 1.0f && complaint.empty());
    CHECK(dpi_scale("", &complaint) == 1.0f && complaint.empty());
    CHECK(dpi_scale("1.5", &complaint) == 1.5f && complaint.empty());
    CHECK(dpi_scale("8", &complaint) == 8.0f && complaint.empty());
    for (const char* bad : {"0", "-1", "9", "abc"}) {
        CHECK(dpi_scale(bad, &complaint) == 1.0f);
        CHECK_EQ(complaint, std::string("[android] INTOXICATED_DPI_SCALE=") + bad +
                                " is not a scale between 0 and 8; using 1.0");
    }

    CHECK_EQ(millimetres_at_96dpi(1280), 338);
    CHECK_EQ(millimetres_at_96dpi(720), 190);
    CHECK_EQ(millimetres_at_96dpi(1920), 508);
    CHECK_EQ(millimetres_at_96dpi(1080), 285);
    CHECK_EQ(millimetres_at_96dpi(3440), 910);
    CHECK_EQ(millimetres_at_96dpi(1440), 381);

    CHECK_EQ(locale_tag(nullptr, nullptr, nullptr), std::string("en_us"));
    CHECK_EQ(locale_tag(nullptr, nullptr, "C"), std::string("en_us"));
    CHECK_EQ(locale_tag(nullptr, nullptr, "C.UTF-8"), std::string("en_us"));
    CHECK_EQ(locale_tag(nullptr, nullptr, "POSIX"), std::string("en_us"));
    CHECK_EQ(locale_tag(nullptr, nullptr, "en_AU.UTF-8"), std::string("en_au"));
    CHECK_EQ(locale_tag("de_DE.UTF-8", nullptr, "fr_FR"), std::string("de_de"));
    CHECK_EQ(locale_tag("", "pt_BR", nullptr), std::string("pt_br"));
    CHECK_EQ(locale_tag(nullptr, nullptr, "sr_RS@latin"), std::string("sr_rs@latin"));
}

void display_state() {
    int w = 0;
    int h = 0;
    display_size(&w, &h);
    CHECK(w == 1280 && h == 720);
    set_display_size(1920, 1080);
    display_size(&w, &h);
    CHECK(w == 1920 && h == 1080);
    set_display_size(0, 1080);
    set_display_size(1920, -1);
    display_size(&w, &h);
    CHECK(w == 1920 && h == 1080);

    CHECK(!physical_mm(&w, &h));
    set_physical_mm(-5, 300);
    CHECK(!physical_mm(&w, &h));
    set_physical_mm(800, 330);
    CHECK(physical_mm(&w, &h) && w == 800 && h == 330);
    set_physical_mm(0, 0);
    CHECK(!physical_mm(&w, &h));

    CHECK_EQ(night(), -1);
    set_night(1);
    CHECK_EQ(night(), 1);
}

void identity_and_text() {
    CHECK(!identity().known);
    identity_publish(123, "user", "", 4, false, true);
    Identity who = identity();
    CHECK(who.known && who.user_id == 123 && who.username == "user" && who.display_name.empty());
    CHECK(who.membership_type == 4 && !who.under13 && who.subscription);
    identity_publish(0, nullptr, nullptr, 0, false, false);
    CHECK(identity().known && identity().username.empty());
    identity_clear();
    who = identity();
    CHECK(!who.known && who.user_id == 0 && who.username.empty() && !who.subscription);

    // A focus with no spec, in a process where no constructor ran: no spec.
    const uint32_t g0 = textbox_generation();
    textbox_focus(7, "text", nullptr);
    CHECK_EQ(textbox_handle(), int64_t{7});
    CHECK(textbox_generation() != g0);
    TextBoxSpec spec{};
    CHECK(!textbox_spec(&spec));

    TextBoxSpec given{1.5f, 2.5f, 3.5f, 4.5f, 5.5f, 1, 6, 7, 8, 9, 10, 11, 0, 1, 1};
    textbox_focus(42, "hello", &given);
    CHECK_EQ(textbox_handle(), int64_t{42});
    CHECK_EQ(textbox_text(), std::string("hello"));
    CHECK(textbox_spec(&spec) && spec.x == 1.5f && spec.font_size == 5.5f && spec.return_key_type == 11 &&
          spec.fifteenth == 1);
    textbox_blur();
    CHECK_EQ(textbox_handle(), int64_t{0});
    CHECK(!textbox_spec(&spec));
    CHECK_EQ(textbox_text(), std::string("hello"));

    // A constructor's values stand in for a focus that brought none.
    textbox_record_built(given);
    CHECK(textbox_focus(9, "", nullptr) == SpecSource::LastBuilt);
    CHECK(textbox_spec(&spec) && spec.text_color == 8);
    textbox_blur();

    char buf[4];
    CHECK_EQ(copy_out("hello", buf, sizeof(buf)), 3);
    CHECK_EQ(std::string(buf), std::string("hel"));
    CHECK_EQ(copy_out("hello", nullptr, 0), 0);

    CHECK_EQ(url_without_query("https://example.com/a?ticket=x#f"), std::string("https://example.com/a?<query elided>"));
    CHECK_EQ(url_without_query("https://example.com/a#f"), std::string("https://example.com/a#<fragment elided>"));
    CHECK_EQ(url_without_query("https://example.com/a"), std::string("https://example.com/a"));
    CHECK_EQ(url_without_query(nullptr), std::string("(null)"));

    const uint32_t ime0 = ime_generation();
    ime_set_state("abc", 1, 2, -1, -1);
    CHECK(ime_generation() != ime0);
    CHECK_EQ(ime_text(), std::string("abc"));
    int start = 0;
    int end = 0;
    ime_selection(&start, &end);
    CHECK(start == 1 && end == 2);

    const uint32_t loads = games_loaded();
    note_game_loaded(5938036553LL);
    CHECK_EQ(games_loaded(), loads + 1);
}

} // namespace

int main() {
    preference_files();
    profiles();
    user_agents();
    memory_density_locale();
    display_state();
    identity_and_text();
    if (g_failures) {
        fprintf(stderr, "jni_values_test: %d of %d checks failed\n", g_failures, g_checks);
        return 1;
    }
    printf("jni_values_test: %d checks passed\n", g_checks);
    return 0;
}
