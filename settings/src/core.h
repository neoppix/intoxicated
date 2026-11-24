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

// A whole-profile graphics level. Lowest to High use only flags Roblox allows
// on its own Windows client. Potato goes below Lowest with any rendering flag
// the 2.748 engine has that lowers cost, and can break game content. No
// preset touches network, physics or camera flags.
struct QualityPreset {
    QString name;
    QString description;
    // Set by the preset; every other key in qualityPresetKeys() is removed.
    QList<QPair<QString, QString>> flags;
    // Shown as a warning, and confirmed before the preset is applied.
    QString warning = {};
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

// --- Profile files ----------------------------------------------------------

// What Export writes and Import reads: one JSON object,
// {"intoxicated-profile": 1, "name": "<profile>", "flags": {...}}. The
// sign-in and Roblox's storage are never in it: a session cookie in a file
// that gets passed around is someone else's way into the account.
QByteArray profileDocument(const QString &name, const FlagMap &flags);

struct ProfileFile {
    QString name;
    FlagMap flags;
    // Flags left out because their name or value would be refused.
    QStringList skipped;
};

// Parse a profile file; false with ERROR for anything that is not one.
bool parseProfileDocument(const QByteArray &data, ProfileFile *out, QString *error);
// A free profile name from WANTED: itself if free and valid, else with
// -2, -3, ... appended (cut to stay within 32 characters).
QString freeProfileName(const QString &wanted);
// Create profile NAME holding FLAGS. Refused if it already exists.
bool importProfile(const QString &name, const FlagMap &flags, QString *error);

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

// --- Effects (the Shaders page) ---------------------------------------------

// The client draws five effects before the HUD: screen-space reflections,
// depth of field, bloom, lens and sharpen. Each is set by settings.conf lines
// the client reads at start: INTOXICATED_SSR=1 (or _DOF, _BLOOM, _LENS,
// _SHARPEN) turns it on,
// INTOXICATED_SSR_LOOK=<name> starts from a named look and
// INTOXICATED_SSR_<KNOB>=<value> overrides one knob of it. The effects, their
// knobs and looks are the client's own, read from `intoxicated
// --shader-looks` (`--ssr-looks` for the reflections alone), so this app
// keeps no copy of them. The types keep the Ssr names of the first effect.

inline const QString kSsrSetting = QStringLiteral("INTOXICATED_SSR");
inline const QString kSsrLookSetting = QStringLiteral("INTOXICATED_SSR_LOOK");

enum class SsrKind { Number, Whole, Switch, Rgb };

// One value: one number, or three for an Rgb knob.
using SsrValue = QList<double>;

struct SsrKnob {
    QString name;
    QString env;
    SsrKind kind = SsrKind::Number;
    double min = 0;
    double max = 1;
    SsrValue defaultValue;
};

struct SsrLook {
    QString name;
    QString description;
    // Every knob's value when the client starts from this look.
    QMap<QString, SsrValue> values;
};

// One effect: its switch and look variables, knobs and looks.
struct SsrTable {
    // Its devctl name ("ssr"), its title and one line on what it does; empty
    // when read from `--ssr-looks`, which does not carry them.
    QString name;
    QString title;
    QString summary;
    QString switchEnv = kSsrSetting;
    QString lookEnv = kSsrLookSetting;
    QList<SsrKnob> knobs;
    QList<SsrLook> looks;
    QString defaultLook;

