// `intoxicated-settings --self-test`: the app's own load and save code run
// against a scratch XDG_CONFIG_HOME and XDG_DATA_HOME (CTest runs it). It
// refuses to start unless every path it would write is inside the scratch
// directory, so the real settings.conf and profiles are never touched.

#include "core.h"

#include <QDebug>
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QJsonDocument>
#include <QJsonObject>
#include <QRegularExpression>
#include <QTemporaryDir>

#include <fcntl.h>
#include <sys/file.h>
#include <unistd.h>

#include <cstdio>

namespace intox {

namespace {

int g_checks = 0;
int g_failures = 0;

void check(bool ok, const QString &what) {
    ++g_checks;
    if (!ok) {
        ++g_failures;
        std::fprintf(stderr, "FAIL: %s\n", qPrintable(what));
    }
}

template <typename T>
QString describe(const T &value) {
    QString text;
    QDebug(&text) << value;
    return text;
}

template <typename A, typename B>
void same(const A &got, const B &want, const QString &what) {
    const bool ok = got == want;
    if (!ok)
        std::fprintf(stderr, "  got:  %s\n  want: %s\n", qPrintable(describe(got)), qPrintable(describe(want)));
    check(ok, what);
}

QByteArray readAll(const QString &path) {
    QFile f(path);
    return f.open(QIODevice::ReadOnly) ? f.readAll() : QByteArray("<unreadable>");
}

void writeAll(const QString &path, const QByteArray &data) {
    QDir().mkpath(QFileInfo(path).absolutePath());
    QFile f(path);
    if (f.open(QIODevice::WriteOnly | QIODevice::Truncate))
        f.write(data);
}

// Entries of DIR other than the ones named, to catch a temporary file that
// was left behind instead of renamed.
QStringList strays(const QString &dir, const QStringList &expected) {
    QStringList out;
    for (const QString &name : QDir(dir).entryList(QDir::AllEntries | QDir::NoDotAndDotDot | QDir::Hidden)) {
        if (!expected.contains(name))
            out.append(name);
    }
    return out;
}

void testPaths(const QString &scratch) {
    same(settingsConfPath(), scratch + "/config/intoxicated/settings.conf", "settings.conf path");
    same(flagsPath("alt"), scratch + "/data/intoxicated/profiles/alt/flags.json", "flags.json path");
    const QByteArray saved = qgetenv("XDG_CONFIG_HOME");
    qputenv("XDG_CONFIG_HOME", "");
    same(configHome(), QDir::homePath() + "/.config", "an empty XDG_CONFIG_HOME falls back to ~/.config");
    qputenv("XDG_CONFIG_HOME", saved);
}

void testSettingsConf() {
    const QString path = settingsConfPath();
    const QByteArray original = "# Written by hand\n"
                                "\n"
                                "INTOXICATED_FULLSCREEN=1\n"
                                "INTOXICATED_SOMETHING_NEW=keep me\n"
                                "PATH=/not/ours\n"
                                "  INTOXICATED_INDENTED=1\n"
                                "INTOXICATED_MONITOR=0\n"
                                "export INTOXICATED_EXPORTED=1\n"
                                "INTOXICATED_MONITOR=1\n"
                                "# trailing comment\n";
    writeAll(path, original);
    QFile::setPermissions(path, QFileDevice::ReadOwner | QFileDevice::WriteOwner);

    SettingsConf conf;
    QString error;
    check(conf.load(path, &error), "settings.conf loads: " + error);
    same(conf.text().toUtf8(), original, "an unchanged settings.conf is written back as it was");
    same(conf.value("INTOXICATED_MONITOR").value_or("<none>"), QString("1"), "the last line for a key wins");
    same(conf.value("INTOXICATED_SOMETHING_NEW").value_or("<none>"), QString("keep me"), "unknown keys read");
    check(!conf.contains("INTOXICATED_INDENTED"), "an indented line is not a setting");
    check(!conf.contains("INTOXICATED_EXPORTED"), "an export line is not a setting");

    check(conf.set("INTOXICATED_MONITOR", "2"), "set an existing key");
    check(conf.set("INTOXICATED_RESOLUTION", "1920x1080"), "set a new key");
    check(conf.set("INTOXICATED_X", "$(touch pwned)"), "a value is literal text");
    conf.remove("INTOXICATED_FULLSCREEN");
    check(!conf.set("INTOXICATED_BAD", "two\nlines"), "a value with a line break is refused");
    check(!conf.set("INTOXICATED_BAD", "cr\r"), "a value with a carriage return is refused");
    check(!conf.set("PATH", "/x"), "a key that is not INTOXICATED_ is refused");
    check(!conf.set("INTOXICATED_", "x"), "INTOXICATED_ alone is refused");
    check(!conf.set("INTOXICATED_lower", "x"), "a lower-case key is refused");
    check(conf.save(&error), "settings.conf saves: " + error);

    same(readAll(path),
         QByteArray("# Written by hand\n"
                    "\n"
                    "INTOXICATED_SOMETHING_NEW=keep me\n"
                    "PATH=/not/ours\n"
                    "  INTOXICATED_INDENTED=1\n"
                    "export INTOXICATED_EXPORTED=1\n"
                    "INTOXICATED_MONITOR=2\n"
                    "# trailing comment\n"
                    "INTOXICATED_RESOLUTION=1920x1080\n"
                    "INTOXICATED_X=$(touch pwned)\n"),
         "settings.conf round trip keeps every line it does not manage");
    same(int(QFileInfo(path).permissions() & 0x7777),
         int(QFileDevice::ReadOwner | QFileDevice::WriteOwner | QFileDevice::ReadUser | QFileDevice::WriteUser),
         "the file keeps its permissions");
    same(strays(QFileInfo(path).absolutePath(), {"settings.conf"}), QStringList(), "no temporary file is left");

    check(editSetting("INTOXICATED_PRESENT_MODE", QString("mailbox"), &error), "editSetting sets: " + error);
    check(editSetting("INTOXICATED_MONITOR", std::nullopt, &error), "editSetting removes: " + error);
    SettingsConf back;
    back.load(path);
    same(back.value("INTOXICATED_PRESENT_MODE").value_or("<none>"), QString("mailbox"), "editSetting wrote");
    check(!back.contains("INTOXICATED_MONITOR"), "editSetting removed the key");
    check(back.text().startsWith("# Written by hand\n\nINTOXICATED_SOMETHING_NEW=keep me\n"), "comments survive");

    // A file with no final newline, and a missing file.
    writeAll(path, "INTOXICATED_A=1");
    conf.load(path);
    conf.set("INTOXICATED_B", "2");
    conf.save();
    same(readAll(path), QByteArray("INTOXICATED_A=1\nINTOXICATED_B=2\n"), "a last line without a newline");
    QFile::remove(path);
    check(conf.load(path, &error) && conf.text().isEmpty(), "a missing settings.conf is empty");
    conf.clear();
    check(conf.save(&error) && readAll(path).isEmpty(), "an emptied settings.conf is an empty file");
}

void testFlagRules() {
    for (const char *ok : {"FFlagDebugGraphicsPreferVulkan", "DFFlagX", "SFFlagX_1", "FIntA", "DFIntA", "FStringA",
                           "DFStringA", "FLogA", "DFLogA"})
        check(checkFlagName(ok).isEmpty(), QString("valid name %1").arg(ok));
    for (const char *bad : {"", "FFlag", "DFInt", "Foo", "fflagX", "FFlag-x", "FIntA b", "FStringA.b", "IntoxicatedX",
                            "XFFlagA"})
        check(!checkFlagName(bad).isEmpty(), QString("invalid name %1").arg(bad));

    struct V {
        const char *name, *value, *normalised;
    };
    const V good[] = {
        {"FFlagA", "true", "True"},     {"FFlagA", "FALSE", "False"},  {"DFFlagA", " True ", "True"},
        {"SFFlagA", "false", "False"},  {"FIntA", "42", "42"},         {"DFIntA", "-5", "-5"},
        {"FIntA", "0042", "42"},        {"FIntA", "+7", "7"},          {"FIntA", "2147483647", "2147483647"},
        {"FStringA", "any thing", "any thing"}, {"FLogA", "7", "7"},   {"DFLogA", "", ""},
    };
    for (const V &v : good) {
        QString n;
        const QString why = checkFlagValue(v.name, v.value, &n);
        check(why.isEmpty() && n == v.normalised, QString("%1 = %2 gives %3 (%4)").arg(v.name, v.value, v.normalised, why));
    }
    const V bad[] = {
        {"FFlagA", "yes", ""}, {"FFlagA", "1", ""}, {"FIntA", "1.5", ""}, {"FIntA", "abc", ""},
        {"DFIntA", "", ""},    {"FIntA", "2147483648", ""}, {"FIntA", "12 3", ""},
    };
    for (const V &v : bad) {
        QString n;
        check(!checkFlagValue(v.name, v.value, &n).isEmpty(), QString("%1 = %2 is refused").arg(v.name, v.value));
    }
}

void testFlagsFiles() {
    const QString path = flagsPath("default");
    FlagsFile missing = loadFlags(path);
    check(!missing.exists && missing.error.isEmpty() && missing.values.isEmpty(), "a missing flags.json is empty");

    writeAll(path, R"({"FFlagA": true, "DFIntB": 12, "FStringC": "x", "FIntBig": 2147483647, "FLogD": 1.5})");
    FlagsFile file = loadFlags(path);
    check(file.exists && file.error.isEmpty(), "flags.json loads: " + file.error);
    same(file.values.value("FFlagA"), QString("true"), "a boolean is read as its JSON text");
    same(file.values.value("DFIntB"), QString("12"), "a number is read as its JSON text");
    same(file.values.value("FIntBig"), QString("2147483647"), "a large integer keeps every digit");
    same(file.values.value("FLogD"), QString("1.5"), "a fraction");

    QString error;
    check(editFlags(path, [](FlagMap &f) { f.insert("FIntNew", "3"); }, &error), "editFlags: " + error);
    const QJsonObject written = QJsonDocument::fromJson(readAll(path)).object();
    same(written.size(), 6, "every flag kept");
    check(written.value("DFIntB").isString() && written.value("DFIntB").toString() == "12", "written as strings");
    check(written.value("FIntNew").toString() == "3", "the new flag is there");
    same(strays(QFileInfo(path).absolutePath(), {"flags.json"}), QStringList(), "no temporary file is left");

    // Files that are not a usable object are never written over.
    for (const QByteArray &broken : {QByteArray("[1, 2]"), QByteArray("{\"FFlagA\": "), QByteArray("\"text\""),
                                     QByteArray("{\"FFlagA\": null}"), QByteArray("{\"A\": {\"b\": 1}}")}) {
        writeAll(path, broken);
        FlagsFile f = loadFlags(path);
        check(!f.error.isEmpty(), "broken flags.json is reported: " + QString::fromUtf8(broken));
        check(!editFlags(path, [](FlagMap &m) { m.insert("FFlagX", "True"); }, &error), "a broken file is not edited");
        same(readAll(path), broken, "a broken file is left as it was");
    }
    writeAll(path, "[1, 2]");
    check(loadFlags(path).error.contains("not a JSON object"), "the reason names the problem");
    const QString backup = backupBrokenFlags(path, &error);
    check(QRegularExpression("/flags\\.json\\.bad-[0-9]{8}-[0-9]{6}$").match(backup).hasMatch(),
          "the backup is flags.json.bad-<time>: " + backup + error);
    same(readAll(backup), QByteArray("[1, 2]"), "the backup holds the old text");
    FlagsFile fresh = loadFlags(path);
    check(fresh.exists && fresh.error.isEmpty() && fresh.values.isEmpty(), "and flags.json starts empty");
    QFile::remove(backup);

    writeAll(path, "  \n");
    check(loadFlags(path).error.isEmpty(), "a blank flags.json is an empty one");
}

void testImport() {
    const FlagMap current{{"FFlagA", "True"}, {"DFIntB", "1"}, {"FStringC", "x"}};
    FlagMap incoming;
    QStringList skipped;
    QString error;
    check(parseFlags(R"({"FFlagA": "true", "DFIntB": 2, "FIntNew": "0042", "FFlagBad": "maybe",
                        "NotAFlag": "1", "Nested": {"a": 1}})",
                     &incoming, &error, &skipped),
          "an import document parses: " + error);
    same(skipped, QStringList{"Nested"}, "a nested value is skipped, not fatal");
    const QList<ImportItem> plan = planImport(current, incoming, skipped);
    QStringList names;
    for (const ImportItem &item : plan)
        names.append(item.name);
    same(names, QStringList({"DFIntB", "FFlagBad", "FIntNew", "NotAFlag", "Nested"}),
         "the plan leaves out flags that already have that value");
    for (const ImportItem &item : plan) {
        if (item.name == "DFIntB")
            check(item.current == QString("1") && item.value == "2" && item.problem.isEmpty(), "DFIntB changes");
        if (item.name == "FIntNew")
            check(!item.current && item.value == "42" && item.problem.isEmpty(), "FIntNew is added, normalised");
        if (item.name == "FFlagBad" || item.name == "NotAFlag" || item.name == "Nested")
            check(!item.problem.isEmpty(), item.name + " cannot be imported");
    }
    FlagMap merged = current;
    applyImport(merged, plan, {"DFIntB", "FFlagBad", "NotAFlag", "Nested"});
    same(merged, FlagMap({{"FFlagA", "True"}, {"DFIntB", "2"}, {"FStringC", "x"}}),
         "only the ticked, valid flags are merged");
    applyImport(merged, plan, {"DFIntB", "FIntNew"});
    same(merged.value("FIntNew"), QString("42"), "a second pick adds the rest");

