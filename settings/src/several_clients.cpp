// Intoxicated Settings: the Play page's "Run several" and "Running" groups.
//
// Each profile is its own Roblox account and storage, and one client runs
// per profile, so clients on different profiles can run side by side. This
// starts one launcher per checked profile, a moment apart, and lists the
// clients running now, read from each profile's lock every two seconds, with
// a Stop button that sends SIGTERM to the process named in that profile's
// client.pid (and only while that process holds the lock).

#include "app.h"
#include "style.h"

#include <QGroupBox>
#include <QHBoxLayout>
#include <QLabel>
#include <QMessageBox>
#include <QPushButton>
#include <QShowEvent>
#include <QTimer>
#include <QVBoxLayout>

namespace intox {

namespace {

// Between two launches, so the clients do not all load the engine at once.
constexpr int kStaggerMs = 1500;
constexpr int kRefreshMs = 2000;

QString profileState(const ProfileInfo &p) {
    QString text = p.signedIn ? QStringLiteral("Signed in")
                              : QStringLiteral("Not signed in: opens at the Roblox login screen in its own window");
    if (p.inUse)
        text += QStringLiteral(". Running now");
    return text;
}

void clearLayout(QVBoxLayout *layout) {
    while (QLayoutItem *item = layout->takeAt(0)) {
        if (QWidget *w = item->widget())
            w->deleteLater();
        delete item;
    }
}

} // namespace

SeveralClients::SeveralClients(AppState *state) : m_state(state) {
    auto *v = new QVBoxLayout(this);
    v->setContentsMargins(0, 0, 0, 0);
    v->setSpacing(8);

    Section pick = section(QStringLiteral("Run several"));
    auto *pv = new QVBoxLayout(pick.body);
    pv->setContentsMargins(8, 2, 8, 10);
    pv->setSpacing(0);
    m_profileRows = new QVBoxLayout;
    m_profileRows->setSpacing(0);
    pv->addLayout(m_profileRows);
    pv->addSpacing(6);
    auto *ph = new QHBoxLayout;
    ph->setContentsMargins(6, 0, 6, 0);
    auto *hint = new QLabel(QStringLiteral("Each checked profile starts in a window of its own, a moment apart. "
                                           "A profile is its own Roblox account; one client runs per profile."));
    hint->setObjectName(QStringLiteral("note"));
    hint->setWordWrap(true);
    ph->addWidget(hint, 1);
    m_playSelected = new QPushButton(QStringLiteral("Play selected"));
    m_playSelected->setObjectName(QStringLiteral("addBtn"));
    m_playSelected->setCursor(Qt::PointingHandCursor);
    ph->addWidget(m_playSelected, 0, Qt::AlignVCenter);
    pv->addLayout(ph);
    v->addWidget(pick.box);
    v->addSpacing(8);

    Section running = section(QStringLiteral("Running"));
    auto *rv = new QVBoxLayout(running.body);
    rv->setContentsMargins(8, 2, 8, 8);
    rv->setSpacing(0);
    m_runningRows = new QVBoxLayout;
    m_runningRows->setSpacing(0);
    rv->addLayout(m_runningRows);
    m_noneRunning = new QLabel(QStringLiteral("No client is running."));
    m_noneRunning->setObjectName(QStringLiteral("note"));
    m_noneRunning->setContentsMargins(6, 8, 6, 2);
    rv->addWidget(m_noneRunning);
    v->addWidget(running.box);

    m_timer = new QTimer(this);
    m_timer->setInterval(kRefreshMs);
    connect(m_timer, &QTimer::timeout, this, &SeveralClients::refresh);
    connect(m_playSelected, &QPushButton::clicked, this, &SeveralClients::playSelected);
    connect(state, &AppState::profilesChanged, this, &SeveralClients::refresh);
    refresh();
}

void SeveralClients::showEvent(QShowEvent *event) {
    QWidget::showEvent(event);
    refresh();
    m_timer->start();
}

void SeveralClients::hideEvent(QHideEvent *event) {
    QWidget::hideEvent(event);
    m_timer->stop();
}

void SeveralClients::refresh() {
    SettingsConf conf;
    conf.load(settingsConfPath());
    const QList<ProfileInfo> profiles = listProfiles(defaultProfileName(conf));

    QStringList names;
    for (const ProfileInfo &p : profiles)
        names.append(p.name);
    if (names != m_names) {
        // The set of profiles changed: rebuild, keeping what was checked.
        QSet<QString> checked;
        for (auto it = m_checks.cbegin(); it != m_checks.cend(); ++it) {
            if (it.value()->isChecked())
                checked.insert(it.key());
        }
        clearLayout(m_profileRows);
        m_checks.clear();
        m_descs.clear();
        for (int i = 0; i < profiles.size(); ++i) {
            if (i > 0)
                m_profileRows->addWidget(separator());
            auto *box = new Switch;
            box->setChecked(checked.contains(profiles[i].name));
            box->setToolTip(QStringLiteral("Start a client on %1 with Play selected").arg(profiles[i].name));
            QLabel *desc = nullptr;
            m_profileRows->addWidget(settingRow(profiles[i].name, QString(), box, &desc));
            m_checks.insert(profiles[i].name, box);
            m_descs.insert(profiles[i].name, desc);
        }
        m_names = names;
    }
    for (const ProfileInfo &p : profiles) {
        Switch *box = m_checks.value(p.name);
        QLabel *desc = m_descs.value(p.name);
        if (!box || !desc)
            continue;
        setProblem(desc, profileState(p));
        // A profile already running cannot get a second client.
        if (p.inUse)
            box->setChecked(false);
        box->setEnabled(!p.inUse && !m_pending.contains(p.name));
    }

    const QList<RunningClient> running = runningClients();
    QStringList shown;
    for (const RunningClient &c : running)
        shown.append(QStringLiteral("%1:%2").arg(c.profile).arg(c.pid));
    if (shown != m_shownRunning) {
        clearLayout(m_runningRows);
        for (int i = 0; i < running.size(); ++i) {
            const RunningClient &c = running[i];
            if (i > 0)
                m_runningRows->addWidget(separator());
            auto *stop = new QPushButton(QStringLiteral("Stop"));
            stop->setObjectName(QStringLiteral("deleteBtn"));
            stop->setCursor(Qt::PointingHandCursor);
            stop->setEnabled(c.pid > 0);
            stop->setToolTip(c.pid > 0 ? QStringLiteral("Close the client on %1").arg(c.profile)
                                       : QStringLiteral("This client has not said which process it is"));
            const QString desc = c.pid > 0 ? QStringLiteral("Process %1").arg(c.pid)
                                           : QStringLiteral("Starting, or a client that does not write client.pid");
            m_runningRows->addWidget(settingRow(c.profile, desc, stop));
            const QString profile = c.profile;
            connect(stop, &QPushButton::clicked, this, [this, profile] { stopOne(profile); });
        }
        m_shownRunning = shown;
    }
    m_noneRunning->setVisible(running.isEmpty());
}

bool SeveralClients::acceptWarning() {
    if (m_state->severalWarned)
        return true;
    QMessageBox box(QMessageBox::Warning, QStringLiteral("Run several"),
                    QStringLiteral("Roblox does not officially support running several clients at once."),
                    QMessageBox::NoButton, this);
    box.setInformativeText(
        QStringLiteral("A Byfron developer has said that running multiple clients may be treated as malicious. "
                       "Use alt accounts, and run several clients at your own risk."));
    QPushButton *go = box.addButton(QStringLiteral("Continue"), QMessageBox::AcceptRole);
    QPushButton *cancel = box.addButton(QStringLiteral("Cancel"), QMessageBox::RejectRole);
    box.setDefaultButton(cancel);
    box.exec();
    m_state->severalWarned = box.clickedButton() == go;
    return m_state->severalWarned;
}

void SeveralClients::playSelected() {
    refresh();
    QStringList chosen;
    int alreadyRunning = 0;
    for (auto it = m_checks.cbegin(); it != m_checks.cend(); ++it) {
        if (it.value()->isChecked() && it.value()->isEnabled())
            chosen.append(it.key());
    }
    for (const QString &name : m_names) {
        if (profileInUse(profileDir(name)))
            ++alreadyRunning;
    }
    // m_checks is a map, so sorted; start them in the order they are listed.
    QStringList ordered;
    for (const QString &name : m_names) {
        if (chosen.contains(name))
            ordered.append(name);
    }
    if (ordered.isEmpty()) {
        emit m_state->status(QStringLiteral("Check the profiles to start first."));
        return;
    }
    if (ordered.size() + alreadyRunning > 1 && !acceptWarning())
        return;

    for (int i = 0; i < ordered.size(); ++i) {
        const QString name = ordered[i];
        m_pending.insert(name);
        if (Switch *box = m_checks.value(name)) {
            box->setChecked(false);
            box->setEnabled(false);
        }
        QTimer::singleShot(i * kStaggerMs, this, [this, name] {
            m_pending.remove(name);
            QString error;
            if (startClient(name, QString(), &error))
                emit m_state->status(QStringLiteral("Starting Intoxicated on profile %1.").arg(name));
            else
                emit m_state->status(error);
            refresh();
        });
    }
}

void SeveralClients::stopOne(const QString &profile) {
    QString error;
    if (stopClient(profile, &error))
        emit m_state->status(QStringLiteral("Stopping the client on profile %1.").arg(profile));
    else
        emit m_state->status(error);
    refresh();
}

} // namespace intox
