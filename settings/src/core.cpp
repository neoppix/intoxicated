// Intoxicated Settings: settings.conf, flags.json, profiles, sound devices
// and the launcher. See core.h.

#include "core.h"

#include <QDateTime>
#include <QDir>
#include <QFile>
#include <QFileInfo>
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

} // namespace intox