    FlagMap none;
    check(!parseFlags("[]", &none, &error), "an import that is not an object is refused");
    check(!parseFlags("not json", &none, &error), "an import that is not JSON is refused");
}

void testPresets() {
    const FlagMap defaults{{"FFlagUserLaunchedWithBloxstrap", "True"},
                           {"FFlagRenderShadowResolutionScaling", "False"},
                           {"FFlagVulkanAllocatorPoolSmallOnly", "False"},
                           {"FIntVulkanAllocatorKeepEmptyPages", "32"},
                           {"FFlagVulkanAllocatorEmptyPageBudget", "False"}};
    const QList<SwitchPreset> presets = switchPresets();
    same(presets.size(), 3, "three switches");
    const SwitchPreset &shadows = presets[0];
    const SwitchPreset &pooling = presets[1];
    const SwitchPreset &msaa = presets[2];

    FlagMap flags;
    check(presetIsOn(shadows, flags, defaults), "a built-in switch is on by default");
    check(presetIsOn(pooling, flags, defaults), "pooling is on by default");
    check(!presetIsOn(msaa, flags, defaults), "MSAA is off by default");
    check(!presetIsOn(shadows, flags, {}), "without the defaults a switch reads off");

    applyPreset(shadows, false, flags, defaults);
    same(flags, FlagMap({{"FFlagRenderShadowResolutionScaling", "True"}}), "turning a built-in off writes the opposite");
    check(!presetIsOn(shadows, flags, defaults), "and reads off");
    applyPreset(shadows, true, flags, defaults);
    check(flags.isEmpty() && presetIsOn(shadows, flags, defaults), "turning it on again restores the built-in");

    applyPreset(pooling, false, flags, defaults);
    same(flags,
         FlagMap({{"FFlagVulkanAllocatorPoolSmallOnly", "True"},
                  {"FIntVulkanAllocatorKeepEmptyPages", "0"},
                  {"FFlagVulkanAllocatorEmptyPageBudget", "True"}}),
         "pooling off writes all three");
    flags.insert("FIntVulkanAllocatorKeepEmptyPages", "32");
    check(!presetIsOn(pooling, flags, defaults), "pooling is on only when all three hold");
    applyPreset(pooling, true, flags, defaults);
    check(flags.isEmpty(), "pooling on removes all three");

    applyPreset(msaa, true, flags, defaults);
    same(flags, FlagMap({{"FIntDebugForceMSAASamples", "4"}}), "MSAA on");
    applyPreset(msaa, false, flags, defaults);
    check(flags.isEmpty(), "MSAA off removes it");

    FlagMap explicitFlags;
    applyPreset(shadows, true, explicitFlags, {});
    same(explicitFlags.value("FFlagRenderShadowResolutionScaling"), QString("False"),
         "with no built-in value, on is written out");
    check(presetIsOn(shadows, FlagMap({{"FFlagRenderShadowResolutionScaling", "false"}}), {}),
          "True/False compare without case");

    SettingsConf conf;
    conf.load(settingsConfPath());
    conf.clear();
    same(effectiveFps({}, defaults, conf), QString("9999"), "the launcher's fps by default");
    conf.set(kFpsSetting, "off");
    same(effectiveFps({}, defaults, conf), QString("60"), "INTOXICATED_FPS=off is Roblox's 60");
    conf.set(kFpsSetting, "144");
    same(effectiveFps({}, defaults, conf), QString("144"), "INTOXICATED_FPS");
    same(effectiveFps({{kFpsFlag, "240"}}, defaults, conf), QString("240"), "flags.json wins");
    same(effectiveVideoMemory({}, defaults, SettingsConf()), QString("1610612736"), "1.5 GB by default");
    conf.set(kVideoMemorySetting, "off");
    same(effectiveVideoMemory({}, defaults, conf), QString(), "off is Roblox's default");
}

