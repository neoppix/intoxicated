// Intoxicated Settings: the Client page. Each control writes one key of
// settings.conf as it changes; a control at its default removes the key.

#include "app.h"
#include "style.h"

#include <QGroupBox>
#include <QGuiApplication>
#include <QHBoxLayout>
#include <QInputDialog>
#include <QLabel>
#include <QLineEdit>
#include <QLocale>
#include <QMessageBox>
#include <QPushButton>
#include <QRegularExpression>
#include <QScreen>
#include <QVBoxLayout>

namespace intox {

namespace {

const QString kFullscreen = QStringLiteral("INTOXICATED_FULLSCREEN");
const QString kMonitor = QStringLiteral("INTOXICATED_MONITOR");
const QString kResolution = QStringLiteral("INTOXICATED_RESOLUTION");
const QString kScale = QStringLiteral("INTOXICATED_DPI_SCALE");
const QString kCursor = QStringLiteral("INTOXICATED_SHOW_CURSOR");
const QString kWheel = QStringLiteral("INTOXICATED_WHEEL_SCALE");
const QString kNoDragLock = QStringLiteral("INTOXICATED_NO_DRAG_LOCK");
const QString kPresent = QStringLiteral("INTOXICATED_PRESENT_MODE");
const QString kNoVulkan = QStringLiteral("INTOXICATED_NO_VULKAN");
const QString kAudio = QStringLiteral("INTOXICATED_AUDIO_DEVICE");
const QString kCloseOnLeave = QStringLiteral("INTOXICATED_CLOSE_ON_LEAVE");

// Select the item holding VALUE, adding one labelled LABEL when none does.
void selectValue(QComboBox *combo, const QString &value, const QString &label) {
    int at = combo->findData(value);
    if (at < 0) {
        combo->addItem(label, value);
        at = combo->count() - 1;
    }
    combo->setCurrentIndex(at);
}

// A switch's state from a key that is on when present and not empty.
bool isOn(const SettingsConf &conf, const QString &key) {
    const auto v = conf.value(key);
    return v && !v->isEmpty();
}

QWidget *group(QVBoxLayout *page, const QString &title, QVBoxLayout **rows) {
    Section s = section(title);
    auto *v = new QVBoxLayout(s.body);
    v->setContentsMargins(0, 0, 0, 0);
    v->setSpacing(0);
    *rows = v;
    page->addWidget(s.box);
    page->addSpacing(8);
    return s.box;
}

QString trimmedNumber(double v) {
    return QString::number(v, 'g', QLocale::FloatingPointShortest);
}

} // namespace

ClientPage::ClientPage(AppState *state) : m_state(state) {
    QVBoxLayout *b = pageLayout(this, QStringLiteral("Client"),
                                QStringLiteral("Options the launcher hands the client, kept in settings.conf. They "
                                               "apply to every profile from the next launch."));
    QVBoxLayout *rows = nullptr;

    group(b, QStringLiteral("Window"), &rows);
    m_fullscreen = new Switch;
    rows->addWidget(settingRow(QStringLiteral("Fullscreen"),
                               QStringLiteral("Cover the chosen monitor. F11 switches while playing."), m_fullscreen));
    rows->addWidget(separator());
    m_monitor = new Combo;
    rows->addWidget(settingRow(QStringLiteral("Monitor"), QStringLiteral("Where the window opens"), m_monitor));
    rows->addWidget(separator());
    m_resolution = new Combo;
    rows->addWidget(settingRow(QStringLiteral("Resolution"),
                               QStringLiteral("The window's size; fullscreen uses the monitor's"), m_resolution));
    rows->addWidget(separator());
    m_scale = new Combo;
    rows->addWidget(settingRow(QStringLiteral("UI scale"),
                               QStringLiteral("How large Roblox draws its menus and text"), m_scale));
    rows->addWidget(separator());
    m_cursor = new Switch;
    rows->addWidget(settingRow(QStringLiteral("Show system cursor"),
                               QStringLiteral("Keep the desktop's cursor over the window as well as Roblox's"),
                               m_cursor));

    group(b, QStringLiteral("Input"), &rows);
    m_wheel = new Combo;
    rows->addWidget(settingRow(QStringLiteral("Mouse wheel speed"), QStringLiteral("Scroll steps per wheel notch"),
                               m_wheel));
    rows->addWidget(separator());
    m_invert = new Switch;
    rows->addWidget(settingRow(QStringLiteral("Invert"), QStringLiteral("Scroll the other way"), m_invert));
    rows->addWidget(separator());
    m_noDragLock = new Switch;
    rows->addWidget(settingRow(QStringLiteral("Capture the mouse only when the game asks"),
                               QStringLiteral("Not while the right or middle button is held"), m_noDragLock));

    group(b, QStringLiteral("Graphics"), &rows);
    m_present = new Combo;
    rows->addWidget(settingRow(QStringLiteral("Present mode"),
                               QStringLiteral("How frames reach the screen. Automatic suits most setups."), m_present));
    rows->addWidget(separator());
    m_renderer = new Combo;
    rows->addWidget(settingRow(QStringLiteral("Renderer"),
                               QStringLiteral("OpenGL ES is the fallback for a GPU without Vulkan"), m_renderer));

    group(b, QStringLiteral("Audio"), &rows);
    m_audio = new Combo;
    rows->addWidget(settingRow(QStringLiteral("Output device"),
                               QStringLiteral("The system default follows hw.snd.default_unit"), m_audio));

    group(b, QStringLiteral("Session"), &rows);
    m_closeOnLeave = new Switch;
    rows->addWidget(settingRow(QStringLiteral("Close Intoxicated when leaving a game"),
                               QStringLiteral("Instead of going back to the home page"), m_closeOnLeave));

    auto *resetRow = new QHBoxLayout;
    resetRow->addStretch(1);
    auto *reset = new QPushButton(QStringLiteral("Reset client options"));
    setButtonIcon(reset, QStringLiteral("arrow-counter-clockwise"));
    reset->setCursor(Qt::PointingHandCursor);
    reset->setFocusPolicy(Qt::NoFocus);
    resetRow->addWidget(reset);
    b->addLayout(resetRow);
    b->addStretch(1);

    // Switches: present (as =1) or absent.
    const QList<QPair<Switch *, QString>> switches = {
        {m_fullscreen, kFullscreen}, {m_cursor, kCursor}, {m_noDragLock, kNoDragLock}, {m_closeOnLeave, kCloseOnLeave}};
    for (const auto &[sw, key] : switches) {
        const QString k = key;
        connect(sw, &QCheckBox::clicked, this, [this, k](bool on) {
            write(k, on ? std::optional<QString>(QStringLiteral("1")) : std::nullopt);
        });
    }
    // Combos whose item data is the value; an empty value removes the key.
    const QList<QPair<Combo *, QString>> combos = {
        {m_monitor, kMonitor}, {m_scale, kScale}, {m_present, kPresent}, {m_renderer, kNoVulkan}, {m_audio, kAudio}};
    for (const auto &[combo, key] : combos) {
        const QString k = key;
        Combo *c = combo;
        connect(c, &QComboBox::activated, this, [this, c, k](int index) {
            const QString value = c->itemData(index).toString();
            write(k, value.isEmpty() ? std::nullopt : std::optional<QString>(value));
        });
    }
    connect(m_resolution, &QComboBox::activated, this, &ClientPage::resolutionChosen);
    connect(m_wheel, &QComboBox::activated, this, &ClientPage::writeWheel);
    connect(m_invert, &QCheckBox::clicked, this, &ClientPage::writeWheel);
    connect(reset, &QPushButton::clicked, this, &ClientPage::reset);
    connect(state, &AppState::settingsChanged, this, &ClientPage::reload);
}

void ClientPage::showEvent(QShowEvent *event) {
    QWidget::showEvent(event);
    reload();
}

void ClientPage::reload() {
    m_filling = true;
    SettingsConf conf;
    QString error;
    if (!conf.load(settingsConfPath(), &error))
        QMessageBox::warning(this, QStringLiteral("Client"), error);
    const auto get = [&](const QString &key) { return conf.value(key).value_or(QString()); };

    m_fullscreen->setChecked(isOn(conf, kFullscreen));
    m_cursor->setChecked(isOn(conf, kCursor));
    m_noDragLock->setChecked(isOn(conf, kNoDragLock));
    m_closeOnLeave->setChecked(isOn(conf, kCloseOnLeave));

    // Qt lists the primary screen first, as Xinerama does. INFERRED that the
    // rest come in the same order; true for one monitor.
    m_monitor->clear();
    m_monitor->addItem(QStringLiteral("Top left of the desktop"), QString());
    const QList<QScreen *> screens = QGuiApplication::screens();
    for (int i = 0; i < screens.size(); ++i) {
        const QRect g = screens[i]->geometry();
        const QSize px = g.size() * screens[i]->devicePixelRatio();
        m_monitor->addItem(QStringLiteral("%1: %2, %3x%4").arg(i).arg(screens[i]->name()).arg(px.width()).arg(px.height()),
                           QString::number(i));
    }
    selectValue(m_monitor, get(kMonitor), QStringLiteral("%1: not connected").arg(get(kMonitor)));

    m_resolution->clear();
    m_resolution->addItem(QStringLiteral("1280x720 (default)"), QString());
    for (const char *size : {"1600x900", "1920x1080", "2560x1440", "3440x1440", "3840x2160"})
        m_resolution->addItem(QString::fromLatin1(size), QString::fromLatin1(size));
    selectValue(m_resolution, get(kResolution), get(kResolution));
    m_resolution->addItem(QStringLiteral("Custom..."), QStringLiteral("custom"));

    m_scale->clear();
    m_scale->addItem(QStringLiteral("1.0 (default)"), QString());
    for (double s = 1.25; s <= 2.5; s += 0.25)
        m_scale->addItem(QString::number(s, 'f', 2), trimmedNumber(s));
    {
        // 1.50 is the 1.5 item and 1.0 the default; any other value set by
        // hand gets an item of its own.
        const QString raw = get(kScale);
        bool ok = false;
        const double v = raw.toDouble(&ok);
        QString scale = raw;
        if (ok)
            scale = v == 1.0 ? QString() : trimmedNumber(v);
        selectValue(m_scale, m_scale->findData(scale) >= 0 ? scale : raw, raw);
    }

    m_wheel->clear();
    for (double s : {0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 2.5, 3.0})
        m_wheel->addItem(s == 1.0 ? QStringLiteral("1 (default)") : trimmedNumber(s), trimmedNumber(s));
    {
        bool ok = false;
        const double v = get(kWheel).toDouble(&ok);
        const bool usable = ok && v != 0.0;
        m_invert->setChecked(usable && v < 0);
        const QString speed = trimmedNumber(usable ? qAbs(v) : 1.0);
        selectValue(m_wheel, speed, speed);
    }

    m_present->clear();
    m_present->addItem(QStringLiteral("Automatic (default)"), QString());
    m_present->addItem(QStringLiteral("Mailbox"), QStringLiteral("mailbox"));
    m_present->addItem(QStringLiteral("Immediate"), QStringLiteral("immediate"));
    m_present->addItem(QStringLiteral("FIFO (vsync)"), QStringLiteral("fifo"));
    {
        const QString mode = get(kPresent);
        selectValue(m_present, mode == QLatin1String("auto") ? QString() : mode, mode);
    }

    m_renderer->clear();
    m_renderer->addItem(QStringLiteral("Vulkan (default)"), QString());
    m_renderer->addItem(QStringLiteral("OpenGL ES (fallback)"), QStringLiteral("1"));
    m_renderer->setCurrentIndex(conf.contains(kNoVulkan) ? 1 : 0);

    m_audio->clear();
    m_audio->addItem(QStringLiteral("System default (/dev/dsp)"), QString());
    for (const SoundDevice &d : soundDevices()) {
        if (!d.play)
            continue;
        // Long names (a USB headset's vendor string) are cut short so the
        // box stays the width of the others; the whole name is its tooltip.
        const QString full = QStringLiteral("pcm%1: %2%3").arg(d.unit).arg(d.name, d.isDefault ? QStringLiteral(" (default)")
                                                                                               : QString());
        const QString shown = full.size() > 40 ? full.left(37).trimmed() + QStringLiteral("...") : full;
        m_audio->addItem(shown, d.node());
        m_audio->setItemData(m_audio->count() - 1, full, Qt::ToolTipRole);
    }
    selectValue(m_audio, get(kAudio), QStringLiteral("%1 (not found)").arg(get(kAudio)));
    m_filling = false;
}

void ClientPage::write(const QString &key, const std::optional<QString> &value) {
    if (m_filling)
        return;
    QString error;
    if (!editSetting(key, value, &error))
        QMessageBox::warning(this, QStringLiteral("Client"), error);
    reload();
}

void ClientPage::writeWheel() {
    const QString speed = m_wheel->currentData().toString();
    const bool invert = m_invert->isChecked();
    if (speed == QLatin1String("1") && !invert)
        write(kWheel, std::nullopt);
    else
        write(kWheel, (invert ? QStringLiteral("-") : QString()) + speed);
}

void ClientPage::resolutionChosen(int index) {
    QString value = m_resolution->itemData(index).toString();
    if (value == QLatin1String("custom")) {
        static const QRegularExpression shape(QStringLiteral("^([0-9]{3,4})[xX]([0-9]{3,4})$"));
        QString text = QStringLiteral("1920x1080");
        for (;;) {
            bool ok = false;
            text = QInputDialog::getText(this, QStringLiteral("Resolution"),
                                         QStringLiteral("Width x height, from 320x240 to 7680x4320:"),
                                         QLineEdit::Normal, text, &ok)
                       .trimmed();
            if (!ok) {
                reload();
                return;
            }
            const auto m = shape.match(text);
            const int w = m.captured(1).toInt(), h = m.captured(2).toInt();
            if (m.hasMatch() && w >= 320 && w <= 7680 && h >= 240 && h <= 4320) {
                value = QStringLiteral("%1x%2").arg(w).arg(h);
                break;
            }
            QMessageBox::warning(this, QStringLiteral("Resolution"),
                                 QStringLiteral("%1 is not a size from 320x240 to 7680x4320.").arg(text));
        }
    }
    write(kResolution, value.isEmpty() ? std::nullopt : std::optional<QString>(value));
}

void ClientPage::reset() {
    if (QMessageBox::question(this, QStringLiteral("Reset client options"),
                              QStringLiteral("Empty settings.conf? Every client option goes back to its default, and "
                                             "so do the default profile and the video memory choice.")) !=
        QMessageBox::Yes)
        return;
    SettingsConf conf;
    QString error;
    conf.load(settingsConfPath(), &error);
    conf.clear();
    if (!conf.save(&error))
        QMessageBox::warning(this, QStringLiteral("Client"), error);
    emit m_state->settingsChanged();
}

} // namespace intox
