// Intoxicated Settings: the shared state, the window, the name dialog and
// the sudo askpass helper.

#include "app.h"
#include "style.h"

#include <QApplication>
#include <QCloseEvent>
#include <QDialogButtonBox>
#include <QDir>
#include <QFileInfo>
#include <QHBoxLayout>
#include <QIcon>
#include <QLabel>
#include <QLineEdit>
#include <QListWidget>
#include <QMessageBox>
#include <QProcess>
#include <QProcessEnvironment>
#include <QPushButton>
#include <QStackedWidget>
#include <QTimer>
#include <QVBoxLayout>

#include <cstdio>

namespace intox {

// --- AppState --------------------------------------------------------------

AppState::AppState(QObject *parent) : QObject(parent) {
    SettingsConf conf;
    conf.load(settingsConfPath());
    m_profile = defaultProfileName(conf);
    QString error;
    if (const auto defaults = readDefaultFlags(&error))
        m_defaults = *defaults;
    else
        m_defaultsError = error;
}

void AppState::setProfile(const QString &name) {
    if (name == m_profile)
        return;
    m_profile = name;
    emit profileChanged(name);
}

bool AppState::launch(const QString &link) {
    if (profileInUse(profileDir(m_profile))) {
        emit status(QStringLiteral("Profile %1 is already open in a client. Close it, or pick another profile.")
                        .arg(m_profile));
        return false;
    }
    const QString program = launcherPath();
    if (!QFileInfo(program).isExecutable()) {
        emit status(QStringLiteral("intoxicated was not found on PATH or in /usr/local/bin."));
        return false;
    }
    QProcess process;
    process.setProgram(program);
    process.setArguments(link.isEmpty() ? QStringList() : QStringList{link});
    QProcessEnvironment env = QProcessEnvironment::systemEnvironment();
    env.insert(QStringLiteral("INTOXICATED_PROFILE"), m_profile);
    process.setProcessEnvironment(env);
    process.setWorkingDirectory(QDir::homePath());
    process.setStandardInputFile(QProcess::nullDevice());
    if (!process.startDetached()) {
        emit status(QStringLiteral("%1 could not be started: %2").arg(program, process.errorString()));
        return false;
    }
    emit status(link.isEmpty() ? QStringLiteral("Starting Intoxicated on profile %1.").arg(m_profile)
                               : QStringLiteral("Joining on profile %1.").arg(m_profile));
    return true;
}

// --- NameDialog ------------------------------------------------------------

NameDialog::NameDialog(QWidget *parent, const QString &title, const QString &prompt, const QString &placeholder,
                       std::function<QString(const QString &)> problem)
    : QDialog(parent) {
    setWindowTitle(title);
    auto *v = new QVBoxLayout(this);
    v->setContentsMargins(20, 18, 20, 16);
    v->setSpacing(8);
    auto *label = new QLabel(prompt);
    label->setWordWrap(true);
    v->addWidget(label);
    m_edit = new QLineEdit;
    m_edit->setPlaceholderText(placeholder);
    m_edit->setMinimumWidth(320);
    v->addWidget(m_edit);
    auto *error = new QLabel;
    error->setObjectName(QStringLiteral("error"));
    error->setWordWrap(true);
    v->addWidget(error);
    auto *buttons = new QDialogButtonBox(QDialogButtonBox::Ok | QDialogButtonBox::Cancel);
    v->addWidget(buttons);
    QPushButton *ok = buttons->button(QDialogButtonBox::Ok);
    connect(buttons, &QDialogButtonBox::accepted, this, &QDialog::accept);
    connect(buttons, &QDialogButtonBox::rejected, this, &QDialog::reject);
    auto recheck = [this, error, ok, problem] {
        const QString why = m_edit->text().isEmpty() ? QString() : problem(m_edit->text());
        setProblem(error, why);
        ok->setEnabled(!m_edit->text().isEmpty() && why.isEmpty());
        m_edit->setProperty("invalid", !why.isEmpty());
        m_edit->style()->unpolish(m_edit);
        m_edit->style()->polish(m_edit);
    };
    connect(m_edit, &QLineEdit::textChanged, this, recheck);
    recheck();
}

QString NameDialog::text() const { return m_edit->text(); }

// --- MainWindow ------------------------------------------------------------

QStringList MainWindow::pageNames() {
    return {QStringLiteral("Play"),     QStringLiteral("FastFlags"), QStringLiteral("Client"),
            QStringLiteral("Shaders"),  QStringLiteral("Macros"),    QStringLiteral("Profiles"),
            QStringLiteral("Mods"),     QStringLiteral("About")};
}

MainWindow::MainWindow(AppState *state, int startPage) : m_state(state) {
    setObjectName(QStringLiteral("root"));
    setWindowTitle(QStringLiteral("Intoxicated Settings"));
    setWindowIcon(QIcon(QStringLiteral(":/intoxicated-logo.png")));
    setMinimumSize(900, 600);

    auto *h = new QHBoxLayout(this);
    h->setContentsMargins(0, 0, 0, 0);
    h->setSpacing(0);
    h->addWidget(sidebar());

    auto *main = new QWidget;
    auto *v = new QVBoxLayout(main);
    v->setContentsMargins(0, 0, 0, 0);
    v->setSpacing(0);
    m_stack = new QStackedWidget;
    m_stack->addWidget(new PlayPage(state));
    m_stack->addWidget(new FlagsPage(state));
    m_stack->addWidget(new ClientPage(state));
    m_stack->addWidget(new ShadersPage(state));
    m_stack->addWidget(new MacrosPage(state));
    m_stack->addWidget(new ProfilesPage(state));
    m_stack->addWidget(new ModsPage);
    m_stack->addWidget(new AboutPage);
    v->addWidget(m_stack, 1);
    v->addWidget(bottomBar());
    h->addWidget(main, 1);

    connect(m_nav, &QListWidget::currentRowChanged, this, [this](int row) {
        m_stack->setCurrentIndex(row);
        for (int i = 0; i < m_nav->count(); ++i) {
            QFont f = m_nav->font();
            f.setWeight(i == row ? QFont::DemiBold : QFont::Normal);
            m_nav->item(i)->setFont(f);
        }
    });
    m_nav->setCurrentRow(qBound(0, startPage, m_nav->count() - 1));

    connect(state, &AppState::profilesChanged, this, &MainWindow::fillProfiles);
    connect(state, &AppState::settingsChanged, this, &MainWindow::fillProfiles);
    connect(state, &AppState::status, this, [this](const QString &message) {
        m_status->setText(message);
        m_statusTimer->start();
    });
    fillProfiles();
}

// The opening size, as a hint rather than a resize so that Qt's own
// -geometry option still applies.
QSize MainWindow::sizeHint() const { return {980, 680}; }

QWidget *MainWindow::sidebar() {
    auto *side = new QFrame;
    side->setObjectName(QStringLiteral("sidebar"));
    side->setFixedWidth(200);
    auto *lay = new QVBoxLayout(side);
    lay->setContentsMargins(14, 24, 14, 16);
    lay->setSpacing(0);

    auto *mark = new QLabel;
    mark->setAlignment(Qt::AlignHCenter);
    mark->setPixmap(logo(72, this));
    mark->setFixedHeight(72);
    lay->addWidget(mark);
    lay->addSpacing(8);
    auto *brand = new QLabel(QStringLiteral("Intoxicated"));
    brand->setObjectName(QStringLiteral("brand"));
    brand->setAlignment(Qt::AlignHCenter);
    lay->addWidget(brand);
    lay->addSpacing(22);

    m_nav = new QListWidget;
    m_nav->setObjectName(QStringLiteral("nav"));
    m_nav->setFocusPolicy(Qt::NoFocus);
    m_nav->setHorizontalScrollBarPolicy(Qt::ScrollBarAlwaysOff);
    m_nav->setVerticalScrollBarPolicy(Qt::ScrollBarAlwaysOff);
    m_nav->setSpacing(2);
    m_nav->setCursor(Qt::PointingHandCursor);
    for (const QString &name : pageNames()) {
        auto *item = new QListWidgetItem(name, m_nav);
        item->setSizeHint(QSize(0, 34));
    }
    lay->addWidget(m_nav, 1);
    return side;
}

QWidget *MainWindow::bottomBar() {
    auto *bar = new QFrame;
    bar->setObjectName(QStringLiteral("bottomBar"));
    auto *h = new QHBoxLayout(bar);
    h->setContentsMargins(28, 10, 28, 10);
    h->setSpacing(14);

    m_profiles = new Combo;
    m_profiles->setToolTip(QStringLiteral("The profile Play and the FastFlags page use"));
    h->addWidget(m_profiles, 0, Qt::AlignVCenter);
    connect(m_profiles, &QComboBox::activated, this,
            [this](int index) { m_state->setProfile(m_profiles->itemData(index).toString()); });

    m_status = new QLabel;
    m_status->setObjectName(QStringLiteral("status"));
    m_status->setWordWrap(true);
    h->addWidget(m_status, 1, Qt::AlignVCenter);
    m_statusTimer = new QTimer(this);
    m_statusTimer->setSingleShot(true);
    m_statusTimer->setInterval(10000);
    connect(m_statusTimer, &QTimer::timeout, m_status, &QLabel::clear);

    auto *play = new QPushButton(QStringLiteral("Play"));
    play->setObjectName(QStringLiteral("play"));
    play->setCursor(Qt::PointingHandCursor);
    play->setFocusPolicy(Qt::NoFocus);
    play->setToolTip(QStringLiteral("Start Intoxicated on the home page"));
    h->addWidget(play, 0, Qt::AlignVCenter);
    connect(play, &QPushButton::clicked, this, [this] { m_state->launch(QString()); });
    return bar;
}

void MainWindow::fillProfiles() {
    SettingsConf conf;
    conf.load(settingsConfPath());
    const QString preferred = defaultProfileName(conf);
    const QList<ProfileInfo> profiles = listProfiles(preferred);
    QString current = m_state->profile();
    bool found = false;
    for (const ProfileInfo &p : profiles)
        found = found || p.name == current;
    if (!found)
        current = preferred;

    const QSignalBlocker block(m_profiles);
    m_profiles->clear();
    for (const ProfileInfo &p : profiles) {
        const bool marked = p.name == preferred && p.name != kDefaultProfile;
        m_profiles->addItem(marked ? QStringLiteral("%1 (default)").arg(p.name) : p.name, p.name);
        if (p.name == current)
            m_profiles->setCurrentIndex(m_profiles->count() - 1);
    }
    // With one profile there is nothing to choose; the picker comes back
    // as soon as a second profile exists.
    m_profiles->setVisible(profiles.size() > 1);
    m_state->setProfile(current);
}

void MainWindow::closeEvent(QCloseEvent *event) {
    if (m_state->busy &&
        QMessageBox::question(this, QStringLiteral("Quit"),
                              QStringLiteral("An update is still running and quitting stops it. Quit anyway?")) !=
            QMessageBox::Yes) {
        event->ignore();
        return;
    }
    event->accept();
}

// --- Askpass ---------------------------------------------------------------

int runAskpass(int argc, char **argv) {
    // sudo -A runs the helper with its prompt as the one argument; the helper
    // script puts --askpass in front of it.
    QString prompt;
    for (int i = 1; i + 1 < argc; ++i) {
        if (qstrcmp(argv[i], "--askpass") == 0)
            prompt = QString::fromLocal8Bit(argv[i + 1]).trimmed();
    }
    QApplication app(argc, argv);
    applyLook(app);

    QDialog dialog;
    dialog.setObjectName(QStringLiteral("root"));
    dialog.setWindowTitle(QStringLiteral("Intoxicated Settings"));
    dialog.setWindowIcon(QIcon(QStringLiteral(":/intoxicated-logo.png")));
    auto *h = new QHBoxLayout(&dialog);
    h->setContentsMargins(20, 18, 20, 16);
    h->setSpacing(16);
    auto *mark = new QLabel;
    mark->setPixmap(logo(48, &dialog));
    mark->setAlignment(Qt::AlignTop);
    h->addWidget(mark);
    auto *v = new QVBoxLayout;
    v->setSpacing(8);
    auto *title = new QLabel(QStringLiteral("Intoxicated needs your password to install system files."));
    title->setObjectName(QStringLiteral("rowTitle"));
    title->setWordWrap(true);
    v->addWidget(title);
    if (!prompt.isEmpty()) {
        auto *said = new QLabel(prompt);
        said->setObjectName(QStringLiteral("note"));
        said->setWordWrap(true);
        v->addWidget(said);
    }
    auto *edit = new QLineEdit;
    edit->setEchoMode(QLineEdit::Password);
    edit->setMinimumWidth(360);
    v->addWidget(edit);
    auto *buttons = new QDialogButtonBox(QDialogButtonBox::Ok | QDialogButtonBox::Cancel);
    QObject::connect(buttons, &QDialogButtonBox::accepted, &dialog, &QDialog::accept);
    QObject::connect(buttons, &QDialogButtonBox::rejected, &dialog, &QDialog::reject);
    v->addWidget(buttons);
    h->addLayout(v, 1);
    edit->setFocus();

    if (dialog.exec() != QDialog::Accepted)
        return 1;
    QByteArray secret = edit->text().toUtf8();
    edit->clear();
    std::fwrite(secret.constData(), 1, size_t(secret.size()), stdout);
    std::fputc('\n', stdout);
    std::fflush(stdout);
    secret.fill('\0');
    return 0;
}

} // namespace intox