void testQualityPresets() {
    const QList<QualityPreset> presets = qualityPresets();
    QStringList names;
    for (const QualityPreset &p : presets)
        names.append(p.name);
    same(names, QStringList({"Potato", "Lowest", "Low", "Default", "High", "Competitive"}), "the six presets, in order");
    check(!presets[0].warning.isEmpty(), "Potato carries a warning");
    for (int i = 1; i < presets.size(); ++i)
        check(presets[i].warning.isEmpty(), presets[i].name + " has no warning");

    FlagMap flags{{"FFlagSomethingElse", "True"}};
    same(matchingQualityPreset(flags), 3, "no graphics flags is Default");
    applyQualityPreset(presets[0], flags);
    same(matchingQualityPreset(flags), 0, "Potato applied reads as Potato");
    same(flags.value("FIntViewportFrameMaxSize"), QString("0"), "Potato turns viewport frames off");
    applyQualityPreset(presets[1], flags);
    same(matchingQualityPreset(flags), 1, "Lowest applied over Potato reads as Lowest");
    check(!flags.contains("FIntViewportFrameMaxSize"), "and drops Potato's own keys");
    same(flags.value("FFlagDebugSkyGray"), QString("True"), "Lowest greys the sky");
    same(flags.value("FFlagSomethingElse"), QString("True"), "an unrelated flag is kept");

    applyQualityPreset(presets[4], flags);
    same(matchingQualityPreset(flags), 4, "High applied over Lowest reads as High");
    check(!flags.contains("FFlagDebugSkyGray") && !flags.contains("FIntFRMMaxGrassDistance"),
          "keys Lowest set and High does not are removed");
    same(flags.value("DFIntDebugFRMQualityLevelOverride"), QString("21"), "High is quality 21");

    flags.insert("DFIntDebugFRMQualityLevelOverride", "20");
    same(matchingQualityPreset(flags), -1, "an edited level is a custom mix");
    flags.insert("DFIntDebugFRMQualityLevelOverride", "021");
    same(matchingQualityPreset(flags), 4, "021 and 21 are the same value to the client");

    applyQualityPreset(presets[3], flags);
    same(flags, FlagMap({{"FFlagSomethingElse", "True"}}), "Default removes every graphics flag and nothing else");

    for (const QualityPreset &p : presets)
        for (const auto &[k, v] : p.flags) {
            QString normalised;
            check(checkFlagName(k).isEmpty() && checkFlagValue(k, v, &normalised).isEmpty(),
                  p.name + " writes a valid " + k);
        }
}

