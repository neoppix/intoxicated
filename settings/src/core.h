// Intoxicated Settings: the files it reads and writes, with no widgets.
//
// Everything here uses QtCore only, so `intoxicated-settings --self-test` can
// run it against scratch XDG directories without a display.
//
//   settings.conf  ${XDG_CONFIG_HOME:-$HOME/.config}/intoxicated/settings.conf
//                  KEY=VALUE lines read by the launcher. Lines this app does
//                  not manage (comments, unknown keys) are kept.
//   flags.json     ${XDG_DATA_HOME:-$HOME/.local/share}/intoxicated/
//                  profiles/<profile>/flags.json, one JSON object.
//   profiles       directories under .../intoxicated/profiles/.
//
// Every write goes to a temporary file that is renamed over the old one.

#pragma once

#include <QList>
#include <QMap>
#include <QSet>
#include <QString>
#include <QStringList>

#include <functional>
#include <optional>

namespace intox {

// --- Paths -----------------------------------------------------------------

QString configHome();
QString dataHome();
QString settingsConfPath();
QString profilesRoot();
QString profileDir(const QString &name);
QString flagsPath(const QString &profile);

// --- settings.conf ---------------------------------------------------------

// Whether KEY can be written by this app: INTOXICATED_ then [A-Z0-9_]+.
bool isSettingKey(const QString &key);

class SettingsConf {
public:
    // Reads PATH. A missing file is an empty one.
    bool load(const QString &path, QString *error = nullptr);
    const QString &path() const { return m_path; }

    // The value the launcher exports: the last line for KEY.
    std::optional<QString> value(const QString &key) const;
    bool contains(const QString &key) const { return value(key).has_value(); }

    // Replace KEY's last line in place (earlier duplicates are dropped) or
    // append one. Refused for a key that is not a setting key or a value
    // with a line break.
    bool set(const QString &key, const QString &value);
    void remove(const QString &key);
    void clear() { m_lines.clear(); }

    QString text() const;
    bool save(QString *error = nullptr) const;

private:
    // The key a line sets for the launcher, or an empty string.
    static QString keyOf(const QString &line);

