// Intoxicated Settings: the Play, Mods and About pages.

#include "app.h"
#include "style.h"

#include <QCoreApplication>
#include <QDesktopServices>
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QGroupBox>
#include <QHBoxLayout>
#include <QLabel>
#include <QLineEdit>
#include <QMessageBox>
#include <QPlainTextEdit>
#include <QProcess>
#include <QProcessEnvironment>
#include <QPushButton>
#include <QRegularExpression>
#include <QScrollBar>
#include <QTemporaryDir>
#include <QTextCursor>
#include <QUrl>
#include <QVBoxLayout>

#include <unistd.h>

namespace intox {

// --- Play ------------------------------------------------------------------

PlayPage::PlayPage(AppState *state) : m_state(state) {
    QVBoxLayout *b = pageLayout(this, QStringLiteral("Play"),
                                QStringLiteral("Start Intoxicated on the profile chosen below, or go straight into a "
                                               "game."));

    Section joinBox = section(QStringLiteral("Join a game"));
    auto *jv = new QVBoxLayout(joinBox.body);
    jv->setContentsMargins(14, 12, 14, 10);
    jv->setSpacing(8);
    auto *jh = new QHBoxLayout;
    jh->setSpacing(8);
    m_link = new QLineEdit;
    m_link->setPlaceholderText(QStringLiteral("Place id or link"));
    m_link->setClearButtonEnabled(true);
    jh->addWidget(m_link, 1);
    auto *join = new QPushButton(QStringLiteral("Join"));
    join->setObjectName(QStringLiteral("addBtn"));
    setButtonIcon(join, QStringLiteral("sign-in"));
    join->setCursor(Qt::PointingHandCursor);
    jh->addWidget(join);
    jv->addLayout(jh);
    auto *hint = new QLabel(QStringLiteral("A place id, a roblox:// link or a roblox.com game link. Play, at the "
                                           "bottom, opens the home page instead."));
    hint->setObjectName(QStringLiteral("note"));
    hint->setWordWrap(true);
    jv->addWidget(hint);
    b->addWidget(joinBox.box);
    b->addSpacing(8);
    b->addWidget(new SeveralClients(state));
    b->addSpacing(8);
    connect(join, &QPushButton::clicked, this, &PlayPage::join);
    connect(m_link, &QLineEdit::returnPressed, this, &PlayPage::join);

    Section updates = section(QStringLiteral("Updates"));
    auto *uv = new QVBoxLayout(updates.body);
    uv->setContentsMargins(0, 0, 0, 0);
    uv->setSpacing(0);
    m_updateApk = new QPushButton(QStringLiteral("Update Roblox build"));
    m_updateApp = new QPushButton(QStringLiteral("Update Intoxicated"));
    setButtonIcon(m_updateApk, QStringLiteral("download-simple"));
    setButtonIcon(m_updateApp, QStringLiteral("arrows-clockwise"));
    for (QPushButton *button : {m_updateApk, m_updateApp}) {
        button->setCursor(Qt::PointingHandCursor);
        button->setFocusPolicy(Qt::NoFocus);
        button->setMinimumWidth(190);
    }
    uv->addWidget(settingRow(QStringLiteral("Roblox build"),
                             QStringLiteral("Download the newest official build from APKPure and check Roblox's "
                                            "signature (intoxicated --update-apk)"),
                             m_updateApk));
    uv->addWidget(separator());
    uv->addWidget(settingRow(QStringLiteral("Intoxicated"),
                             QStringLiteral("Pull, build and reinstall Intoxicated and this app (intoxicated "
                                            "--update). Installing asks for your password."),
                             m_updateApp));
    // The log and the result, inset from the group's edges.
    auto *logBox = new QVBoxLayout;
    logBox->setContentsMargins(14, 4, 14, 14);
    logBox->setSpacing(0);
    uv->addLayout(logBox, 1);
    m_log = new QPlainTextEdit;
    m_log->setObjectName(QStringLiteral("log"));
    m_log->setReadOnly(true);
    m_log->setPlaceholderText(QStringLiteral("What an update prints shows here."));
    QFont mono(QStringLiteral("Noto Sans Mono"));
    mono.setStyleHint(QFont::Monospace);
    mono.setPixelSize(12);
    m_log->setFont(mono);
    m_log->setMinimumHeight(190);
    m_log->setLineWrapMode(QPlainTextEdit::WidgetWidth);
    logBox->addWidget(m_log, 1);
    m_result = new QLabel;
    m_result->setObjectName(QStringLiteral("status"));
    m_result->setWordWrap(true);
    m_result->setContentsMargins(2, 6, 2, 0);
    m_result->hide();
    logBox->addWidget(m_result);
    b->addWidget(updates.box, 1);
    b->addStretch(0);

    connect(m_updateApk, &QPushButton::clicked, this,
            [this] { runUpdate(QStringLiteral("Updating the Roblox build"), {QStringLiteral("--update-apk")}, false); });
    connect(m_updateApp, &QPushButton::clicked, this,
            [this] { runUpdate(QStringLiteral("Updating Intoxicated"), {QStringLiteral("--update")}, true); });
}

PlayPage::~PlayPage() {
    if (m_process) {
        m_process->disconnect(this);
        m_process->kill();
        m_process->waitForFinished(2000);
    }
}

void PlayPage::join() {
    const QString link = m_link->text().trimmed();
    static const QRegularExpression blank(QStringLiteral("[\\s\\x00-\\x1f\\x7f]"));
    if (link.isEmpty()) {
        emit m_state->status(QStringLiteral("Enter a place id or a link first."));
        return;
    }
    if (link.size() > 2048 || link.contains(blank)) {
        emit m_state->status(QStringLiteral("That is not a place id or a link."));
        return;
    }
    m_state->launch(link);
}

void PlayPage::runUpdate(const QString &what, const QStringList &args, bool install) {
    if (m_process)
        return;
    if (install &&
        QMessageBox::question(this, QStringLiteral("Update Intoxicated"),
                              QStringLiteral("Update Intoxicated now? The installer pulls the newest version into the "
                                             "checkout, builds it (a few minutes) and installs it again; installing "
                                             "asks for your password.")) != QMessageBox::Yes)
        return;
    const QString program = launcherPath();
    if (!QFileInfo(program).isExecutable()) {
        m_result->setObjectName(QStringLiteral("error"));
        setProblem(m_result, QStringLiteral("Failed: intoxicated was not found on PATH or in /usr/local/bin."));
        m_result->style()->unpolish(m_result);
        m_result->style()->polish(m_result);
        return;
    }

    QProcessEnvironment env = QProcessEnvironment::systemEnvironment();
    if (install) {
        // install.sh asks nothing with INTOXICATED_YES=1 and, with no
        // terminal, runs sudo -A, which asks through this app's dialog.
        env.insert(QStringLiteral("INTOXICATED_YES"), QStringLiteral("1"));
        m_askpassDir = std::make_unique<QTemporaryDir>(QDir::tempPath() + QStringLiteral("/intoxicated-askpass-XXXXXX"));
        const QString helper = m_askpassDir->path() + QStringLiteral("/askpass");
        QString self = QCoreApplication::applicationFilePath();
        self.replace(QLatin1Char('\''), QStringLiteral("'\\''"));
        QFile script(helper);
        if (m_askpassDir->isValid() && script.open(QIODevice::WriteOnly)) {
            script.write(QStringLiteral("#!/bin/sh\nexec '%1' --askpass \"$@\"\n").arg(self).toUtf8());
            script.close();
            script.setPermissions(QFileDevice::ReadOwner | QFileDevice::WriteOwner | QFileDevice::ExeOwner);
            env.insert(QStringLiteral("SUDO_ASKPASS"), helper);
        }
    }

    m_log->clear();
    append(QStringLiteral("$ intoxicated %1\n").arg(args.join(QLatin1Char(' '))));
    m_process = new QProcess(this);
    m_process->setProgram(program);
    m_process->setArguments(args);
    m_process->setProcessEnvironment(env);
    m_process->setProcessChannelMode(QProcess::MergedChannels);
    m_process->setStandardInputFile(QProcess::nullDevice());
    m_process->setWorkingDirectory(QDir::homePath());
    // A session of its own and so no terminal: sudo then asks through
    // SUDO_ASKPASS rather than on a terminal nobody is looking at.
    m_process->setChildProcessModifier([] { ::setsid(); });
    connect(m_process, &QProcess::readyReadStandardOutput, this,
            [this] { append(QString::fromUtf8(m_process->readAllStandardOutput())); });
    connect(m_process, &QProcess::finished, this,
            [this](int code, QProcess::ExitStatus status) { finished(code, status == QProcess::CrashExit); });
    connect(m_process, &QProcess::errorOccurred, this, [this](QProcess::ProcessError error) {
        if (error == QProcess::FailedToStart)
            finished(-1, true);
    });
    m_running = what;
    m_updateApk->setEnabled(false);
    m_updateApp->setEnabled(false);
    m_state->busy = true;
    m_result->setObjectName(QStringLiteral("status"));
    m_result->style()->unpolish(m_result);
    m_result->style()->polish(m_result);
    setProblem(m_result, what + QStringLiteral("..."));
    m_process->start();
}

void PlayPage::finished(int code, bool crashed) {
    if (!m_process)
        return;
    append(QString::fromUtf8(m_process->readAllStandardOutput()));
    const bool ok = !crashed && code == 0;
    const QString line = ok ? QStringLiteral("Done: %1 finished.").arg(m_running)
                            : crashed ? QStringLiteral("Failed: %1 stopped (%2).").arg(m_running, m_process->errorString())
                                      : QStringLiteral("Failed: %1 ended with status %2. The output above says why.")
                                            .arg(m_running)
                                            .arg(code);
    append(QStringLiteral("\n") + line + QStringLiteral("\n"));
    m_result->setObjectName(ok ? QStringLiteral("status") : QStringLiteral("error"));
    setProblem(m_result, line);
    m_result->style()->unpolish(m_result);
    m_result->style()->polish(m_result);
    m_process->deleteLater();
    m_process = nullptr;
    m_askpassDir.reset();
    m_updateApk->setEnabled(true);
    m_updateApp->setEnabled(true);
    m_state->busy = false;
}

void PlayPage::append(const QString &text) {
    if (text.isEmpty())
        return;
    QString clean = text;
    clean.replace(QStringLiteral("\r\n"), QStringLiteral("\n"));
    clean.replace(QLatin1Char('\r'), QLatin1Char('\n'));
    QScrollBar *bar = m_log->verticalScrollBar();
    const bool atEnd = bar->value() >= bar->maximum() - 4;
    QTextCursor cursor = m_log->textCursor();
    cursor.movePosition(QTextCursor::End);
    cursor.insertText(clean);
    if (atEnd)
        bar->setValue(bar->maximum());
}

// --- Mods ------------------------------------------------------------------

ModsPage::ModsPage() {
    QVBoxLayout *b = pageLayout(this, QStringLiteral("Mods"),
                                QStringLiteral("Custom fonts, sounds and textures are coming in a later version."));
    b->addSpacing(8);
    auto *row = new QHBoxLayout;
    auto *open = new QPushButton(QStringLiteral("Open mods folder"));
    setButtonIcon(open, QStringLiteral("folder"));
    open->setEnabled(false);
    row->addWidget(open);
    row->addStretch(1);
    b->addLayout(row);
    b->addStretch(1);
}

// --- About -----------------------------------------------------------------

AboutPage::AboutPage() {
    QVBoxLayout *b = pageLayout(this, QStringLiteral("About"), QString());
    Section s = section(QString());
    // No title, so none of the room a title takes above the box.
    s.box->setStyleSheet(QStringLiteral("QGroupBox { margin-top: 6px; }"));
    auto *h = new QHBoxLayout(s.body);
    h->setContentsMargins(22, 22, 22, 22);
    h->setSpacing(26);
    auto *mark = new QLabel;
    mark->setPixmap(logo(128, this));
    mark->setAlignment(Qt::AlignTop);
    h->addWidget(mark, 0, Qt::AlignTop);

    auto *v = new QVBoxLayout;
    v->setSpacing(6);
    auto *name = new QLabel(QStringLiteral("Intoxicated"));
    name->setObjectName(QStringLiteral("aboutName"));
    v->addWidget(name);
    auto *tag = new QLabel(QStringLiteral("Roblox on FreeBSD, natively."));
    tag->setObjectName(QStringLiteral("aboutTag"));
    v->addWidget(tag);
    v->addSpacing(10);
    m_version = new QLabel(QStringLiteral("Asking intoxicated for its version..."));
    m_version->setObjectName(QStringLiteral("note"));
    m_version->setWordWrap(true);
    m_version->setTextInteractionFlags(Qt::TextSelectableByMouse);
    v->addWidget(m_version);
    auto *licence = new QLabel(QStringLiteral("Licence: GPL-3.0-or-later"));
    licence->setObjectName(QStringLiteral("note"));
    v->addWidget(licence);
    v->addSpacing(6);
    auto *link = new QPushButton(QStringLiteral("https://github.com/neoppix/intoxicated"));
    link->setObjectName(QStringLiteral("linkBtn"));
    link->setCursor(Qt::PointingHandCursor);
    link->setFocusPolicy(Qt::NoFocus);
    connect(link, &QPushButton::clicked, this,
            [] { QDesktopServices::openUrl(QUrl(QStringLiteral("https://github.com/neoppix/intoxicated"))); });
    v->addWidget(link, 0, Qt::AlignLeft);
    v->addStretch(1);
    h->addLayout(v, 1);
    b->addWidget(s.box);
    b->addStretch(1);
}

void AboutPage::showEvent(QShowEvent *event) {
    QWidget::showEvent(event);
    if (m_query)
        return;
    // Asked once, in the background: the launcher prints the client's banner.
    m_query = new QProcess(this);
    m_query->setProgram(launcherPath());
    m_query->setArguments({QStringLiteral("--version")});
    m_query->setStandardInputFile(QProcess::nullDevice());
    connect(m_query, &QProcess::finished, this, [this](int code, QProcess::ExitStatus status) {
        const QString line = QString::fromUtf8(m_query->readAllStandardOutput()).section(QLatin1Char('\n'), 0, 0).trimmed();
        if (status == QProcess::NormalExit && code == 0 && !line.isEmpty())
            m_version->setText(line);
        else
            m_version->setText(QStringLiteral("The version is unknown: intoxicated --version did not answer."));
    });
    connect(m_query, &QProcess::errorOccurred, this, [this](QProcess::ProcessError error) {
        if (error == QProcess::FailedToStart)
            m_version->setText(QStringLiteral("The version is unknown: intoxicated was not found."));
    });
    m_query->start();
}

} // namespace intox