void testProfileFiles() {
    const FlagMap flags{{"DFIntTaskSchedulerTargetFps", "9999"}, {"FFlagDebugSkyGray", "True"}};
    const QByteArray doc = profileDocument("alt", flags);
    check(!doc.contains("cookie") && !doc.contains("session"), "an export holds no sign-in");
    ProfileFile file;
    QString error;
    check(parseProfileDocument(doc, &file, &error), "an export reads back: " + error);
    same(file.name, QString("alt"), "the name comes back");
    same(file.flags, flags, "the flags come back");

    check(!parseProfileDocument("[1,2]", &file, &error), "a JSON array is refused");
    check(!parseProfileDocument("{\"flags\":{}}", &file, &error), "a file without the marker is refused");
    check(!parseProfileDocument("{\"intoxicated-profile\":1}", &file, &error), "a file without flags is refused");

    const QByteArray odd = "{\"intoxicated-profile\":1,\"name\":\"../evil\",\"flags\":"
                           "{\"FFlagGood\":true,\"FIntNumber\":12,\"NotAFlag\":\"x\",\"FFlagBad\":\"maybe\","
                           "\"FStringList\":[1]}}";
    check(parseProfileDocument(odd, &file, &error), "a file with odd entries still reads: " + error);
    same(file.name, QString("imported"), "an unusable name becomes imported");
    same(file.flags, FlagMap({{"FFlagGood", "True"}, {"FIntNumber", "12"}}), "only valid flags are kept");
    same(file.skipped.size(), 3, "three are reported as left out");

    check(importProfile("imp1", flags, &error), "import creates a profile: " + error);
    same(loadFlags(flagsPath("imp1")).values, flags, "with the flags in its flags.json");
    check(!importProfile("imp1", flags, &error), "importing over an existing profile is refused");
    same(freeProfileName("imp1"), QString("imp1-2"), "a taken name gets a suffix");
    same(freeProfileName("fresh"), QString("fresh"), "a free name is kept");
    QString err2;
    deleteProfile("imp1", &err2);
}

void testVideoMemoryDefault() {
    QString error;
    writeAll(settingsConfPath(), "# mine\nINTOXICATED_FULLSCREEN=1\n");
    writeAll(flagsPath("alt"), R"({"FIntRenderForceVideoMemorySize": "1610612736", "FFlagA": "True"})");
    check(useRobloxVideoMemory("alt", &error), "Roblox's default: " + error);
    const FlagsFile f = loadFlags(flagsPath("alt"));
    check(!f.values.contains(kVideoMemoryFlag) && f.values.value("FFlagA") == "True", "the key left flags.json");
    same(readAll(settingsConfPath()), QByteArray("# mine\nINTOXICATED_FULLSCREEN=1\nINTOXICATED_VIDEO_MEMORY=off\n"),
         "and the launcher is told not to add it back");
    QDir(profileDir("alt")).removeRecursively();
    QFile::remove(settingsConfPath());
}

