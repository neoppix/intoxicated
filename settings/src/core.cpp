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
#include <sys/file.h>
#include <unistd.h>

#include <algorithm>
#include <cerrno>
#include <climits>

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
    return QFileInfo::exists(dir + QStringLiteral("/cookies"));
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

std::optional<QList<SsrTable>> readShaderTables(QString *error) {
    const auto out = runLauncher({QStringLiteral("--shader-looks")}, 10000, error);
    if (!out)
        return std::nullopt;
    QList<SsrTable> tables;
    QString parseError;
    if (!parseShaderTables(*out, &tables, &parseError)) {
        *error = QStringLiteral("intoxicated --shader-looks printed something else: %1").arg(parseError);
        return std::nullopt;
    }
    return tables;
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

bool rtOn(const SettingsConf &conf) { return effectOn(kRtSetting, conf); }

void setRtOn(SettingsConf &conf, bool on) { setEffectOn(kRtSetting, conf, on); }

QStringList rtWarnings() {
    return {
        QStringLiteral("While on it replaces the screen-space reflections; their look and fine tuning still apply."),
        QStringLiteral("Off-screen hits show textures only where the game samples them plainly (clothing); other "
                       "textured walls and parts reflect in flat colours."),
        QStringLiteral("Interface drawn in the world, such as leaderboards and name tags, does not reflect."),
        QStringLiteral("Reflections can flicker while the camera moves."),
        QStringLiteral("Objects the game has never drawn cannot appear in a reflection."),
        QStringLiteral("Uses about 400 MB more video memory and about 1 ms more GPU time a frame."),
        QStringLiteral("Runs as compute on the shader cores, because NVIDIA's FreeBSD driver has no hardware ray "
                       "tracing."),
        QStringLiteral("May crash or misbehave on other GPUs and drivers: only tested on an RTX 4070 Ti with NVIDIA "
                       "620.70.06."),
    };
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

} // namespace intox