    QString m_path;
    QStringList m_lines;
};

// Read settings.conf, set KEY to VALUE (or remove it for nullopt), write it.
bool editSetting(const QString &key, const std::optional<QString> &value, QString *error);

// --- Flags -----------------------------------------------------------------

enum class FlagKind { Bool, Int, String, Unknown };

FlagKind flagKind(const QString &name);
// Empty when NAME is a flag name this app writes, else the reason.
QString checkFlagName(const QString &name);
// Empty when VALUE suits NAME's kind (and NORMALISED holds what to write),
// else the reason.
QString checkFlagValue(const QString &name, const QString &value, QString *normalised);

using FlagMap = QMap<QString, QString>;

struct FlagsFile {
    bool exists = false;
    // Non-empty when the file cannot be used: not JSON, not an object, or a
    // value that is not text, a number or true/false. It is then never
    // written over; see backupBrokenFlags.
    QString error;
    FlagMap values;
};

// Parse a flags document: a JSON object whose values are strings, numbers or
// booleans, taken as text the way the client takes them. Blank text is an
// empty object. With SKIPPED, a value of another type is listed there and
// left out instead of failing the whole document (for imports).
bool parseFlags(const QByteArray &data, FlagMap *values, QString *error, QStringList *skipped = nullptr);
FlagsFile loadFlags(const QString &path);
bool saveFlags(const QString &path, const FlagMap &values, QString *error);
// Rename a broken flags.json to flags.json.bad-<time> and start an empty one.
// Returns the backup's path, or an empty string with ERROR set.
QString backupBrokenFlags(const QString &path, QString *error);
// Read the profile's flags, change them, write them back. Refused, with the
// file untouched, when it is broken.
bool editFlags(const QString &path, const std::function<void(FlagMap &)> &edit, QString *error);

// Values compared as the client reads them: True/true alike, 032 and 32 alike.
bool sameFlagValue(const QString &name, const QString &a, const QString &b);

// --- Presets ---------------------------------------------------------------

struct SwitchPreset {
    QString title;
    QString description;
    // All of these hold when the switch is on.
    QList<QPair<QString, QString>> on;
    // Written when it is turned off; empty means the keys are removed.
    QList<QPair<QString, QString>> off;
};

QList<SwitchPreset> switchPresets();
// The value KEY has at launch: flags.json's, else the client's built-in one.
std::optional<QString> effectiveFlag(const FlagMap &flags, const FlagMap &defaults, const QString &key);
bool presetIsOn(const SwitchPreset &preset, const FlagMap &flags, const FlagMap &defaults);
// Turning on removes a key whose built-in value already matches, else writes
// it; turning off writes the opposite values (or removes the keys).
void applyPreset(const SwitchPreset &preset, bool on, FlagMap &flags, const FlagMap &defaults);

// --- Quality presets -------------------------------------------------------

// A whole-profile graphics level. Only flags Roblox allows on its own Windows
// client are used, so no preset gives an edge another player could not have.
struct QualityPreset {
    QString name;
    QString description;
    // Set by the preset; every other key in qualityPresetKeys() is removed.
    QList<QPair<QString, QString>> flags;
};

QList<QualityPreset> qualityPresets();
// Every key any quality preset sets.
QStringList qualityPresetKeys();
// The preset the profile's flags match exactly over qualityPresetKeys(), or -1
// for a custom mix.
int matchingQualityPreset(const FlagMap &flags);
void applyQualityPreset(const QualityPreset &preset, FlagMap &flags);

inline const QString kFpsFlag = QStringLiteral("DFIntTaskSchedulerTargetFps");
inline const QString kVideoMemoryFlag = QStringLiteral("FIntRenderForceVideoMemorySize");
inline const QString kFpsSetting = QStringLiteral("INTOXICATED_FPS");
inline const QString kVideoMemorySetting = QStringLiteral("INTOXICATED_VIDEO_MEMORY");

// The frame rate cap at the next launch: flags.json's value, else the
// built-in one, else what the launcher adds (9999, INTOXICATED_FPS, or
// Roblox's own 60 when INTOXICATED_FPS=off).
QString effectiveFps(const FlagMap &flags, const FlagMap &defaults, const SettingsConf &conf);
// The video memory budget at the next launch, in bytes, or an empty string
// for Roblox's own default.
QString effectiveVideoMemory(const FlagMap &flags, const FlagMap &defaults, const SettingsConf &conf);
// "Roblox's default": the key leaves flags.json and the launcher is told not
// to add it back.
bool useRobloxVideoMemory(const QString &profile, QString *error);

// --- Import ----------------------------------------------------------------

struct ImportItem {
    QString name;
    std::optional<QString> current;
    QString value;   // normalised when valid
    QString problem; // non-empty: cannot be imported
};

// What importing INCOMING into CURRENT would add or change. Keys whose value
// is already the same are left out; SKIPPED lists values of the wrong type.
QList<ImportItem> planImport(const FlagMap &current, const FlagMap &incoming, const QStringList &skipped = {});
// Merge the chosen, valid items.
void applyImport(FlagMap &flags, const QList<ImportItem> &items, const QSet<QString> &chosen);

// --- Profiles --------------------------------------------------------------

inline const QString kDefaultProfile = QStringLiteral("default");

bool isValidProfileName(const QString &name);

struct ProfileInfo {
    QString name;
    bool exists = false;
    bool signedIn = false;
    bool inUse = false;
};

// Whether a client holds the profile's lock right now. Taken and released
// at once with flock(LOCK_EX | LOCK_NB) on a read-only open.
bool profileInUse(const QString &dir);
// Signed in means a cookies file exists; its content is never read.
bool profileSignedIn(const QString &dir);
// `default` first, then every directory with a usable name, sorted, plus
// EXTRA (the default profile named in settings.conf) when it is missing.
QList<ProfileInfo> listProfiles(const QString &extra = QString());
bool createProfile(const QString &name, QString *error);
bool deleteProfile(const QString &name, QString *error);
// The profile the launcher uses when nothing else is said:
// settings.conf's INTOXICATED_PROFILE when usable, else `default`.
QString defaultProfileName(const SettingsConf &conf);

// --- Sound devices ---------------------------------------------------------

struct SoundDevice {
    int unit = -1;
    QString name;
    bool play = false;
    bool record = false;
    bool isDefault = false;
    QString node() const { return QStringLiteral("/dev/dsp%1").arg(unit); }
};

// FreeBSD's /dev/sndstat: lines `pcmN: <Name> (play/rec)`, maybe followed
// by ` default`.
QList<SoundDevice> parseSndstat(const QString &text);
QList<SoundDevice> soundDevices();

// --- The launcher ----------------------------------------------------------

// `intoxicated` on PATH, else /usr/local/bin/intoxicated.
QString launcherPath();
// Run `intoxicated ARGS` and return its stdout, or nullopt with ERROR set.
std::optional<QByteArray> runLauncher(const QStringList &args, int timeoutMs, QString *error);
// `intoxicated --default-flags`, parsed.
std::optional<FlagMap> readDefaultFlags(QString *error);

// --- Self-test -------------------------------------------------------------

int runSelfTest();

} // namespace intox