void testProfiles() {
    QString error;
    // The flags tests above made `default`; start from no profiles at all.
    QDir(profilesRoot()).removeRecursively();
    QList<ProfileInfo> list = listProfiles();
    check(list.size() == 1 && list[0].name == "default" && !list[0].exists, "default is listed before it exists");

    check(createProfile("alt", &error), "create alt: " + error);
    const QString alt = profileDir("alt");
    check(QFileInfo(alt).isDir(), "alt is a directory");
    same(int(QFileInfo(alt).permissions() & 0x0077), 0, "and owner only");
    check(!createProfile("alt", &error), "a second alt is refused");
    const QString longest(32, QLatin1Char('x'));
    check(createProfile(longest, &error), "32 characters: " + error);
    for (const QString &bad : {QString(), QString("../x"), QString("a b"), QString(33, QLatin1Char('y')),
                               QString("dot.name"), QString("sl/ash")}) {
        check(!createProfile(bad, &error), "refused profile name " + bad);
    }
    same(strays(profilesRoot(), {"alt", longest}), QStringList(), "nothing else was created");

    list = listProfiles("fromconf");
    QStringList names;
    for (const ProfileInfo &p : list)
        names.append(p.name);
    same(names, QStringList({"default", "alt", "fromconf", longest}), "default first, the rest sorted");

    check(!profileSignedIn(alt), "not signed in without cookies");
    writeAll(alt + "/cookies", "secret");
    check(profileSignedIn(alt), "signed in with a cookies file");

    // A client's lock, held by another open file description.
    const QByteArray lockPath = QFile::encodeName(alt + "/.lock");
    const int fd = ::open(lockPath.constData(), O_RDWR | O_CREAT | O_CLOEXEC, 0600);
    check(fd >= 0 && ::flock(fd, LOCK_EX | LOCK_NB) == 0, "the test holds alt's lock");
    check(profileInUse(alt), "a held lock is in use");
    check(listProfiles()[1].inUse, "and listed as in use");
    check(!deleteProfile("alt", &error), "an in-use profile is not deleted");
    check(QFileInfo(alt).isDir() && QFileInfo::exists(alt + "/cookies"), "and is left whole");
    ::close(fd);
    check(!profileInUse(alt), "a released lock is not in use");
    const int again = ::open(lockPath.constData(), O_RDWR | O_CLOEXEC);
    check(again >= 0 && ::flock(again, LOCK_EX | LOCK_NB) == 0, "the check itself does not keep the lock");
    ::close(again);

    check(deleteProfile("alt", &error), "delete alt: " + error);
    check(!QFileInfo::exists(alt), "alt is gone");
    check(!deleteProfile("default", &error), "default cannot be deleted");
    check(!deleteProfile("nope", &error), "a missing profile is reported");
    check(!deleteProfile("../config", &error), "a path is not a profile");

    SettingsConf conf;
    conf.load(settingsConfPath());
    conf.set("INTOXICATED_PROFILE", longest);
    same(defaultProfileName(conf), longest, "settings.conf names the default profile");
    conf.set("INTOXICATED_PROFILE", "../x");
    same(defaultProfileName(conf), QString("default"), "an unusable name falls back to default");
}

void testSndstat() {
    const QString text = "Installed devices:\n"
                         "pcm0: <NVIDIA (0x00a5) (HDMI/DP 8ch)> (play)\n"
                         "pcm5: <Realtek ALC897 (Rear Analog)> (play/rec)\n"
                         "pcm7: <Realtek ALC897 (Internal Digital)> (play)\n"
                         "pcm8: <Harman International Inc JBL Quantum810 Wireless> (play/rec) default\n"
                         "pcm9: <Mic only> (rec)\n"
                         "No devices installed from userspace.\n";
    const QList<SoundDevice> devices = parseSndstat(text);
    same(devices.size(), 5, "five pcm lines");
    if (devices.size() == 5) {
        same(devices[0].name, QString("NVIDIA (0x00a5) (HDMI/DP 8ch)"), "a name with brackets in it");
        same(devices[0].node(), QString("/dev/dsp0"), "the node");
        check(devices[1].play && devices[1].record && !devices[1].isDefault, "play/rec");
        check(devices[3].isDefault && devices[3].unit == 8, "the default one");
        check(!devices[4].play && devices[4].record, "record only");
    }
}

// What `intoxicated --ssr-looks` prints, cut down to one knob of each kind
// and three looks (the client's own table is checked against its source by
// the runtime's tests).
const QByteArray kSsrLooks = R"({
  "default_look": "competitive",
  "knobs": [
    {"default": 0.4, "env": "INTOXICATED_SSR_INTENSITY", "kind": "number", "max": 1.0, "min": 0.0, "name": "intensity"},
    {"default": 1, "env": "INTOXICATED_SSR_UP_ONLY", "kind": "switch", "max": 1, "min": 0, "name": "up_only"},
    {"default": [1.0, 1.0, 1.0], "env": "INTOXICATED_SSR_TINT", "kind": "rgb", "max": 2.0, "min": 0.0, "name": "tint"},
    {"default": 32, "env": "INTOXICATED_SSR_STEPS", "kind": "whole", "max": 64, "min": 4, "name": "steps"},
    {"default": 0.06, "env": "INTOXICATED_SSR_THICKNESS", "kind": "number", "max": 1.0, "min": 0.005, "name": "thickness"}
  ],
  "look_env": "INTOXICATED_SSR_LOOK",
  "looks": [
    {"description": "Faint and soft.", "name": "competitive",
     "values": {"intensity": 0.4, "steps": 32, "thickness": 0.06, "tint": [1.0, 1.0, 1.0], "up_only": 1}},
    {"description": "Wet tarmac.", "name": "wet_road",
     "values": {"intensity": 0.55, "steps": 32, "thickness": 0.06, "tint": [0.92, 0.95, 1.0], "up_only": 1}},
    {"description": "Walls too.", "name": "glossy",
     "values": {"intensity": 0.4, "steps": 32, "thickness": 0.06, "tint": [1.0, 1.0, 1.0], "up_only": 0}}
  ],
  "switch": "INTOXICATED_SSR"
})";

