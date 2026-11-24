// Intoxicated Settings: settings.conf, flags.json, profiles, sound devices
// and the launcher. See core.h.

#include "core.h"

#include <QDateTime>
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QJsonParseError>
#include <QJsonValue>
#include <QLocale>
#include <QProcess>
#include <QRegularExpression>
#include <QSaveFile>
#include <QStandardPaths>

#include <fcntl.h>
#include <signal.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/user.h>
#include <libutil.h>
#include <unistd.h>

#include <algorithm>
#include <cerrno>
#include <climits>
#include <cstdlib>
#include <cstring>

namespace intox {

// --- Paths -----------------------------------------------------------------

// ${NAME:-$HOME/FALLBACK}: unset and empty alike fall back, as in the launcher.
static QString xdgDir(const char *name, const char *fallback) {
    const QString set = qEnvironmentVariable(name);
    if (!set.isEmpty())
        return set;
    return QDir::homePath() + QLatin1Char('/') + QLatin1String(fallback);
}

QString configHome() { return xdgDir("XDG_CONFIG_HOME", ".config"); }
QString dataHome() { return xdgDir("XDG_DATA_HOME", ".local/share"); }
QString settingsConfPath() { return configHome() + QStringLiteral("/intoxicated/settings.conf"); }
QString profilesRoot() { return dataHome() + QStringLiteral("/intoxicated/profiles"); }
QString profileDir(const QString &name) { return profilesRoot() + QLatin1Char('/') + name; }
QString flagsPath(const QString &profile) { return profileDir(profile) + QStringLiteral("/flags.json"); }

// --- Atomic writes ---------------------------------------------------------

// QSaveFile writes beside the target and renames it in on commit, keeping
// the old file's permissions.
static bool writeAtomically(const QString &path, const QByteArray &data, QString *error) {
    const QFileInfo info(path);
    if (!QDir().mkpath(info.absolutePath())) {
        if (error)
            *error = QStringLiteral("%1: could not create the directory").arg(info.absolutePath());
        return false;
    }
    QSaveFile file(path);
    if (!file.open(QIODevice::WriteOnly)) {
        if (error)
            *error = QStringLiteral("%1: %2").arg(path, file.errorString());
        return false;
    }
    if (file.write(data) != data.size() || !file.commit()) {
        if (error)
            *error = QStringLiteral("%1: %2").arg(path, file.errorString());
        file.cancelWriting();
        return false;
    }
    return true;
}

// --- settings.conf ---------------------------------------------------------

bool isSettingKey(const QString &key) {
    static const QRegularExpression re(QStringLiteral("^INTOXICATED_[A-Z0-9_]+$"));
    return re.match(key).hasMatch();
}

QString SettingsConf::keyOf(const QString &line) {
    // The launcher's test: ^INTOXICATED_[A-Z0-9_]*=
    static const QRegularExpression re(QStringLiteral("^(INTOXICATED_[A-Z0-9_]*)="));
    const auto m = re.match(line);
    return m.hasMatch() ? m.captured(1) : QString();
}

bool SettingsConf::load(const QString &path, QString *error) {
    m_path = path;
    m_lines.clear();
    QFile file(path);
    if (!file.exists())
        return true;
    if (!file.open(QIODevice::ReadOnly)) {
        if (error)
            *error = QStringLiteral("%1: %2").arg(path, file.errorString());
        return false;
    }
    QString text = QString::fromUtf8(file.readAll());
    if (text.endsWith(QLatin1Char('\n')))
        text.chop(1);
    if (!text.isEmpty() || file.size() > 0)
        m_lines = text.split(QLatin1Char('\n'));
    return true;
}

std::optional<QString> SettingsConf::value(const QString &key) const {
    for (auto it = m_lines.crbegin(); it != m_lines.crend(); ++it) {
        if (keyOf(*it) == key)
            return it->mid(key.size() + 1);
    }
    return std::nullopt;
}

bool SettingsConf::set(const QString &key, const QString &value) {
    if (!isSettingKey(key) || value.contains(QLatin1Char('\n')) || value.contains(QLatin1Char('\r')))
        return false;
    const QString line = key + QLatin1Char('=') + value;
    int last = -1;
    for (int i = 0; i < m_lines.size(); ++i) {
        if (keyOf(m_lines[i]) == key)
            last = i;
    }
    if (last < 0) {
        m_lines.append(line);
        return true;
    }
    m_lines[last] = line;
    for (int i = last - 1; i >= 0; --i) {
        if (keyOf(m_lines[i]) == key)
            m_lines.removeAt(i);
    }
    return true;
}

void SettingsConf::remove(const QString &key) {
    m_lines.erase(std::remove_if(m_lines.begin(), m_lines.end(),
                                 [&](const QString &line) { return keyOf(line) == key; }),
                  m_lines.end());
}

QString SettingsConf::text() const {
    if (m_lines.isEmpty())
        return QString();
    return m_lines.join(QLatin1Char('\n')) + QLatin1Char('\n');
}

bool SettingsConf::save(QString *error) const {
    return writeAtomically(m_path, text().toUtf8(), error);
}

bool editSetting(const QString &key, const std::optional<QString> &value, QString *error) {
    SettingsConf conf;
    if (!conf.load(settingsConfPath(), error))
        return false;
    if (value) {
        if (!conf.set(key, *value)) {
            if (error)
                *error = QStringLiteral("%1 cannot be set to that value").arg(key);
            return false;
        }
    } else {
        conf.remove(key);
    }
    return conf.save(error);
}

// --- Flags -----------------------------------------------------------------

namespace {
struct Prefix {
    const char *text;
    FlagKind kind;
};
const Prefix kPrefixes[] = {
    {"FFlag", FlagKind::Bool},     {"DFFlag", FlagKind::Bool},    {"SFFlag", FlagKind::Bool},
    {"FInt", FlagKind::Int},       {"DFInt", FlagKind::Int},      {"FString", FlagKind::String},
    {"DFString", FlagKind::String}, {"FLog", FlagKind::String},   {"DFLog", FlagKind::String},
};
} // namespace

FlagKind flagKind(const QString &name) {
    for (const Prefix &p : kPrefixes) {
        if (name.startsWith(QLatin1String(p.text)))
            return p.kind;
    }
    return FlagKind::Unknown;
}

QString checkFlagName(const QString &name) {
    if (name.isEmpty())
        return QStringLiteral("A flag needs a name.");
    for (const Prefix &p : kPrefixes) {
        const QLatin1String prefix(p.text);
        if (!name.startsWith(prefix))
            continue;
        static const QRegularExpression rest(QStringLiteral("^[A-Za-z0-9_]+$"));
        const QString tail = name.mid(prefix.size());
        if (tail.isEmpty())
            return QStringLiteral("%1 needs a name after it.").arg(prefix);
        if (!rest.match(tail).hasMatch())
            return QStringLiteral("A flag name has only letters, digits and _ after its prefix.");
        return QString();
    }
    return QStringLiteral("A flag name starts with FFlag, DFFlag, SFFlag, FInt, DFInt, FString, DFString, "
                          "FLog or DFLog.");
}

QString checkFlagValue(const QString &name, const QString &value, QString *normalised) {
    switch (flagKind(name)) {
    case FlagKind::Bool: {
        const QString v = value.trimmed();
        if (v.compare(QLatin1String("true"), Qt::CaseInsensitive) == 0) {
            *normalised = QStringLiteral("True");
            return QString();
        }
        if (v.compare(QLatin1String("false"), Qt::CaseInsensitive) == 0) {
            *normalised = QStringLiteral("False");
            return QString();
        }
        return QStringLiteral("%1 takes True or False.").arg(name);
    }
    case FlagKind::Int: {
        static const QRegularExpression digits(QStringLiteral("^[+-]?[0-9]+$"));
        const QString v = value.trimmed();
        bool ok = false;
        const qlonglong n = v.toLongLong(&ok);
        if (!digits.match(v).hasMatch() || !ok || n < INT_MIN || n > INT_MAX)
            return QStringLiteral("%1 takes a whole number from -2147483648 to 2147483647.").arg(name);
        *normalised = QString::number(n);
        return QString();
    }
    case FlagKind::String:
    case FlagKind::Unknown:
        *normalised = value;
        return QString();
    }
    return QString();
}

bool sameFlagValue(const QString &name, const QString &a, const QString &b) {
    QString na, nb;
    if (checkFlagValue(name, a, &na).isEmpty() && checkFlagValue(name, b, &nb).isEmpty())
        return na == nb;
    return a == b;
}

// A JSON scalar as the client takes it: strings as they are, numbers and
// booleans as their JSON text.
static std::optional<QString> scalarText(const QJsonValue &v) {
    switch (v.type()) {
    case QJsonValue::String:
        return v.toString();
    case QJsonValue::Bool:
        return v.toBool() ? QStringLiteral("true") : QStringLiteral("false");
    case QJsonValue::Double: {
        const double d = v.toDouble();
        const qint64 whole = v.toInteger(LLONG_MIN);
        if (whole != LLONG_MIN || d == double(LLONG_MIN))
            return QString::number(whole);
        return QString::number(d, 'g', QLocale::FloatingPointShortest);
    }
    default:
        return std::nullopt;
    }
}

bool parseFlags(const QByteArray &data, FlagMap *values, QString *error, QStringList *skipped) {
    values->clear();
    if (data.trimmed().isEmpty())
        return true;
    QJsonParseError parseError;
    const QJsonDocument doc = QJsonDocument::fromJson(data, &parseError);
    if (parseError.error != QJsonParseError::NoError) {
        *error = QStringLiteral("not valid JSON (%1 at byte %2)").arg(parseError.errorString()).arg(parseError.offset);
        return false;
    }
    if (!doc.isObject()) {
        *error = QStringLiteral("not a JSON object of flag names to values");
        return false;
    }
    const QJsonObject object = doc.object();
    for (auto it = object.begin(); it != object.end(); ++it) {
        if (it.key().trimmed().isEmpty()) {
            if (skipped) {
                skipped->append(QStringLiteral("(a flag with an empty name)"));
                continue;
            }
            *error = QStringLiteral("a flag with an empty name");
            return false;
        }
        const auto text = scalarText(it.value());
        if (!text) {
            if (skipped) {
                skipped->append(it.key());
                continue;
            }
            *error = QStringLiteral("%1: a flag value must be text, a number or true/false").arg(it.key());
            return false;
        }
        values->insert(it.key(), *text);
    }
    return true;
}

FlagsFile loadFlags(const QString &path) {
    FlagsFile result;
    QFile file(path);
    if (!file.exists())
        return result;
    result.exists = true;
    if (!file.open(QIODevice::ReadOnly)) {
        result.error = file.errorString();
        return result;
    }
    QString error;
    if (!parseFlags(file.readAll(), &result.values, &error))
        result.error = error;
    return result;
}

bool saveFlags(const QString &path, const FlagMap &values, QString *error) {
    QJsonObject object;
    for (auto it = values.begin(); it != values.end(); ++it)
        object.insert(it.key(), it.value());
    return writeAtomically(path, QJsonDocument(object).toJson(QJsonDocument::Indented), error);
}

QString backupBrokenFlags(const QString &path, QString *error) {
    const QString stamp = QDateTime::currentDateTime().toString(QStringLiteral("yyyyMMdd-HHmmss"));
    QString backup = path + QStringLiteral(".bad-") + stamp;
    for (int n = 2; QFileInfo::exists(backup); ++n)
        backup = path + QStringLiteral(".bad-") + stamp + QLatin1Char('-') + QString::number(n);
    if (!QFile::rename(path, backup)) {
        *error = QStringLiteral("%1 could not be renamed to %2").arg(path, backup);
        return QString();
    }
    if (!saveFlags(path, {}, error))
        return QString();
    return backup;
}

bool editFlags(const QString &path, const std::function<void(FlagMap &)> &edit, QString *error) {
    FlagsFile file = loadFlags(path);
    if (!file.error.isEmpty()) {
        *error = QStringLiteral("%1 cannot be used (%2); it was left as it is").arg(path, file.error);
        return false;
    }
    edit(file.values);
    return saveFlags(path, file.values, error);
}

// --- Presets ---------------------------------------------------------------

QList<SwitchPreset> switchPresets() {
    return {
        {QStringLiteral("Full-resolution light shadows"),
         QStringLiteral("FFlagRenderShadowResolutionScaling = False"),
         {{QStringLiteral("FFlagRenderShadowResolutionScaling"), QStringLiteral("False")}},
         {{QStringLiteral("FFlagRenderShadowResolutionScaling"), QStringLiteral("True")}}},
        {QStringLiteral("Desktop GPU memory pooling"),
         QStringLiteral("FFlagVulkanAllocatorPoolSmallOnly = False, FIntVulkanAllocatorKeepEmptyPages = 32, "
                        "FFlagVulkanAllocatorEmptyPageBudget = False"),
         {{QStringLiteral("FFlagVulkanAllocatorPoolSmallOnly"), QStringLiteral("False")},
          {QStringLiteral("FIntVulkanAllocatorKeepEmptyPages"), QStringLiteral("32")},
          {QStringLiteral("FFlagVulkanAllocatorEmptyPageBudget"), QStringLiteral("False")}},
         // The phone settings the built-in values undo: pool only small
         // allocations and free each page as soon as it empties. INFERRED
         // that Roblox's own value for the page count is 0.
         {{QStringLiteral("FFlagVulkanAllocatorPoolSmallOnly"), QStringLiteral("True")},
          {QStringLiteral("FIntVulkanAllocatorKeepEmptyPages"), QStringLiteral("0")},
          {QStringLiteral("FFlagVulkanAllocatorEmptyPageBudget"), QStringLiteral("True")}}},
        {QStringLiteral("Force 4x MSAA"),
         QStringLiteral("FIntDebugForceMSAASamples = 4"),
         {{QStringLiteral("FIntDebugForceMSAASamples"), QStringLiteral("4")}},
         {}},
    };
}

std::optional<QString> effectiveFlag(const FlagMap &flags, const FlagMap &defaults, const QString &key) {
    if (flags.contains(key))
        return flags.value(key);
    if (defaults.contains(key))
        return defaults.value(key);
    return std::nullopt;
}

bool presetIsOn(const SwitchPreset &preset, const FlagMap &flags, const FlagMap &defaults) {
    for (const auto &[key, wanted] : preset.on) {
        const auto value = effectiveFlag(flags, defaults, key);
        if (!value || !sameFlagValue(key, *value, wanted))
            return false;
    }
    return true;
}

void applyPreset(const SwitchPreset &preset, bool on, FlagMap &flags, const FlagMap &defaults) {
    if (on) {
        for (const auto &[key, wanted] : preset.on) {
            if (defaults.contains(key) && sameFlagValue(key, defaults.value(key), wanted))
                flags.remove(key);
            else
                flags.insert(key, wanted);
        }
    } else if (preset.off.isEmpty()) {
        for (const auto &entry : preset.on)
            flags.remove(entry.first);
    } else {
        for (const auto &[key, value] : preset.off)
            flags.insert(key, value);
    }
}

QList<QualityPreset> qualityPresets() {
    const auto kv = [](const char *k, const char *v) {
        return QPair<QString, QString>(QString::fromLatin1(k), QString::fromLatin1(v));
    };
    return {
        {QStringLiteral("Potato"),
         QStringLiteral("Below Lowest: everything Lowest does, plus no item and weapon pictures, no videos, no "
                        "avatar clothing compositing, one local light update a frame, no terrain material textures, "
                        "the shortest draw distance and cheaper animation. It also sets every texture, mesh and "
                        "resolution limit this engine has; in tests those made no visible difference."),
         {kv("DFIntDebugFRMQualityLevelOverride", "1"),
          kv("FIntDebugForceMSAASamples", "1"),
          kv("FFlagDebugSkyGray", "True"),
          kv("DFFlagDebugPauseVoxelizer", "True"),
          kv("FIntFRMMinGrassDistance", "0"),
          kv("FIntFRMMaxGrassDistance", "0"),
          kv("DFIntCSGLevelOfDetailSwitchingDistance", "0"),
          kv("DFIntCSGLevelOfDetailSwitchingDistanceL12", "0"),
          kv("DFIntCSGLevelOfDetailSwitchingDistanceL23", "0"),
          kv("DFIntCSGLevelOfDetailSwitchingDistanceL34", "0"),
          kv("FIntDebugTextureManagerSkipMips", "8"),
          kv("DFIntDebugLimitMinTextureResolutionWhenSkipMips", "0"),
          kv("DFIntDebugAdditionalNumberOfMipsToSkipForNonAlbedoTextures", "8"),
          kv("FIntTextureCompositorLowResFactor", "1"),
          kv("DFIntTextureCompositorActiveJobs", "0"),
          kv("DFFlagPerformanceControlLowPowerTextureQualityEnabled", "True"),
          kv("DFIntPerformanceControlLowPowerTextureQualityPercent", "0"),
          kv("FStringTerrainMaterialTable2022", ""),
          kv("FStringTerrainMaterialTablePre2022", ""),
          kv("FIntCSGVoxelizerFadeRadius", "0"),
          kv("FIntRenderLocalLightUpdatesMax", "1"),
          kv("FIntRenderLocalLightUpdatesMin", "1"),
          kv("FIntRenderLocalLightFadeInMs", "0"),
          kv("FIntSSAOMipLevels", "0"),
          kv("FIntRobloxGuiBlurIntensity", "0"),
          kv("FIntTerrainArraySliceSize", "4"),
          kv("DFIntDebugRestrictGCDistance", "1"),
          kv("DFIntAnimationLodThrottleMaxFramesToSkip", "10"),
          kv("DFIntAnimationLodDistanceMaxLod0", "0"),
          kv("DFIntAnimationLodDistanceMaxLod1", "0"),
          kv("DFIntDebugDynamicRenderKiloPixels", "100"),
          kv("FIntViewportFrameMaxSize", "0"),
          kv("DFIntVideoMaxNumberOfVideosPlaying", "0"),
          kv("FIntMaxCloudAssetDimension", "64"),
          kv("FStringMaxCloudAssetDimensionByRam", "999999,64"),
          kv("FIntDebugRenderMeshForceLod", "3"),
          kv("FIntDebugRenderMeshLodBudgetOverride", "1"),
          kv("DFIntMipLodBiasPercent", "400")},
         QStringLiteral("Potato may break game content: avatars without clothing, blank item and weapon pictures, "
                        "no videos, plain terrain and things missing in the distance.")},
        {QStringLiteral("Lowest"),
         QStringLiteral("Graphics quality 1, no anti-aliasing, grey sky, no grass, frozen lighting updates and the "
                        "simplest meshes at every distance. For the weakest machines."),
         {kv("DFIntDebugFRMQualityLevelOverride", "1"), kv("FIntDebugForceMSAASamples", "1"),
          kv("FFlagDebugSkyGray", "True"), kv("DFFlagDebugPauseVoxelizer", "True"),
          kv("FIntFRMMinGrassDistance", "0"), kv("FIntFRMMaxGrassDistance", "0"),
          kv("DFIntCSGLevelOfDetailSwitchingDistance", "0"), kv("DFIntCSGLevelOfDetailSwitchingDistanceL12", "0"),
          kv("DFIntCSGLevelOfDetailSwitchingDistanceL23", "0"), kv("DFIntCSGLevelOfDetailSwitchingDistanceL34", "0")}},
        {QStringLiteral("Low"),
         QStringLiteral("Graphics quality 5, no anti-aliasing and no grass, for machines that struggle at Default."),
         {kv("DFIntDebugFRMQualityLevelOverride", "5"), kv("FIntDebugForceMSAASamples", "1"),
          kv("FIntFRMMinGrassDistance", "0"), kv("FIntFRMMaxGrassDistance", "0")}},
        {QStringLiteral("Default"),
         QStringLiteral("Roblox's own settings and the in-game graphics slider, with Intoxicated's desktop fixes."),
         {}},
        {QStringLiteral("High"),
         QStringLiteral("Graphics quality 21, the highest, with 4x anti-aliasing."),
         {kv("DFIntDebugFRMQualityLevelOverride", "21"), kv("FIntDebugForceMSAASamples", "4")}},
        {QStringLiteral("Competitive"),
         QStringLiteral("Mid graphics (quality 9) so the scene stays easy to read, with no anti-aliasing and no "
                        "grass for frame rate and a clear line of sight."),
         {kv("DFIntDebugFRMQualityLevelOverride", "9"), kv("FIntDebugForceMSAASamples", "1"),
          kv("FIntFRMMinGrassDistance", "0"), kv("FIntFRMMaxGrassDistance", "0")}},
    };
}

QStringList qualityPresetKeys() {
    QStringList keys;
    for (const QualityPreset &p : qualityPresets())
        for (const auto &entry : p.flags)
            if (!keys.contains(entry.first))
                keys.append(entry.first);
    return keys;
}

int matchingQualityPreset(const FlagMap &flags) {
    const QStringList keys = qualityPresetKeys();
    const QList<QualityPreset> presets = qualityPresets();
    for (int i = 0; i < presets.size(); ++i) {
        FlagMap want;
        for (const auto &[k, v] : presets[i].flags)
            want.insert(k, v);
        bool same = true;
        for (const QString &k : keys) {
            if (want.contains(k) != flags.contains(k) ||
                (want.contains(k) && !sameFlagValue(k, want.value(k), flags.value(k)))) {
                same = false;
                break;
            }
        }
        if (same)
            return i;
    }
    return -1;
}

void applyQualityPreset(const QualityPreset &preset, FlagMap &flags) {
    for (const QString &k : qualityPresetKeys())
        flags.remove(k);
    for (const auto &[k, v] : preset.flags)
        flags.insert(k, v);
}

QString effectiveFps(const FlagMap &flags, const FlagMap &defaults, const SettingsConf &conf) {
    if (const auto v = effectiveFlag(flags, defaults, kFpsFlag))
        return *v;
    const auto setting = conf.value(kFpsSetting);
    if (!setting || setting->isEmpty())
        return QStringLiteral("9999");
    if (*setting == QLatin1String("off"))
        return QStringLiteral("60");
    return *setting;
}

QString effectiveVideoMemory(const FlagMap &flags, const FlagMap &defaults, const SettingsConf &conf) {
    if (const auto v = effectiveFlag(flags, defaults, kVideoMemoryFlag))
        return *v;
    const auto setting = conf.value(kVideoMemorySetting);
    if (!setting || setting->isEmpty())
        return QStringLiteral("1610612736");
    if (*setting == QLatin1String("off"))
        return QString();
    return *setting;
}

bool useRobloxVideoMemory(const QString &profile, QString *error) {
    if (!editFlags(flagsPath(profile), [](FlagMap &flags) { flags.remove(kVideoMemoryFlag); }, error))
        return false;
    return editSetting(kVideoMemorySetting, QStringLiteral("off"), error);
}

// --- Import ----------------------------------------------------------------

QList<ImportItem> planImport(const FlagMap &current, const FlagMap &incoming, const QStringList &skipped) {
    QList<ImportItem> items;
    for (auto it = incoming.begin(); it != incoming.end(); ++it) {
        ImportItem item;
        item.name = it.key();
        if (current.contains(it.key()))
            item.current = current.value(it.key());
        item.problem = checkFlagName(it.key());
        if (item.problem.isEmpty())
            item.problem = checkFlagValue(it.key(), it.value(), &item.value);
        if (!item.problem.isEmpty())
            item.value = it.value();
        else if (item.current && sameFlagValue(item.name, *item.current, item.value))
            continue;
        items.append(item);
    }
    for (const QString &name : skipped) {
        ImportItem item;
        item.name = name;
        if (current.contains(name))
            item.current = current.value(name);
        item.problem = QStringLiteral("The value is not text, a number or true/false.");
        items.append(item);
    }
    return items;
}

void applyImport(FlagMap &flags, const QList<ImportItem> &items, const QSet<QString> &chosen) {
    for (const ImportItem &item : items) {
        if (item.problem.isEmpty() && chosen.contains(item.name))
            flags.insert(item.name, item.value);
    }
}

// --- Profiles --------------------------------------------------------------

bool isValidProfileName(const QString &name) {
    static const QRegularExpression re(QStringLiteral("^[A-Za-z0-9_-]{1,32}$"));
    return re.match(name).hasMatch();
}

bool profileInUse(const QString &dir) {
    const QByteArray lock = QFile::encodeName(dir + QStringLiteral("/.lock"));
    const int fd = ::open(lock.constData(), O_RDONLY | O_CLOEXEC);
    if (fd < 0)
        return false;
    bool busy = false;
    if (::flock(fd, LOCK_EX | LOCK_NB) != 0)
        busy = errno == EWOULDBLOCK;
    else
        ::flock(fd, LOCK_UN);
    ::close(fd);
    return busy;
}

bool profileSignedIn(const QString &dir) {
    // The client keeps the engine's cookies before anyone signs in, so the
    // jar alone is not a session; identity is written on sign-in and erased
    // on sign-out.
    return QFileInfo::exists(dir + QStringLiteral("/cookies")) && QFileInfo::exists(dir + QStringLiteral("/identity"));
}

bool processHasOpen(qint64 pid, const QString &file) {
    if (pid <= 0 || pid > INT_MAX)
        return false;
    struct stat st;
    if (::stat(QFile::encodeName(file).constData(), &st) != 0)
        return false;
    int count = 0;
    // Another user's process, or one that is gone, gives nothing.
    struct kinfo_file *files = ::kinfo_getfile(pid_t(pid), &count);
    if (!files)
        return false;
    bool found = false;
    for (int i = 0; i < count && !found; ++i) {
        const struct kinfo_file &f = files[i];
        found = f.kf_type == KF_TYPE_VNODE && f.kf_un.kf_file.kf_file_fsid == uint64_t(st.st_dev) &&
                f.kf_un.kf_file.kf_file_fileid == uint64_t(st.st_ino);
    }
    std::free(files);
    return found;
}

std::optional<qint64> clientPid(const QString &dir) {
    QFile file(dir + QLatin1Char('/') + kClientPidFile);
    if (!file.open(QIODevice::ReadOnly))
        return std::nullopt;
    const QByteArray text = file.read(32).trimmed();
    bool ok = false;
    const qint64 pid = text.toLongLong(&ok);
    if (!ok || pid <= 1)
        return std::nullopt;
    // Only while that very process holds the profile's lock file open: a file
    // a crashed client left, or a pid used again since, names nothing.
    if (!processHasOpen(pid, dir + QStringLiteral("/.lock")))
        return std::nullopt;
    return pid;
}

QList<RunningClient> runningClients() {
    SettingsConf conf;
    conf.load(settingsConfPath());
    QList<RunningClient> out;
    for (const ProfileInfo &p : listProfiles(defaultProfileName(conf))) {
        if (!p.inUse)
            continue;
        RunningClient c;
        c.profile = p.name;
        c.pid = clientPid(profileDir(p.name)).value_or(-1);
        out.append(c);
    }
    return out;
}

LaunchCommand launchCommand(const QString &profile, const QString &link) {
    LaunchCommand c;
    c.program = launcherPath();
    if (!link.isEmpty())
        c.arguments.append(link);
    c.environment = QProcessEnvironment::systemEnvironment();
    c.environment.insert(QStringLiteral("INTOXICATED_PROFILE"), profile);
    return c;
}

bool startClient(const QString &profile, const QString &link, QString *error) {
    if (!isValidProfileName(profile)) {
        *error = QStringLiteral("%1 is not a profile name.").arg(profile);
        return false;
    }
    if (profileInUse(profileDir(profile))) {
        *error = QStringLiteral("Profile %1 is already open in a client.").arg(profile);
        return false;
    }
    const LaunchCommand c = launchCommand(profile, link);
    if (!QFileInfo(c.program).isExecutable()) {
        *error = QStringLiteral("intoxicated was not found on PATH or in /usr/local/bin.");
        return false;
    }
    QProcess process;
    process.setProgram(c.program);
    process.setArguments(c.arguments);
    process.setProcessEnvironment(c.environment);
    process.setWorkingDirectory(QDir::homePath());
    process.setStandardInputFile(QProcess::nullDevice());
    if (!process.startDetached()) {
        *error = QStringLiteral("%1 could not be started: %2").arg(c.program, process.errorString());
        return false;
    }
    return true;
}

bool stopClient(const QString &profile, QString *error) {
    if (!isValidProfileName(profile)) {
        *error = QStringLiteral("%1 is not a profile name.").arg(profile);
        return false;
    }
    const std::optional<qint64> pid = clientPid(profileDir(profile));
    if (!pid) {
        *error = profileInUse(profileDir(profile))
                     ? QStringLiteral("The client on %1 has not said which process it is; close its window instead.")
                           .arg(profile)
                     : QStringLiteral("No client runs on profile %1.").arg(profile);
        return false;
    }
    // SIGTERM: the client leaves through its normal teardown, as on Ctrl-C.
    if (::kill(pid_t(*pid), SIGTERM) != 0) {
        *error = QStringLiteral("Process %1 could not be stopped: %2").arg(*pid).arg(QString::fromLocal8Bit(std::strerror(errno)));
        return false;
    }
    return true;
}

QList<ProfileInfo> listProfiles(const QString &extra) {
    QStringList names{kDefaultProfile};
    const QDir root(profilesRoot());
    const QStringList dirs = root.entryList(QDir::Dirs | QDir::NoDotAndDotDot | QDir::Hidden, QDir::Name);
    for (const QString &name : dirs) {
        if (isValidProfileName(name) && name != kDefaultProfile)
            names.append(name);
    }
    if (isValidProfileName(extra) && !names.contains(extra))
        names.append(extra);
    std::sort(names.begin() + 1, names.end());
    QList<ProfileInfo> list;
    for (const QString &name : names) {
        const QString dir = profileDir(name);
        ProfileInfo info;
        info.name = name;
        info.exists = QFileInfo(dir).isDir();
        info.signedIn = info.exists && profileSignedIn(dir);
        info.inUse = info.exists && profileInUse(dir);
        list.append(info);
    }
    return list;
}

bool createProfile(const QString &name, QString *error) {
    if (!isValidProfileName(name)) {
        *error = QStringLiteral("A profile name is 1 to 32 letters, digits, - and _.");
        return false;
    }
    const QString dir = profileDir(name);
    if (QFileInfo::exists(dir)) {
        *error = QStringLiteral("There is already a profile called %1.").arg(name);
        return false;
    }
    if (!QDir().mkpath(profilesRoot()) || !QDir().mkdir(dir)) {
        *error = QStringLiteral("%1 could not be created.").arg(dir);
        return false;
    }
    // Owner only, as the client makes it: the profile will hold a session.
    QFile::setPermissions(dir, QFileDevice::ReadOwner | QFileDevice::WriteOwner | QFileDevice::ExeOwner);
    return true;
}

bool deleteProfile(const QString &name, QString *error) {
    if (name == kDefaultProfile) {
        *error = QStringLiteral("The default profile cannot be deleted.");
        return false;
    }
    if (!isValidProfileName(name)) {
        *error = QStringLiteral("%1 is not a profile name.").arg(name);
        return false;
    }
    const QString dir = profileDir(name);
    if (!QFileInfo(dir).isDir()) {
        *error = QStringLiteral("There is no profile called %1.").arg(name);
        return false;
    }
    if (profileInUse(dir)) {
        *error = QStringLiteral("%1 is open in a client. Close it first.").arg(name);
        return false;
    }
    if (!QDir(dir).removeRecursively()) {
        *error = QStringLiteral("Some of %1 could not be removed.").arg(dir);
        return false;
    }
    return true;
}

QByteArray profileDocument(const QString &name, const FlagMap &flags) {
    QJsonObject f;
    for (auto it = flags.cbegin(); it != flags.cend(); ++it)
        f.insert(it.key(), it.value());
    QJsonObject root;
    root.insert(QStringLiteral("intoxicated-profile"), 1);
    root.insert(QStringLiteral("name"), name);
    root.insert(QStringLiteral("flags"), f);
    return QJsonDocument(root).toJson(QJsonDocument::Indented);
}

bool parseProfileDocument(const QByteArray &data, ProfileFile *out, QString *error) {
    QJsonParseError perr;
    const QJsonDocument doc = QJsonDocument::fromJson(data, &perr);
    if (perr.error != QJsonParseError::NoError || !doc.isObject()) {
        *error = QStringLiteral("This is not an Intoxicated profile file (not a JSON object).");
        return false;
    }
    const QJsonObject root = doc.object();
    if (root.value(QStringLiteral("intoxicated-profile")).toInt() != 1) {
        *error = QStringLiteral("This is not an Intoxicated profile file.");
        return false;
    }
    if (!root.value(QStringLiteral("flags")).isObject()) {
        *error = QStringLiteral("The profile file has no flags object.");
        return false;
    }
    ProfileFile result;
    result.name = root.value(QStringLiteral("name")).toString();
    if (!isValidProfileName(result.name))
        result.name = QStringLiteral("imported");
    const QJsonObject flags = root.value(QStringLiteral("flags")).toObject();
    for (auto it = flags.begin(); it != flags.end(); ++it) {
        QString value;
        if (it.value().isString())
            value = it.value().toString();
        else if (it.value().isBool())
            value = it.value().toBool() ? QStringLiteral("True") : QStringLiteral("False");
        else if (it.value().isDouble())
            value = QString::number(it.value().toDouble(), 'g', 17);
        else {
            result.skipped.append(it.key());
            continue;
        }
        QString normalised;
        if (!checkFlagName(it.key()).isEmpty() || !checkFlagValue(it.key(), value, &normalised).isEmpty()) {
            result.skipped.append(it.key());
            continue;
        }
        result.flags.insert(it.key(), normalised);
    }
    *out = result;
    return true;
}

QString freeProfileName(const QString &wanted) {
    const QString base = isValidProfileName(wanted) ? wanted : QStringLiteral("imported");
    if (!QFileInfo::exists(profileDir(base)))
        return base;
    for (int n = 2; n < 1000; ++n) {
        const QString suffix = QStringLiteral("-%1").arg(n);
        const QString candidate = base.left(32 - suffix.size()) + suffix;
        if (!QFileInfo::exists(profileDir(candidate)))
            return candidate;
    }
    return QString();
}

bool importProfile(const QString &name, const FlagMap &flags, QString *error) {
    if (!createProfile(name, error))
        return false;
    return saveFlags(flagsPath(name), flags, error);
}

QString defaultProfileName(const SettingsConf &conf) {
    const auto set = conf.value(QStringLiteral("INTOXICATED_PROFILE"));
    if (set && isValidProfileName(*set))
        return *set;
    return kDefaultProfile;
}

// --- Sound devices ---------------------------------------------------------

QList<SoundDevice> parseSndstat(const QString &text) {
    static const QRegularExpression re(QStringLiteral("^pcm([0-9]+): <(.*)> \\(([^)]*)\\)(.*)$"));
    QList<SoundDevice> devices;
    for (const QString &line : text.split(QLatin1Char('\n'))) {
        const auto m = re.match(line.trimmed());
        if (!m.hasMatch())
            continue;
        SoundDevice d;
        d.unit = m.captured(1).toInt();
        d.name = m.captured(2);
        const QString modes = m.captured(3);
        d.play = modes.contains(QLatin1String("play"));
        d.record = modes.contains(QLatin1String("rec"));
        d.isDefault = m.captured(4).split(QLatin1Char(' '), Qt::SkipEmptyParts).contains(QLatin1String("default"));
        devices.append(d);
    }
    return devices;
}

QList<SoundDevice> soundDevices() {
    QFile file(QStringLiteral("/dev/sndstat"));
    if (!file.open(QIODevice::ReadOnly))
        return {};
    return parseSndstat(QString::fromUtf8(file.readAll()));
}

// --- The launcher ----------------------------------------------------------

QString launcherPath() {
    const QString found = QStandardPaths::findExecutable(QStringLiteral("intoxicated"));
    return found.isEmpty() ? QStringLiteral("/usr/local/bin/intoxicated") : found;
}

std::optional<QByteArray> runLauncher(const QStringList &args, int timeoutMs, QString *error) {
    const QString program = launcherPath();
    if (!QFileInfo(program).isExecutable()) {
        *error = QStringLiteral("intoxicated was not found on PATH or in /usr/local/bin");
        return std::nullopt;
    }
    QProcess process;
    process.setProgram(program);
    process.setArguments(args);
    process.setStandardInputFile(QProcess::nullDevice());
    process.start();
    if (!process.waitForFinished(timeoutMs)) {
        process.kill();
        process.waitForFinished(1000);
        *error = QStringLiteral("intoxicated %1 did not finish").arg(args.join(QLatin1Char(' ')));
        return std::nullopt;
    }
    if (process.exitStatus() != QProcess::NormalExit || process.exitCode() != 0) {
        const QString said = QString::fromUtf8(process.readAllStandardError()).trimmed().section(QLatin1Char('\n'), 0, 0);
        *error = QStringLiteral("intoxicated %1 failed%2")
                     .arg(args.join(QLatin1Char(' ')), said.isEmpty() ? QString() : QStringLiteral(": ") + said);
        return std::nullopt;
    }
    return process.readAllStandardOutput();
}

std::optional<FlagMap> readDefaultFlags(QString *error) {
    const auto out = runLauncher({QStringLiteral("--default-flags")}, 10000, error);
    if (!out)
        return std::nullopt;
    FlagMap flags;
    QString parseError;
    if (!parseFlags(*out, &flags, &parseError)) {
        *error = QStringLiteral("intoxicated --default-flags printed something else: %1").arg(parseError);
        return std::nullopt;
    }
    return flags;
}

// --- Effects -----------------------------------------------------------------

const SsrKnob *SsrTable::knob(const QString &name) const {
    for (const SsrKnob &k : knobs) {
        if (k.name == name)
            return &k;
    }
    return nullptr;
}

const SsrLook *SsrTable::look(const QString &name) const {
    for (const SsrLook &l : looks) {
        if (l.name == name)
            return &l;
    }
    return nullptr;
}

namespace {

int ssrWidth(const SsrKnob &knob) { return knob.kind == SsrKind::Rgb ? 3 : 1; }

// A JSON value as a knob's value: a number, or three for an Rgb knob.
std::optional<SsrValue> ssrJsonValue(const SsrKnob &knob, const QJsonValue &v) {
    SsrValue out;
    if (knob.kind == SsrKind::Rgb) {
        const QJsonArray a = v.toArray();
        if (!v.isArray() || a.size() != 3)
            return std::nullopt;
        for (const QJsonValue &part : a) {
            if (!part.isDouble())
                return std::nullopt;
            out.append(part.toDouble());
        }
    } else {
        if (!v.isDouble())
            return std::nullopt;
        out.append(v.toDouble());
    }
    return out;
}

// One effect's object: its switch INTOXICATED_<NAME>, its look variable
// <switch>_LOOK, knobs whose variables start with <switch>_, and looks.
bool parseEffectObject(const QJsonObject &root, SsrTable *out, QString *error) {
    const auto fail = [&](const QString &why) {
        if (error)
            *error = why;
        return false;
    };
    static const QRegularExpression switchName(QStringLiteral("^INTOXICATED_[A-Z0-9]+$"));
    SsrTable table;
    table.switchEnv = root.value(QStringLiteral("switch")).toString();
    table.lookEnv = root.value(QStringLiteral("look_env")).toString();
    if (!switchName.match(table.switchEnv).hasMatch() || table.lookEnv != table.switchEnv + QStringLiteral("_LOOK"))
        return fail(QStringLiteral("it names no usable switch and look variables: %1 and %2")
                        .arg(table.switchEnv, table.lookEnv));
    for (const QJsonValue &v : root.value(QStringLiteral("knobs")).toArray()) {
        const QJsonObject o = v.toObject();
        SsrKnob k;
        k.name = o.value(QStringLiteral("name")).toString();
        k.env = o.value(QStringLiteral("env")).toString();
        const QString kind = o.value(QStringLiteral("kind")).toString();
        if (kind == QLatin1String("number"))
            k.kind = SsrKind::Number;
        else if (kind == QLatin1String("whole"))
            k.kind = SsrKind::Whole;
        else if (kind == QLatin1String("switch"))
            k.kind = SsrKind::Switch;
        else if (kind == QLatin1String("rgb"))
            k.kind = SsrKind::Rgb;
        else
            return fail(QStringLiteral("knob %1 has a kind this app does not know: %2").arg(k.name, kind));
        if (k.name.isEmpty() || !isSettingKey(k.env) || !k.env.startsWith(table.switchEnv + QLatin1Char('_')) ||
            k.env == table.lookEnv || table.knob(k.name))
            return fail(QStringLiteral("a knob without a usable name or variable: %1").arg(k.name));
        if (!o.value(QStringLiteral("min")).isDouble() || !o.value(QStringLiteral("max")).isDouble())
            return fail(QStringLiteral("knob %1 has no range").arg(k.name));
        k.min = o.value(QStringLiteral("min")).toDouble();
        k.max = o.value(QStringLiteral("max")).toDouble();
        const auto def = ssrJsonValue(k, o.value(QStringLiteral("default")));
        if (!(k.min < k.max) || !def)
            return fail(QStringLiteral("knob %1 has no usable range or default").arg(k.name));
        k.defaultValue = *def;
        table.knobs.append(k);
    }
    if (table.knobs.isEmpty())
        return fail(QStringLiteral("it lists no knobs"));

    static const QRegularExpression lookName(QStringLiteral("^[a-z0-9_]+$"));
    for (const QJsonValue &v : root.value(QStringLiteral("looks")).toArray()) {
        const QJsonObject o = v.toObject();
        SsrLook look;
        look.name = o.value(QStringLiteral("name")).toString();
        look.description = o.value(QStringLiteral("description")).toString();
        if (!lookName.match(look.name).hasMatch() || table.look(look.name))
            return fail(QStringLiteral("a look without a usable name: %1").arg(look.name));
        const QJsonObject values = o.value(QStringLiteral("values")).toObject();
        for (const SsrKnob &k : table.knobs) {
            const auto value = ssrJsonValue(k, values.value(k.name));
            if (!value)
                return fail(QStringLiteral("look %1 has no value for %2").arg(look.name, k.name));
            look.values.insert(k.name, *value);
        }
        table.looks.append(look);
    }
    table.defaultLook = root.value(QStringLiteral("default_look")).toString();
    if (!table.look(table.defaultLook))
        return fail(QStringLiteral("the default look %1 is not listed").arg(table.defaultLook));
    *out = table;
    return true;
}

} // namespace

bool parseSsrTable(const QByteArray &json, SsrTable *out, QString *error) {
    const auto fail = [&](const QString &why) {
        if (error)
            *error = why;
        return false;
    };
    QJsonParseError parseError;
    const QJsonDocument doc = QJsonDocument::fromJson(json, &parseError);
    if (doc.isNull())
        return fail(parseError.errorString());
    if (!doc.isObject())
        return fail(QStringLiteral("not a JSON object"));
    const QJsonObject root = doc.object();
    if (root.value(QStringLiteral("switch")).toString() != kSsrSetting ||
        root.value(QStringLiteral("look_env")).toString() != kSsrLookSetting)
        return fail(QStringLiteral("it names other settings than %1 and %2").arg(kSsrSetting, kSsrLookSetting));
    return parseEffectObject(root, out, error);
}

bool parseShaderTables(const QByteArray &json, QList<SsrTable> *out, QString *error) {
    const auto fail = [&](const QString &why) {
        if (error)
            *error = why;
        return false;
    };
    QJsonParseError parseError;
    const QJsonDocument doc = QJsonDocument::fromJson(json, &parseError);
    if (doc.isNull())
        return fail(parseError.errorString());
    if (!doc.isObject())
        return fail(QStringLiteral("not a JSON object"));
    static const QRegularExpression effectName(QStringLiteral("^[a-z0-9_]+$"));
    QList<SsrTable> tables;
    QSet<QString> names;
    QSet<QString> variables;
    for (const QJsonValue &v : doc.object().value(QStringLiteral("effects")).toArray()) {
        const QJsonObject o = v.toObject();
        SsrTable t;
        QString why;
        if (!parseEffectObject(o, &t, &why))
            return fail(QStringLiteral("effect %1: %2").arg(o.value(QStringLiteral("name")).toString(), why));
        t.name = o.value(QStringLiteral("name")).toString();
        t.title = o.value(QStringLiteral("title")).toString();
        t.summary = o.value(QStringLiteral("summary")).toString();
        if (!effectName.match(t.name).hasMatch() || names.contains(t.name) || t.title.isEmpty())
            return fail(QStringLiteral("an effect without a usable name or title: %1").arg(t.name));
        names.insert(t.name);
        // No variable may belong to two effects.
        QStringList own = {t.switchEnv, t.lookEnv};
        for (const SsrKnob &k : t.knobs)
            own.append(k.env);
        for (const QString &var : own) {
            if (variables.contains(var))
                return fail(QStringLiteral("%1 belongs to more than one effect").arg(var));
            variables.insert(var);
        }
        tables.append(t);
    }
    if (tables.isEmpty())
        return fail(QStringLiteral("it lists no effects"));
    *out = tables;
    return true;
}

bool effectsUnavailable(const QByteArray &json) {
    const QJsonDocument doc = QJsonDocument::fromJson(json);
    if (!doc.isObject())
        return false;
    const QJsonValue available = doc.object().value(QStringLiteral("available"));
    return available.isBool() && !available.toBool();
}

std::optional<QList<SsrTable>> readShaderTables(QString *error, QList<ShaderMode> *modes, bool *unavailable) {
    if (unavailable)
        *unavailable = false;
    const auto out = runLauncher({QStringLiteral("--shader-looks")}, 10000, error);
    if (!out)
        return std::nullopt;
    if (effectsUnavailable(*out)) {
        if (unavailable)
            *unavailable = true;
        *error = QStringLiteral("this Intoxicated was built without visual effects; %1 adds them")
                     .arg(kAddEffectsCommand);
        return std::nullopt;
    }
    QList<SsrTable> tables;
    QList<ShaderMode> parsedModes;
    QString parseError;
    if (!parseShaderTables(*out, &tables, &parseError) || !parseShaderModes(*out, tables, &parsedModes, &parseError)) {
        *error = QStringLiteral("intoxicated --shader-looks printed something else: %1").arg(parseError);
        return std::nullopt;
    }
    if (modes)
        *modes = parsedModes;
    return tables;
}

bool parseShaderModes(const QByteArray &json, const QList<SsrTable> &tables, QList<ShaderMode> *out, QString *error) {
    const auto fail = [&](const QString &why) {
        if (error)
            *error = why;
        return false;
    };
    const QJsonObject root = QJsonDocument::fromJson(json).object();
    QList<ShaderMode> modes;
    if (!root.contains(QStringLiteral("modes"))) {
        *out = modes;
        return true;
    }
    if (root.value(QStringLiteral("mode_env")).toString() != kShaderModeSetting)
        return fail(QStringLiteral("the modes name another variable than %1").arg(kShaderModeSetting));
    static const QRegularExpression modeName(QStringLiteral("^[a-z0-9_]+$"));
    for (const QJsonValue &v : root.value(QStringLiteral("modes")).toArray()) {
        const QJsonObject o = v.toObject();
        ShaderMode m;
        m.name = o.value(QStringLiteral("name")).toString();
        m.title = o.value(QStringLiteral("title")).toString();
        m.description = o.value(QStringLiteral("description")).toString();
        if (!modeName.match(m.name).hasMatch() || m.title.isEmpty() || findShaderMode(modes, m.name))
            return fail(QStringLiteral("a mode without a usable name or title: %1").arg(m.name));
        const QJsonObject effects = o.value(QStringLiteral("effects")).toObject();
        for (const SsrTable &t : tables) {
            const QJsonObject e = effects.value(t.name).toObject();
            const QString look = e.value(QStringLiteral("look")).toString();
            if (!e.value(QStringLiteral("on")).isBool() || !t.look(look))
                return fail(QStringLiteral("mode %1 has no usable switch and look for %2").arg(m.name, t.name));
            m.effects.insert(t.name, {e.value(QStringLiteral("on")).toBool(), look});
        }
        modes.append(m);
    }
    *out = modes;
    return true;
}

const ShaderMode *findShaderMode(const QList<ShaderMode> &modes, const QString &name) {
    for (const ShaderMode &m : modes) {
        if (m.name == name)
            return &m;
    }
    return nullptr;
}

bool expandShaderMode(const QList<SsrTable> &tables, const QList<ShaderMode> &modes, SettingsConf &conf) {
    const auto line = conf.value(kShaderModeSetting);
    if (!line)
        return false;
    // A name that is no mode the client ignores, so it is dropped as it is.
    if (const ShaderMode *m = findShaderMode(modes, line->trimmed())) {
        const auto unset = [&](const QString &key) { return conf.value(key).value_or(QString()).trimmed().isEmpty(); };
        for (const SsrTable &t : tables) {
            if (!m->effects.contains(t.name))
                continue;
            const auto [on, look] = m->effects.value(t.name);
            if (unset(t.switchEnv))
                setEffectOn(t.switchEnv, conf, on);
            if (unset(t.lookEnv)) {
                if (look == t.defaultLook)
                    conf.remove(t.lookEnv);
                else
                    conf.set(t.lookEnv, look);
            }
        }
    }
    conf.remove(kShaderModeSetting);
    return true;
}

QString currentShaderMode(const QList<SsrTable> &tables, const QList<ShaderMode> &modes, const SettingsConf &conf) {
    SettingsConf c = conf;
    expandShaderMode(tables, modes, c);
    for (const ShaderMode &m : modes) {
        bool all = true;
        for (const SsrTable &t : tables) {
            const auto [on, look] = m.effects.value(t.name, {false, t.defaultLook});
            const bool isOn = effectOn(t.switchEnv, c);
            QString unknown;
            if (isOn != on || (on && (ssrLook(t, c, &unknown) != look || !unknown.isEmpty() || !ssrOverrides(t, c).isEmpty()))) {
                all = false;
                break;
            }
        }
        if (all)
            return m.name;
    }
    return QString();
}

bool applyShaderMode(const QList<SsrTable> &tables, const QList<ShaderMode> &modes, SettingsConf &conf,
                     const QString &mode) {
    const ShaderMode *m = findShaderMode(modes, mode);
    if (!m)
        return false;
    conf.remove(kShaderModeSetting);
    for (const SsrTable &t : tables) {
        const auto [on, look] = m->effects.value(t.name, {false, t.defaultLook});
        setEffectOn(t.switchEnv, conf, on);
        applySsrLook(t, conf, look);
    }
    return true;
}

std::optional<SsrTable> readSsrTable(QString *error) {
    const auto out = runLauncher({QStringLiteral("--ssr-looks")}, 10000, error);
    if (!out)
        return std::nullopt;
    SsrTable table;
    QString parseError;
    if (!parseSsrTable(*out, &table, &parseError)) {
        *error = QStringLiteral("intoxicated --ssr-looks printed something else: %1").arg(parseError);
        return std::nullopt;
    }
    return table;
}

QStringList knobChoices(const QString &effect, const SsrKnob &knob) {
    struct Choices {
        const char *effect;
        const char *knob;
        QStringList titles;
    };
    static const Choices kChoices[] = {
        {"dof", "focus_mode",
         {QStringLiteral("Fixed distance"), QStringLiteral("Character"), QStringLiteral("Past the character")}},
    };
    for (const Choices &c : kChoices) {
        if (effect != QLatin1String(c.effect) || knob.name != QLatin1String(c.knob))
            continue;
        if (knob.kind != SsrKind::Whole || knob.min != 0 || knob.max != c.titles.size() - 1)
            return {};
        return c.titles;
    }
    return {};
}

QString ssrLookTitle(const QString &name) {
    QString title = name;
    title.replace(QLatin1Char('_'), QLatin1Char(' '));
    if (!title.isEmpty())
        title[0] = title[0].toUpper();
    return title;
}

std::optional<SsrValue> parseSsrValue(const SsrKnob &knob, const QString &text) {
    const QString t = text.trimmed();
    // As the client reads a number: an f32, inside the range as an f32.
    const auto number = [&](const QString &part) -> std::optional<double> {
        bool ok = false;
        const double d = QLocale::c().toDouble(part.trimmed(), &ok);
        const float f = float(d);
        if (!ok || !(f >= float(knob.min) && f <= float(knob.max)))
            return std::nullopt;
        return d;
    };
    switch (knob.kind) {
    case SsrKind::Number:
        if (const auto v = number(t))
            return SsrValue{*v};
        return std::nullopt;
    case SsrKind::Whole: {
        static const QRegularExpression digits(QStringLiteral("^\\+?[0-9]{1,10}$"));
        if (!digits.match(t).hasMatch())
            return std::nullopt;
        const qulonglong n = t.toULongLong();
        if (double(n) < knob.min || double(n) > knob.max)
            return std::nullopt;
        return SsrValue{double(n)};
    }
    case SsrKind::Switch:
        if (t == QLatin1String("1"))
            return SsrValue{1.0};
        if (t == QLatin1String("0"))
            return SsrValue{0.0};
        return std::nullopt;
    case SsrKind::Rgb: {
        const QStringList parts = t.split(QLatin1Char(','));
        if (parts.size() != 1 && parts.size() != 3)
            return std::nullopt;
        SsrValue out;
        for (const QString &part : parts) {
            const auto v = number(part);
            if (!v)
                return std::nullopt;
            out.append(*v);
        }
        if (out.size() == 1)
            out = {out[0], out[0], out[0]};
        return out;
    }
    }
    return std::nullopt;
}

QString shortestFloat(float f) {
    // Fixed point, with as few decimals as read back as the same float: 0.4
    // for 0.4f, where the double it widens to would print 0.4000000059604645.
    for (int decimals = 0; decimals <= 9; ++decimals) {
        const QString text = QString::number(double(f), 'f', decimals);
        if (float(text.toDouble()) == f)
            return text;
    }
    return QString::number(double(f), 'g', 9);
}

QString formatSsrValue(const SsrKnob &knob, const SsrValue &value) {
    QStringList parts;
    for (int i = 0; i < ssrWidth(knob) && i < value.size(); ++i) {
        // The shortest text that reads back as the same f32, as the client
        // writes its own values.
        if (knob.kind == SsrKind::Whole || knob.kind == SsrKind::Switch)
            parts.append(QString::number(qRound64(value[i])));
        else
            parts.append(shortestFloat(float(value[i])));
    }
    return parts.join(QLatin1Char(','));
}

bool effectOn(const QString &switchEnv, const SettingsConf &conf) {
    const auto v = conf.value(switchEnv);
    return v && v->trimmed() == QLatin1String("1");
}

void setEffectOn(const QString &switchEnv, SettingsConf &conf, bool on) {
    if (on)
        conf.set(switchEnv, QStringLiteral("1"));
    else
        conf.remove(switchEnv);
}

bool ssrOn(const SettingsConf &conf) { return effectOn(kSsrSetting, conf); }

void setSsrOn(SettingsConf &conf, bool on) { setEffectOn(kSsrSetting, conf, on); }

QString ssrLook(const SsrTable &table, const SettingsConf &conf, QString *unknown) {
    const QString name = conf.value(table.lookEnv).value_or(QString()).trimmed();
    if (unknown)
        unknown->clear();
    if (name.isEmpty())
        return table.defaultLook;
    if (table.look(name))
        return name;
    if (unknown)
        *unknown = name;
    return table.defaultLook;
}

std::optional<QString> ssrOverride(const SettingsConf &conf, const SsrKnob &knob) {
    const auto v = conf.value(knob.env);
    if (!v || v->trimmed().isEmpty())
        return std::nullopt;
    return v;
}

QStringList ssrOverrides(const SsrTable &table, const SettingsConf &conf) {
    QStringList out;
    for (const SsrKnob &k : table.knobs) {
        if (ssrOverride(conf, k))
            out.append(k.name);
    }
    return out;
}

SsrValue effectiveSsrValue(const SsrTable &table, const SettingsConf &conf, const SsrKnob &knob) {
    if (const auto text = ssrOverride(conf, knob)) {
        if (const auto v = parseSsrValue(knob, *text))
            return *v;
    }
    const SsrLook *look = table.look(ssrLook(table, conf));
    return look ? look->values.value(knob.name, knob.defaultValue) : knob.defaultValue;
}

bool applySsrLook(const SsrTable &table, SettingsConf &conf, const QString &look) {
    if (!table.look(look))
        return false;
    if (look == table.defaultLook)
        conf.remove(table.lookEnv);
    else
        conf.set(table.lookEnv, look);
    for (const SsrKnob &k : table.knobs)
        conf.remove(k.env);
    return true;
}

bool setSsrKnob(SettingsConf &conf, const SsrKnob &knob, const SsrValue &value) {
    if (value.size() != ssrWidth(knob))
        return false;
    const QString text = formatSsrValue(knob, value);
    if (!parseSsrValue(knob, text))
        return false;
    return conf.set(knob.env, text);
}

void resetSsrKnob(SettingsConf &conf, const SsrKnob &knob) { conf.remove(knob.env); }

// --- Macros ----------------------------------------------------------------

QString macrosPath(const QString &profile) { return profileDir(profile) + QStringLiteral("/macros.json"); }

namespace {

// The step kinds as the file names them, and the client reads them.
struct KindName {
    StepKind kind;
    const char *name;
};
const KindName kKindNames[] = {
    {StepKind::Tap, "tap"},         {StepKind::KeyDown, "keydown"},       {StepKind::KeyUp, "keyup"},
    {StepKind::Move, "move"},       {StepKind::Click, "click"},           {StepKind::ButtonDown, "buttondown"},
    {StepKind::ButtonUp, "buttonup"}, {StepKind::Scroll, "scroll"},       {StepKind::Text, "text"},
    {StepKind::Wait, "wait"},
};

QString kindName(StepKind kind) {
    for (const KindName &k : kKindNames)
        if (k.kind == kind)
            return QString::fromLatin1(k.name);
    return QStringLiteral("tap");
}

std::optional<StepKind> kindFromName(const QString &name) {
    for (const KindName &k : kKindNames)
        if (name == QLatin1String(k.name))
            return k.kind;
    return std::nullopt;
}

bool isKeyStep(StepKind k) { return k == StepKind::Tap || k == StepKind::KeyDown || k == StepKind::KeyUp; }
bool isButtonStep(StepKind k) {
    return k == StepKind::Click || k == StepKind::ButtonDown || k == StepKind::ButtonUp;
}
bool isPointStep(StepKind k) { return isButtonStep(k) || k == StepKind::Move || k == StepKind::Scroll; }

} // namespace

const QList<KeyName> &keyNames() {
    // A fixed, curated set: the keys a game macro usually wants, each with its
    // Linux evdev code. The file stores the code; this names it both ways.
    static const QList<KeyName> table = [] {
        QList<KeyName> t;
        const auto add = [&](int evdev, const char *label) { t.append({evdev, QString::fromLatin1(label)}); };
        add(30, "A"); add(48, "B"); add(46, "C"); add(32, "D"); add(18, "E"); add(33, "F");
        add(34, "G"); add(35, "H"); add(23, "I"); add(36, "J"); add(37, "K"); add(38, "L");
        add(50, "M"); add(49, "N"); add(24, "O"); add(25, "P"); add(16, "Q"); add(19, "R");
        add(31, "S"); add(20, "T"); add(22, "U"); add(47, "V"); add(17, "W"); add(45, "X");
        add(21, "Y"); add(44, "Z");
        add(2, "1"); add(3, "2"); add(4, "3"); add(5, "4"); add(6, "5"); add(7, "6");
        add(8, "7"); add(9, "8"); add(10, "9"); add(11, "0");
        add(57, "Space"); add(28, "Enter"); add(15, "Tab"); add(1, "Escape"); add(14, "Backspace");
        add(58, "Caps Lock"); add(29, "Left Ctrl"); add(97, "Right Ctrl"); add(42, "Left Shift");
        add(54, "Right Shift"); add(56, "Left Alt"); add(100, "Right Alt");
        add(103, "Up"); add(108, "Down"); add(105, "Left"); add(106, "Right");
        add(110, "Insert"); add(111, "Delete"); add(102, "Home"); add(107, "End");
        add(104, "Page Up"); add(109, "Page Down");
        add(12, "Minus"); add(13, "Equals"); add(26, "Left Bracket"); add(27, "Right Bracket");
        add(39, "Semicolon"); add(40, "Quote"); add(41, "Backtick"); add(43, "Backslash");
        add(51, "Comma"); add(52, "Period"); add(53, "Slash");
        add(59, "F1"); add(60, "F2"); add(61, "F3"); add(62, "F4"); add(63, "F5"); add(64, "F6");
        add(65, "F7"); add(66, "F8"); add(67, "F9"); add(68, "F10"); add(87, "F11"); add(88, "F12");
        return t;
    }();
    return table;
}

QString keyLabel(int evdev) {
    for (const KeyName &k : keyNames())
        if (k.evdev == evdev)
            return k.label;
    return QStringLiteral("Key %1").arg(evdev);
}

int keyEvdev(const QString &label) {
    for (const KeyName &k : keyNames())
        if (k.label == label)
            return k.evdev;
    return -1;
}

QString checkMacroName(const QString &name) {
    if (name.trimmed().isEmpty())
        return QStringLiteral("A macro needs a name.");
    return QString();
}

QString checkMacro(const MacroDef &m) {
    if (const QString why = checkMacroName(m.name); !why.isEmpty())
        return why;
    if (m.triggerEvdev < 1 || m.triggerEvdev > 255)
        return QStringLiteral("Pick a trigger key.");
    if (m.steps.isEmpty())
        return QStringLiteral("A macro needs at least one step.");
    for (int i = 0; i < m.steps.size(); ++i) {
        const MacroStep &s = m.steps[i];
        if (isKeyStep(s.kind) && (s.evdev < 1 || s.evdev > 255))
            return QStringLiteral("Step %1 has no key.").arg(i + 1);
        if (s.kind == StepKind::Text && s.text.isEmpty())
            return QStringLiteral("Step %1 has no text.").arg(i + 1);
    }
    return QString();
}

QString describeStep(const MacroStep &step) {
    const auto mods = [&] {
        QStringList m;
        if (step.ctrl)
            m << QStringLiteral("Ctrl");
        if (step.shift)
            m << QStringLiteral("Shift");
        if (step.alt)
            m << QStringLiteral("Alt");
        m << keyLabel(step.evdev);
        return m.join(QStringLiteral(" + "));
    };
    const auto button = [&] {
        switch (step.button) {
        case 2:
            return QStringLiteral("right");
        case 3:
            return QStringLiteral("middle");
        default:
            return QStringLiteral("left");
        }
    };
    const QString xy = QStringLiteral("%1, %2").arg(step.x).arg(step.y);
    switch (step.kind) {
    case StepKind::Tap:
        return QStringLiteral("Tap  %1").arg(mods());
    case StepKind::KeyDown:
        return QStringLiteral("Hold  %1").arg(mods());
    case StepKind::KeyUp:
        return QStringLiteral("Release  %1").arg(mods());
    case StepKind::Move:
        return QStringLiteral("Move  %1").arg(xy);
    case StepKind::Click:
        return QStringLiteral("Click %1  at %2").arg(button(), xy);
    case StepKind::ButtonDown:
        return QStringLiteral("Press %1  at %2").arg(button(), xy);
    case StepKind::ButtonUp:
        return QStringLiteral("Let go %1  at %2").arg(button(), xy);
    case StepKind::Scroll:
        return QStringLiteral("Scroll  %1 at %2").arg(step.detents).arg(xy);
    case StepKind::Text:
        return QStringLiteral("Type  \"%1\"").arg(step.text);
    case StepKind::Wait:
        return QStringLiteral("Wait  %1 ms").arg(step.ms);
    }
    return QString();
}

namespace {

std::optional<MacroStep> parseStep(const QJsonObject &o, QString *why) {
    const auto fail = [&](const QString &m) -> std::optional<MacroStep> {
        if (why)
            *why = m;
        return std::nullopt;
    };
    const std::optional<StepKind> kind = kindFromName(o.value(QStringLiteral("type")).toString());
    if (!kind)
        return fail(QStringLiteral("unknown step type %1").arg(o.value(QStringLiteral("type")).toString()));
    MacroStep s;
    s.kind = *kind;
    if (isKeyStep(s.kind)) {
        if (!o.value(QStringLiteral("key")).isDouble())
            return fail(QStringLiteral("a key step needs a \"key\" code"));
        s.evdev = o.value(QStringLiteral("key")).toInt();
        if (s.evdev < 1 || s.evdev > 255)
            return fail(QStringLiteral("key %1 is not an evdev code 1..255").arg(s.evdev));
        s.ctrl = o.value(QStringLiteral("ctrl")).toBool();
        s.shift = o.value(QStringLiteral("shift")).toBool();
        s.alt = o.value(QStringLiteral("alt")).toBool();
    }
    if (isPointStep(s.kind)) {
        if (!o.value(QStringLiteral("x")).isDouble() || !o.value(QStringLiteral("y")).isDouble())
            return fail(QStringLiteral("this step needs \"x\" and \"y\""));
        s.x = o.value(QStringLiteral("x")).toDouble();
        s.y = o.value(QStringLiteral("y")).toDouble();
    }
    if (isButtonStep(s.kind))
        s.button = o.contains(QStringLiteral("button")) ? o.value(QStringLiteral("button")).toInt() : 1;
    if (s.kind == StepKind::Scroll) {
        if (!o.value(QStringLiteral("detents")).isDouble())
            return fail(QStringLiteral("a scroll needs \"detents\""));
        s.detents = o.value(QStringLiteral("detents")).toDouble();
    }
    if (s.kind == StepKind::Text) {
        s.text = o.value(QStringLiteral("text")).toString();
        if (s.text.isEmpty())
            return fail(QStringLiteral("empty text"));
    }
    if (s.kind == StepKind::Wait) {
        if (!o.value(QStringLiteral("ms")).isDouble())
            return fail(QStringLiteral("a wait needs \"ms\""));
        const double ms = o.value(QStringLiteral("ms")).toDouble();
        s.ms = quint64(qBound(0.0, ms, 3600000.0));
    }
    return s;
}

std::optional<MacroDef> parseMacro(const QJsonValue &value, QString *why) {
    const auto fail = [&](const QString &m) -> std::optional<MacroDef> {
        if (why)
            *why = m;
        return std::nullopt;
    };
    if (!value.isObject())
        return fail(QStringLiteral("each macro must be a JSON object"));
    const QJsonObject o = value.toObject();
    MacroDef m;
    m.name = o.value(QStringLiteral("name")).toString().trimmed();
    if (m.name.isEmpty())
        return fail(QStringLiteral("a macro needs a name"));
    const QJsonValue trig = o.value(QStringLiteral("trigger"));
    if (!trig.isObject())
        return fail(QStringLiteral("the trigger must be a JSON object"));
    const QJsonObject to = trig.toObject();
    if (!to.value(QStringLiteral("key")).isDouble())
        return fail(QStringLiteral("the trigger needs a \"key\" code"));
    m.triggerEvdev = to.value(QStringLiteral("key")).toInt();
    if (m.triggerEvdev < 1 || m.triggerEvdev > 255)
        return fail(QStringLiteral("trigger key %1 is not an evdev code 1..255").arg(m.triggerEvdev));
    m.triggerCtrl = to.value(QStringLiteral("ctrl")).toBool();
    m.triggerShift = to.value(QStringLiteral("shift")).toBool();
    m.triggerAlt = to.value(QStringLiteral("alt")).toBool();
    m.enabled = o.contains(QStringLiteral("enabled")) ? o.value(QStringLiteral("enabled")).toBool() : true;
    m.swallow = o.value(QStringLiteral("swallow")).toBool();
    m.repeat = o.value(QStringLiteral("repeat")).toBool();
    const QJsonValue steps = o.value(QStringLiteral("steps"));
    if (!steps.isArray())
        return fail(QStringLiteral("a macro needs a \"steps\" array"));
    const QJsonArray arr = steps.toArray();
    for (int i = 0; i < arr.size(); ++i) {
        QString stepWhy;
        const auto step = parseStep(arr[i].toObject(), &stepWhy);
        if (!step)
            return fail(QStringLiteral("step %1: %2").arg(i + 1).arg(stepWhy));
        m.steps.append(*step);
    }
    if (m.steps.isEmpty())
        return fail(QStringLiteral("a macro needs at least one step"));
    return m;
}

QJsonObject stepToJson(const MacroStep &s) {
    QJsonObject o;
    o.insert(QStringLiteral("type"), kindName(s.kind));
    if (isKeyStep(s.kind)) {
        o.insert(QStringLiteral("key"), s.evdev);
        if (s.ctrl)
            o.insert(QStringLiteral("ctrl"), true);
        if (s.shift)
            o.insert(QStringLiteral("shift"), true);
        if (s.alt)
            o.insert(QStringLiteral("alt"), true);
    }
    if (isPointStep(s.kind)) {
        o.insert(QStringLiteral("x"), s.x);
        o.insert(QStringLiteral("y"), s.y);
    }
    if (isButtonStep(s.kind) && s.button != 1)
        o.insert(QStringLiteral("button"), s.button);
    if (s.kind == StepKind::Scroll)
        o.insert(QStringLiteral("detents"), s.detents);
    if (s.kind == StepKind::Text)
        o.insert(QStringLiteral("text"), s.text);
    if (s.kind == StepKind::Wait)
        o.insert(QStringLiteral("ms"), double(s.ms));
    return o;
}

} // namespace

bool parseMacros(const QByteArray &data, QList<MacroDef> *out, QString *error, QStringList *skipped) {
    out->clear();
    if (skipped)
        skipped->clear();
    if (data.trimmed().isEmpty())
        return true;
    QJsonParseError perr{};
    const QJsonDocument doc = QJsonDocument::fromJson(data, &perr);
    if (perr.error != QJsonParseError::NoError) {
        if (error)
            *error = QStringLiteral("not valid JSON: %1").arg(perr.errorString());
        return false;
    }
    if (!doc.isObject()) {
        if (error)
            *error = QStringLiteral("the document must be a JSON object");
        return false;
    }
    const QJsonValue list = doc.object().value(QStringLiteral("macros"));
    if (list.isUndefined() || list.isNull())
        return true;
    if (!list.isArray()) {
        if (error)
            *error = QStringLiteral("\"macros\" must be an array");
        return false;
    }
    const QJsonArray arr = list.toArray();
    for (int i = 0; i < arr.size(); ++i) {
        QString why;
        const auto m = parseMacro(arr[i], &why);
        if (m)
            out->append(*m);
        else if (skipped)
            skipped->append(QStringLiteral("macro %1: %2").arg(i + 1).arg(why));
    }
    return true;
}

MacrosFile loadMacros(const QString &path) {
    MacrosFile file;
    QFile f(path);
    if (!f.exists())
        return file;
    file.exists = true;
    if (!f.open(QIODevice::ReadOnly)) {
        file.error = QStringLiteral("%1: %2").arg(path, f.errorString());
        return file;
    }
    if (!parseMacros(f.readAll(), &file.macros, &file.error, &file.skipped))
        file.macros.clear();
    return file;
}

bool saveMacros(const QString &path, const QList<MacroDef> &macros, QString *error) {
    QJsonArray arr;
    for (const MacroDef &m : macros) {
        QJsonObject o;
        o.insert(QStringLiteral("name"), m.name);
        o.insert(QStringLiteral("enabled"), m.enabled);
        QJsonObject trig;
        trig.insert(QStringLiteral("key"), m.triggerEvdev);
        if (m.triggerCtrl)
            trig.insert(QStringLiteral("ctrl"), true);
        if (m.triggerShift)
            trig.insert(QStringLiteral("shift"), true);
        if (m.triggerAlt)
            trig.insert(QStringLiteral("alt"), true);
        o.insert(QStringLiteral("trigger"), trig);
        if (m.swallow)
            o.insert(QStringLiteral("swallow"), true);
        if (m.repeat)
            o.insert(QStringLiteral("repeat"), true);
        QJsonArray steps;
        for (const MacroStep &s : m.steps)
            steps.append(stepToJson(s));
        o.insert(QStringLiteral("steps"), steps);
        arr.append(o);
    }
    QJsonObject root;
    root.insert(QStringLiteral("intoxicated-macros"), 1);
    root.insert(QStringLiteral("macros"), arr);
    return writeAtomically(path, QJsonDocument(root).toJson(QJsonDocument::Indented), error);
}

} // namespace intox