    const SsrKnob *knob(const QString &name) const;
    const SsrLook *look(const QString &name) const;
};

// Parse what `intoxicated --ssr-looks` prints; false with ERROR for anything
// that is not a usable table.
bool parseSsrTable(const QByteArray &json, SsrTable *out, QString *error);
std::optional<SsrTable> readSsrTable(QString *error);
// Parse what `intoxicated --shader-looks` prints: every effect, in the order
// the client draws them, each a usable table with its own variables.
bool parseShaderTables(const QByteArray &json, QList<SsrTable> *out, QString *error);

// A whole-page mode: every effect's switch and look at once. In
// settings.conf INTOXICATED_SHADERS_MODE=<name> fills in each effect's switch
// and look where its own line is unset; this app writes a picked mode as the
// effects' own lines instead, so the page and the file always agree.
inline const QString kShaderModeSetting = QStringLiteral("INTOXICATED_SHADERS_MODE");

struct ShaderMode {
    QString name;
    QString title;
    QString description;
    // Effect name to its switch and look.
    QMap<QString, QPair<bool, QString>> effects;
};

// Whether JSON is what a client built without the visual effects prints for
// `--shader-looks` and `--ssr-looks` (install.sh --no-effects): an object
// whose "available" is false.
bool effectsUnavailable(const QByteArray &json);
// How the effects are added to such a client.
inline const QString kAddEffectsCommand = QStringLiteral("intoxicated --update --effects");

// `intoxicated --shader-looks`, parsed. MODES, when given, gets the
// whole-page modes the client lists (none from a client that lists none).
// From a client built without the effects: nullopt, with UNAVAILABLE (when
// given) set and ERROR saying so.
std::optional<QList<SsrTable>> readShaderTables(QString *error, QList<ShaderMode> *modes = nullptr,
                                                bool *unavailable = nullptr);

// Parse the "modes" of what `intoxicated --shader-looks` prints, against its
// TABLES: every mode names a look of each effect. No "modes" is no modes.
bool parseShaderModes(const QByteArray &json, const QList<SsrTable> &tables, QList<ShaderMode> *out, QString *error);
const ShaderMode *findShaderMode(const QList<ShaderMode> &modes, const QString &name);
// Write out what an INTOXICATED_SHADERS_MODE line asks for as the effects' own
// lines, where theirs are unset, and remove it. Whether CONF changed.
bool expandShaderMode(const QList<SsrTable> &tables, const QList<ShaderMode> &modes, SettingsConf &conf);
// The mode the client starts in: the name of the one whose switches and
// looks every effect has (an effect off in both counts as matching whatever
// its look), an empty string when none (Custom). An effect that is on with
// fine tuning matches no mode.
QString currentShaderMode(const QList<SsrTable> &tables, const QList<ShaderMode> &modes, const SettingsConf &conf);
// Every effect's switch and look as the mode sets them, every knob's
// override removed. Refused, with CONF unchanged, for a name that is no mode.
bool applyShaderMode(const QList<SsrTable> &tables, const QList<ShaderMode> &modes, SettingsConf &conf,
                     const QString &mode);

// "wet_road" as a button says it: "Wet road".
QString ssrLookTitle(const QString &name);

// A whole knob of EFFECT whose values are named choices rather than numbers
// (the depth of field's focus mode): one title per value from its minimum
// to its maximum. Empty for any other knob, and for one whose range no
// longer has exactly as many values as there are names.
QStringList knobChoices(const QString &effect, const SsrKnob &knob);

// A value as the client reads it: a number in range, a whole number, 0 or 1,
// or r,g,b (or one number for all three). nullopt when the client would not
// take TEXT.
std::optional<SsrValue> parseSsrValue(const SsrKnob &knob, const QString &text);
// F as the shortest fixed-point text that reads back as the same float.
QString shortestFloat(float f);
// A value as written to settings.conf: the shortest text for each number.
QString formatSsrValue(const SsrKnob &knob, const SsrValue &value);

// Whether INTOXICATED_SSR is 1, the only value that turns reflections on.
bool ssrOn(const SettingsConf &conf);
// On writes INTOXICATED_SSR=1, off removes it.
void setSsrOn(SettingsConf &conf, bool on);
// The same for any effect's switch (TABLE's, or the variable SWITCHENV).
bool effectOn(const QString &switchEnv, const SettingsConf &conf);
void setEffectOn(const QString &switchEnv, SettingsConf &conf, bool on);
// The look the client starts from: the one the table's look variable names,
// else the default one (also when it names no look). UNKNOWN is set to the
// setting's text when it names no look.
QString ssrLook(const SsrTable &table, const SettingsConf &conf, QString *unknown = nullptr);
// The knob's line in settings.conf, when it has a non-blank one.
std::optional<QString> ssrOverride(const SettingsConf &conf, const SsrKnob &knob);
// The knobs that have an override line.
QStringList ssrOverrides(const SsrTable &table, const SettingsConf &conf);
// The value the client starts with: a usable override, else the look's.
SsrValue effectiveSsrValue(const SsrTable &table, const SettingsConf &conf, const SsrKnob &knob);
// Start from LOOK: the default look removes the look variable, another one
// writes it. Either way every knob's override is removed. Refused, with CONF
// unchanged, for a name that is not a look.
bool applySsrLook(const SsrTable &table, SettingsConf &conf, const QString &look);
// Write the knob's override; refused for a value the client would not take.
bool setSsrKnob(SettingsConf &conf, const SsrKnob &knob, const SsrValue &value);
void resetSsrKnob(SettingsConf &conf, const SsrKnob &knob);

// --- Macros ----------------------------------------------------------------

// In-game macros: a key press, seen on the client's own input path while the
// Roblox window is focused, plays a recorded sequence of input back through
// that same path. They live in <profile>/macros.json, which the client reads
// once at launch (crate::macros). The feature is off unless INTOXICATED_MACROS=1.
inline const QString kMacrosSetting = QStringLiteral("INTOXICATED_MACROS");

// <profile>/macros.json.
QString macrosPath(const QString &profile);

// What one step of a macro does. The names match the "type" the file stores
// and the client reads: tap, keydown, keyup, move, click, buttondown,
// buttonup, scroll, text, wait.
enum class StepKind { Tap, KeyDown, KeyUp, Move, Click, ButtonDown, ButtonUp, Scroll, Text, Wait };

struct MacroStep {
    StepKind kind = StepKind::Tap;
    // Tap, KeyDown, KeyUp: a physical key as a Linux evdev code, and the
    // modifiers held with it.
    int evdev = 0;
    bool ctrl = false;
    bool shift = false;
    bool alt = false;
    // Move, Click, ButtonDown, ButtonUp, Scroll: window coordinates.
    double x = 0;
    double y = 0;
    // Click, ButtonDown, ButtonUp: 1 left, 2 right, 3 middle.
    int button = 1;
    // Scroll: wheel detents, away from the user positive.
    double detents = 0;
    // Text: what is typed.
    QString text;
    // Wait: the pause in milliseconds.
    quint64 ms = 0;
};

struct MacroDef {
    QString name;
    bool enabled = true;
    // The trigger: a physical key and the modifiers that must be held with it.
    int triggerEvdev = 0;
    bool triggerCtrl = false;
    bool triggerShift = false;
    bool triggerAlt = false;
    // Keep the trigger key from the game while the macro runs.
    bool swallow = false;
    // Repeat the steps while the trigger key is held.
    bool repeat = false;
    QList<MacroStep> steps;
};

struct MacrosFile {
    bool exists = false;
    // Non-empty when the file cannot be used (not JSON, not an object, or
    // "macros" is not an array); it is then never written over.
    QString error;
    QList<MacroDef> macros;
    // Macros left out because they could not be read, each with its reason.
    QStringList skipped;
};

// Parse a macros document. Blank text is no macros, not an error. A macro that
// cannot be read is listed in SKIPPED rather than failing the whole document.
bool parseMacros(const QByteArray &data, QList<MacroDef> *out, QString *error, QStringList *skipped = nullptr);
MacrosFile loadMacros(const QString &path);
bool saveMacros(const QString &path, const QList<MacroDef> &macros, QString *error);

// Empty when NAME is a usable macro name (non-blank once trimmed), else why.
QString checkMacroName(const QString &name);
// Empty when the macro is complete enough to save (a name, a trigger key and
// at least one step), else the first reason it is not.
QString checkMacro(const MacroDef &m);

// The keys the editor offers and the file stores, as (evdev code, label). One
// fixed table, shared so the label a code shows and the code a label saves
// always agree.
struct KeyName {
    int evdev;
    QString label;
};
const QList<KeyName> &keyNames();
// A code's label, or "Key <n>" for one not in the table.
QString keyLabel(int evdev);
// A label's code, or -1 for one not in the table.
int keyEvdev(const QString &label);
// A step as one line for the editor's list, e.g. "Tap  Space" or "Wait  40 ms".
QString describeStep(const MacroStep &step);

// --- Self-test -------------------------------------------------------------

int runSelfTest();

} // namespace intox