void testSsrTable() {
    SsrTable table;
    QString error;
    check(parseSsrTable(kSsrLooks, &table, &error), "the looks parse: " + error);
    same(table.knobs.size(), 5, "five knobs");
    same(table.looks.size(), 3, "three looks");
    same(table.defaultLook, QString("competitive"), "the default look");
    if (table.knobs.size() == 5 && table.looks.size() == 3) {
        check(table.knob("tint")->kind == SsrKind::Rgb && table.knob("tint")->max == 2.0, "tint is a colour 0 to 2");
        check(table.knob("steps")->kind == SsrKind::Whole && table.knob("steps")->min == 4, "steps is whole from 4");
        check(table.knob("up_only")->kind == SsrKind::Switch, "up_only is a switch");
        same(table.knob("intensity")->env, QString("INTOXICATED_SSR_INTENSITY"), "a knob's variable");
        same(table.look("wet_road")->values.value("tint"), SsrValue({0.92, 0.95, 1.0}), "a look's colour");
        same(table.look("wet_road")->description, QString("Wet tarmac."), "a look's description");
        check(!table.knob("nope") && !table.look("nope"), "no such knob or look");
    }

    const auto refused = [](const QByteArray &json, const QString &what) {
        SsrTable t;
        QString why;
        check(!parseSsrTable(json, &t, &why) && !why.isEmpty(), "refused: " + what);
    };
    refused("not json", "not JSON");
    refused("[]", "not an object");
    refused(QByteArray(kSsrLooks).replace("\"switch\": \"INTOXICATED_SSR\"", "\"switch\": \"INTOXICATED_FPS\""),
            "another switch");
    refused(QByteArray(kSsrLooks).replace("\"kind\": \"whole\"", "\"kind\": \"curve\""), "an unknown kind");
    refused(QByteArray(kSsrLooks).replace("\"default_look\": \"competitive\"", "\"default_look\": \"shiny\""),
            "a default look that is not listed");
    refused(QByteArray(kSsrLooks).replace("\"intensity\": 0.55, ", ""), "a look missing a knob");
    refused(QByteArray(kSsrLooks).replace("\"tint\": [0.92, 0.95, 1.0]", "\"tint\": 0.92"), "a colour as one number");
    refused(QByteArray(kSsrLooks).replace("\"env\": \"INTOXICATED_SSR_STEPS\"", "\"env\": \"INTOXICATED_FPS\""),
            "a knob whose variable is not INTOXICATED_SSR_");
    refused(QByteArray(kSsrLooks).replace("\"env\": \"INTOXICATED_SSR_STEPS\"", "\"env\": \"INTOXICATED_SSR_LOOK\""),
            "a knob that would write the look's variable");
    refused(QByteArray(kSsrLooks).replace("\"name\": \"glossy\"", "\"name\": \"wet_road\""), "a look listed twice");
    refused(QByteArray(kSsrLooks).replace("\"name\": \"glossy\"", "\"name\": \"Glossy look\""), "a look name with capitals");

    same(ssrLookTitle("wet_road"), QString("Wet road"), "a look's title");
    same(ssrLookTitle("clean_mirror"), QString("Clean mirror"), "another look's title");
    same(ssrLookTitle("competitive"), QString("Competitive"), "a one-word title");
}

void testSsrValues() {
    SsrTable table;
    parseSsrTable(kSsrLooks, &table, nullptr);
    if (table.knobs.size() != 5)
        return;
    const SsrKnob &intensity = *table.knob("intensity");
    const SsrKnob &tint = *table.knob("tint");
    const SsrKnob &steps = *table.knob("steps");
    const SsrKnob &upOnly = *table.knob("up_only");
    const SsrKnob &thickness = *table.knob("thickness");
    const auto takes = [](const SsrKnob &k, const QString &text) { return parseSsrValue(k, text); };

    same(takes(intensity, "0.5").value_or(SsrValue()), SsrValue({0.5}), "a number in range");
    same(takes(intensity, " 1 ").value_or(SsrValue()), SsrValue({1.0}), "trimmed, and the top of the range");
    check(!takes(intensity, "1.5") && !takes(intensity, "-0.1"), "out of range is refused");
    check(!takes(intensity, "abc") && !takes(intensity, "") && !takes(intensity, "nan"), "not a number is refused");
    check(takes(thickness, "0.005") && !takes(thickness, "0.004"), "a range that does not start at 0");
    same(takes(steps, "20").value_or(SsrValue()), SsrValue({20.0}), "a whole number");
    check(takes(steps, "+8").has_value(), "a whole number with a plus, as the client takes it");
    check(!takes(steps, "3") && !takes(steps, "65") && !takes(steps, "20.5") && !takes(steps, "-8"),
          "whole numbers out of range or with a fraction are refused");
    check(takes(upOnly, "1") && takes(upOnly, "0"), "a switch takes 1 and 0");
    check(!takes(upOnly, "2") && !takes(upOnly, "true") && !takes(upOnly, "on"), "and nothing else");
    same(takes(tint, "0.5").value_or(SsrValue()), SsrValue({0.5, 0.5, 0.5}), "one number for all three colours");
    same(takes(tint, "1, 0.5 ,2").value_or(SsrValue()), SsrValue({1.0, 0.5, 2.0}), "r,g,b");
    check(!takes(tint, "1,2") && !takes(tint, "1,3,1") && !takes(tint, "1,1,1,1"), "a colour that does not fit");

    same(shortestFloat(0.4f), QString("0.4"), "0.4f is written as 0.4");
    same(shortestFloat(0.06f), QString("0.06"), "0.06f as 0.06");
    same(shortestFloat(1000.0f), QString("1000"), "a whole float without decimals");
    same(formatSsrValue(intensity, {0.1 + 0.2}), QString("0.3"), "the shortest text for a float");
    same(formatSsrValue(tint, {0.92, 0.95, 1.0}), QString("0.92,0.95,1"), "a colour as r,g,b");
    same(formatSsrValue(steps, {32.0}), QString("32"), "a whole number");
    same(formatSsrValue(upOnly, {0.0}), QString("0"), "a switch");
    same(formatSsrValue(thickness, {0.005}), QString("0.005"), "a small number");
}

void testSsrSettings() {
    SsrTable table;
    parseSsrTable(kSsrLooks, &table, nullptr);
    if (table.knobs.size() != 5)
        return;
    const SsrKnob &intensity = *table.knob("intensity");
    const SsrKnob &tint = *table.knob("tint");
    const SsrKnob &steps = *table.knob("steps");
    const SsrKnob &upOnly = *table.knob("up_only");
    const QString path = settingsConfPath();
    const QString dir = QFileInfo(path).absolutePath();
    QString error;
    const auto reloaded = [&] {
        SettingsConf c;
        c.load(path);
        return c;
    };

    writeAll(path, "# mine\nINTOXICATED_FPS=144\n");
    SettingsConf conf;
    conf.load(path);
    check(!ssrOn(conf), "reflections are off without the line");
    same(ssrLook(table, conf), QString("competitive"), "no look line is the default look");
    same(effectiveSsrValue(table, conf, intensity), SsrValue({0.4}), "the default look's value");
    check(ssrOverrides(table, conf).isEmpty(), "no overrides");

    // The switch.
    setSsrOn(conf, true);
    check(conf.save(&error), "save: " + error);
    same(readAll(path), QByteArray("# mine\nINTOXICATED_FPS=144\nINTOXICATED_SSR=1\n"), "on writes INTOXICATED_SSR=1");
    check(ssrOn(reloaded()), "and reads back as on");
    for (const char *off : {"0", "", "yes", "2"}) {
        SettingsConf c;
        c.set("INTOXICATED_SSR", off);
        check(!ssrOn(c), QStringLiteral("INTOXICATED_SSR=%1 is off").arg(off));
    }
    SettingsConf spaced;
    spaced.set("INTOXICATED_SSR", " 1 ");
    check(ssrOn(spaced), "the client trims the value");
    setSsrOn(conf, false);
    check(conf.save(&error), "save: " + error);
    same(readAll(path), QByteArray("# mine\nINTOXICATED_FPS=144\n"), "off removes the line and keeps the rest");
    setSsrOn(conf, true);

    // The look.
    check(applySsrLook(table, conf, "wet_road"), "pick wet_road");
    check(conf.save(&error), "save: " + error);
    same(readAll(path), QByteArray("# mine\nINTOXICATED_FPS=144\nINTOXICATED_SSR=1\nINTOXICATED_SSR_LOOK=wet_road\n"),
         "a look other than the default is written");
    conf = reloaded();
    same(ssrLook(table, conf), QString("wet_road"), "and read back");
    same(effectiveSsrValue(table, conf, intensity), SsrValue({0.55}), "its value without an override");
    same(effectiveSsrValue(table, conf, tint), SsrValue({0.92, 0.95, 1.0}), "its colour");
    check(applySsrLook(table, conf, "competitive"), "pick the default look");
    check(!conf.contains("INTOXICATED_SSR_LOOK"), "the default look removes the line");
    check(!applySsrLook(table, conf, "shiny"), "a name that is not a look is refused");
    check(!conf.contains("INTOXICATED_SSR_LOOK"), "and changes nothing");
    QString unknown;
    conf.set("INTOXICATED_SSR_LOOK", "shiny");
    same(ssrLook(table, conf, &unknown), QString("competitive"), "an unknown look starts from the default");
    same(unknown, QString("shiny"), "and is reported");
    conf.set("INTOXICATED_SSR_LOOK", " glossy ");
    same(ssrLook(table, conf, &unknown), QString("glossy"), "a look name is trimmed, as the client does");
    check(unknown.isEmpty(), "and is not reported");
    same(effectiveSsrValue(table, conf, upOnly), SsrValue({0.0}), "glossy's switch");

    // A knob's override.
    check(setSsrKnob(conf, intensity, {0.7}), "set intensity");
    check(setSsrKnob(conf, tint, {0.5, 1.0, 1.5}), "set the tint");
    check(setSsrKnob(conf, steps, {20.0}), "set steps");
    check(setSsrKnob(conf, upOnly, {1.0}), "set up_only");
    check(!setSsrKnob(conf, intensity, {1.5}), "out of range is refused");
    check(!setSsrKnob(conf, tint, {0.5}), "a colour needs three values");
    check(!setSsrKnob(conf, steps, {70.0}), "a whole number out of range is refused");
    check(conf.save(&error), "save: " + error);
    same(readAll(path),
         QByteArray("# mine\nINTOXICATED_FPS=144\nINTOXICATED_SSR=1\nINTOXICATED_SSR_LOOK= glossy \n"
                    "INTOXICATED_SSR_INTENSITY=0.7\nINTOXICATED_SSR_TINT=0.5,1,1.5\nINTOXICATED_SSR_STEPS=20\n"
                    "INTOXICATED_SSR_UP_ONLY=1\n"),
         "each override is one line, as the client reads it");
    conf = reloaded();
    same(effectiveSsrValue(table, conf, intensity), SsrValue({0.7}), "an override wins over the look");
    same(effectiveSsrValue(table, conf, upOnly), SsrValue({1.0}), "also for a switch");
    same(ssrOverrides(table, conf), QStringList({"intensity", "up_only", "tint", "steps"}), "the overrides, in knob order");
    check(setSsrKnob(conf, intensity, {0.25}), "set intensity again");
    same(conf.value("INTOXICATED_SSR_INTENSITY").value_or(""), QString("0.25"), "the line is replaced");

    resetSsrKnob(conf, intensity);
    check(!conf.contains("INTOXICATED_SSR_INTENSITY"), "reset removes the override");
    same(effectiveSsrValue(table, conf, intensity), SsrValue({0.4}), "and the look's value is back");
    check(conf.save(&error), "save: " + error);
    check(!readAll(path).contains("INTOXICATED_SSR_INTENSITY"), "also in the file");

    // Overrides the client would not take, or blank ones.
    conf.set("INTOXICATED_SSR_INTENSITY", "lots");
    conf.set("INTOXICATED_SSR_THICKNESS", "  ");
    check(ssrOverride(conf, intensity).has_value(), "a bad override is still an override");
    same(effectiveSsrValue(table, conf, intensity), SsrValue({0.4}), "but the client uses the look's value");
    check(!ssrOverride(conf, *table.knob("thickness")), "a blank line is no override");

    // Picking a look removes every override, duplicates and bad ones too.
    writeAll(path, "INTOXICATED_SSR=1\nINTOXICATED_SSR_STEPS=8\n# keep\nINTOXICATED_SSR_STEPS=12\n"
                   "INTOXICATED_SSR_INTENSITY=lots\nINTOXICATED_SSR_TINT=1,1,1\nINTOXICATED_SSR_LOOK=glossy\n"
                   "INTOXICATED_SSRX=1\n");
    conf = reloaded();
    same(ssrOverrides(table, conf), QStringList({"intensity", "tint", "steps"}), "three overrides");
    check(applySsrLook(table, conf, "wet_road"), "pick wet_road over them");
    check(conf.save(&error), "save: " + error);
    same(readAll(path), QByteArray("INTOXICATED_SSR=1\n# keep\nINTOXICATED_SSR_LOOK=wet_road\nINTOXICATED_SSRX=1\n"),
         "every override is gone, the rest kept");
    conf = reloaded();
    check(applySsrLook(table, conf, "competitive"), "and the default look");
    check(conf.save(&error), "save: " + error);
    same(readAll(path), QByteArray("INTOXICATED_SSR=1\n# keep\nINTOXICATED_SSRX=1\n"), "leaves no look line");
    same(strays(dir, {"settings.conf"}), QStringList(), "no temporary file left beside settings.conf");
    QFile::remove(path);
}

// readSsrTable runs `intoxicated --ssr-looks`: a fake launcher on PATH, in
// the scratch directory.
void testSsrLauncher(const QString &scratch) {
    const QString bin = scratch + "/bin";
    QDir().mkpath(bin);
    writeAll(scratch + "/looks.json", kSsrLooks);
    const QString launcher = bin + "/intoxicated";
    const auto fake = [&](const QByteArray &body) {
        writeAll(launcher, "#!/bin/sh\n" + body);
        QFile::setPermissions(launcher, QFileDevice::ReadOwner | QFileDevice::WriteOwner | QFileDevice::ExeOwner);
    };
    const QByteArray savedPath = qgetenv("PATH");
    qputenv("PATH", QFile::encodeName(bin) + ":" + savedPath);
    QString error;
    fake("[ \"$1\" = --ssr-looks ] && [ $# -eq 1 ] && exec cat '" + QFile::encodeName(scratch) + "/looks.json'\nexit 7\n");
    same(launcherPath(), launcher, "the fake launcher is the one found");
    const auto table = readSsrTable(&error);
    check(table.has_value(), "the looks come from intoxicated --ssr-looks: " + error);
    if (table)
        same(table->looks.size(), 3, "all of them");

    fake("echo '{\"looks\": []}'\n");
    check(!readSsrTable(&error), "something else is refused");
    check(error.startsWith("intoxicated --ssr-looks printed something else"), "and said: " + error);

    fake("echo 'intoxicated: target/release/intoxicated-run is not built' >&2\nexit 1\n");
    check(!readSsrTable(&error), "a failed launcher is reported");
    check(error.contains("is not built"), "with what it said: " + error);
    qputenv("PATH", savedPath);
}

} // namespace

int runSelfTest() {
    QTemporaryDir scratch;
    if (!scratch.isValid()) {
        std::fprintf(stderr, "self-test: no scratch directory\n");
        return 1;
    }
    const QString root = QFileInfo(scratch.path()).canonicalFilePath();
    qputenv("XDG_CONFIG_HOME", QFile::encodeName(root + "/config"));
    qputenv("XDG_DATA_HOME", QFile::encodeName(root + "/data"));
    if (!settingsConfPath().startsWith(root + "/") || !profilesRoot().startsWith(root + "/")) {
        std::fprintf(stderr, "self-test: the paths are not in the scratch directory; refusing to run\n");
        return 1;
    }

    testPaths(root);
    testSettingsConf();
    testFlagRules();
    testFlagsFiles();
    testImport();
    testPresets();
    testQualityPresets();
    testProfileFiles();
    testVideoMemoryDefault();
    testProfiles();
    testSndstat();
    testSsrTable();
    testSsrValues();
    testSsrSettings();
    testSsrLauncher(root);

    std::printf("self-test: %d checks, %d failed (scratch %s)\n", g_checks, g_failures, qPrintable(root));
    return g_failures == 0 ? 0 : 1;
}

} // namespace intox
