// Intoxicated Settings: the Profiles page. A profile is a directory under
// .../intoxicated/profiles; the default one is named in settings.conf.

#include "app.h"
#include "style.h"

#include <QFileDialog>
#include <QSaveFile>
#include <QFile>
#include <QDir>
#include <QFileInfo>
#include <QGroupBox>
#include <QHBoxLayout>
#include <QHeaderView>
#include <QLabel>
#include <QMessageBox>
#include <QPushButton>
#include <QTableWidget>
#include <QTimer>
#include <QVBoxLayout>

namespace intox {

namespace {
const int kRowHeight = 30;
const int kHeaderHeight = 28;
} // namespace

ProfilesPage::ProfilesPage(AppState *state) : m_state(state) {
    QVBoxLayout *b = pageLayout(this, QStringLiteral("Profiles"),
                                QStringLiteral("Each profile is its own Roblox account and storage. One client runs "
                                               "per profile, so two different profiles can play at the same time."));
    Section s = section(QStringLiteral("Profiles"));
    // The table is its own rounded group; the buttons sit under it.
    s.box->setObjectName(QStringLiteral("plain"));
    auto *v = new QVBoxLayout(s.body);
    v->setContentsMargins(0, 0, 0, 0);
    v->setSpacing(8);

    m_table = new QTableWidget(0, 4);
    m_table->setObjectName(QStringLiteral("profiles"));
    m_table->setHorizontalHeaderLabels(
        {QStringLiteral("Profile"), QStringLiteral("Signed in"), QStringLiteral("In use"), QStringLiteral("Default")});
    m_table->verticalHeader()->setVisible(false);
    m_table->setShowGrid(false);
    m_table->setAlternatingRowColors(true);
    m_table->setSelectionBehavior(QAbstractItemView::SelectRows);
    m_table->setSelectionMode(QAbstractItemView::SingleSelection);
    m_table->setEditTriggers(QAbstractItemView::NoEditTriggers);
    m_table->setFocusPolicy(Qt::NoFocus);
    m_table->setHorizontalScrollBarPolicy(Qt::ScrollBarAlwaysOff);
    m_table->setVerticalScrollBarPolicy(Qt::ScrollBarAlwaysOff);
    auto *hh = m_table->horizontalHeader();
    hh->setHighlightSections(false);
    hh->setDefaultAlignment(Qt::AlignLeft | Qt::AlignVCenter);
    hh->setSectionResizeMode(0, QHeaderView::Stretch);
    for (int c = 1; c < 4; ++c) {
        hh->setSectionResizeMode(c, QHeaderView::Fixed);
        hh->resizeSection(c, 110);
    }
    hh->setFixedHeight(kHeaderHeight);
    m_table->verticalHeader()->setDefaultSectionSize(kRowHeight);
    m_table->verticalHeader()->setSectionResizeMode(QHeaderView::Fixed);
    v->addWidget(m_table);

    auto *buttons = new QHBoxLayout;
    buttons->setSpacing(8);
    m_new = new QPushButton(QStringLiteral("New..."));
    m_new->setObjectName(QStringLiteral("addBtn"));
    m_setDefault = new QPushButton(QStringLiteral("Set as default"));
    m_import = new QPushButton(QStringLiteral("Import..."));
    m_export = new QPushButton(QStringLiteral("Export..."));
    m_import->setToolTip(QStringLiteral("Make a new profile from an exported profile file"));
    m_export->setToolTip(QStringLiteral("Save this profile's name and flags to a file. The sign-in is not included."));
    m_delete = new QPushButton(QStringLiteral("Delete..."));
    setButtonIcon(m_new, QStringLiteral("plus"));
    setButtonIcon(m_setDefault, QStringLiteral("star"));
    setButtonIcon(m_import, QStringLiteral("folder-open"));
    setButtonIcon(m_export, QStringLiteral("export"));
    setButtonIcon(m_delete, QStringLiteral("trash"));
    for (QPushButton *button : {m_new, m_setDefault, m_import, m_export, m_delete}) {
        button->setCursor(Qt::PointingHandCursor);
        button->setFocusPolicy(Qt::NoFocus);
    }
    buttons->addWidget(m_new);
    buttons->addWidget(m_setDefault);
    buttons->addWidget(m_import);
    buttons->addWidget(m_export);
    buttons->addStretch(1);
    buttons->addWidget(m_delete);
    v->addLayout(buttons);
    b->addWidget(s.box);
    b->addSpacing(10);

    auto *note = new QLabel(QStringLiteral(
        "The default profile is the one Intoxicated starts with when nothing else is chosen, from a terminal or "
        "from roblox.com's Play button. The profile picked at the bottom of this window is the one Play and the "
        "FastFlags page use. Signing in happens in the client."));
    note->setObjectName(QStringLiteral("note"));
    note->setWordWrap(true);
    b->addWidget(note);
    b->addStretch(1);

    connect(m_table, &QTableWidget::itemSelectionChanged, this, &ProfilesPage::updateButtons);
    connect(m_new, &QPushButton::clicked, this, &ProfilesPage::newProfile);
    connect(m_setDefault, &QPushButton::clicked, this, &ProfilesPage::setDefault);
    connect(m_delete, &QPushButton::clicked, this, &ProfilesPage::deleteSelected);
    connect(m_import, &QPushButton::clicked, this, &ProfilesPage::importFile);
    connect(m_export, &QPushButton::clicked, this, &ProfilesPage::exportSelected);
    connect(state, &AppState::settingsChanged, this, &ProfilesPage::reload);

    // A client may start or stop while the page is open.
    m_poll = new QTimer(this);
    m_poll->setInterval(2000);
    connect(m_poll, &QTimer::timeout, this, &ProfilesPage::reload);
}

void ProfilesPage::showEvent(QShowEvent *event) {
    QWidget::showEvent(event);
    reload();
    m_poll->start();
}

void ProfilesPage::hideEvent(QHideEvent *event) {
    QWidget::hideEvent(event);
    m_poll->stop();
}

QString ProfilesPage::selected() const {
    const QList<QTableWidgetItem *> items = m_table->selectedItems();
    return items.isEmpty() ? QString() : m_table->item(items.first()->row(), 0)->text();
}

void ProfilesPage::reload() {
    SettingsConf conf;
    conf.load(settingsConfPath());
    m_default = defaultProfileName(conf);
    QString keep = selected();
    if (keep.isEmpty())
        keep = m_state->profile();
    m_profiles = listProfiles(m_default);

    const QSignalBlocker block(m_table);
    m_table->clearContents();
    m_table->setRowCount(int(m_profiles.size()));
    const QColor dim(0x80, 0x80, 0x8a);
    int select = 0;
    for (int r = 0; r < m_profiles.size(); ++r) {
        const ProfileInfo &p = m_profiles[r];
        auto *name = new QTableWidgetItem(p.name);
        QFont bold = name->font();
        bold.setWeight(QFont::DemiBold);
        name->setFont(bold);
        auto *signedIn = new QTableWidgetItem(p.signedIn ? QStringLiteral("Yes") : QStringLiteral("No"));
        auto *inUse = new QTableWidgetItem(p.inUse ? QStringLiteral("Yes, running") : QStringLiteral("No"));
        auto *isDefault = new QTableWidgetItem(p.name == m_default ? QStringLiteral("Default") : QString());
        if (!p.signedIn)
            signedIn->setForeground(dim);
        if (!p.inUse)
            inUse->setForeground(dim);
        else
            inUse->setForeground(QColor(0x7d, 0xd8, 0x8f));
        isDefault->setForeground(QColor(0xff, 0x6b, 0x70));
        m_table->setItem(r, 0, name);
        m_table->setItem(r, 1, signedIn);
        m_table->setItem(r, 2, inUse);
        m_table->setItem(r, 3, isDefault);
        if (p.name == keep)
            select = r;
    }
    m_table->selectRow(select);
    m_table->setFixedHeight(kHeaderHeight + int(m_profiles.size()) * kRowHeight + 2);
    updateButtons();
}

void ProfilesPage::updateButtons() {
    const QString name = selected();
    m_setDefault->setEnabled(!name.isEmpty() && name != m_default);
    m_export->setEnabled(!name.isEmpty());
    m_delete->setEnabled(!name.isEmpty() && name != kDefaultProfile);
    m_delete->setToolTip(name == kDefaultProfile ? QStringLiteral("The default profile cannot be deleted")
                                                 : QString());
}

void ProfilesPage::newProfile() {
    NameDialog dialog(this, QStringLiteral("New profile"),
                      QStringLiteral("A name for the new profile: 1 to 32 letters, digits, - and _. It starts "
                                     "signed out, with no flags."),
                      QStringLiteral("alt"), [](const QString &name) {
                          if (!isValidProfileName(name))
                              return QStringLiteral("Use 1 to 32 letters, digits, - and _.");
                          if (QFileInfo::exists(profileDir(name)))
                              return QStringLiteral("There is already a profile called %1.").arg(name);
                          return QString();
                      });
    if (dialog.exec() != QDialog::Accepted)
        return;
    QString error;
    if (!createProfile(dialog.text(), &error)) {
        QMessageBox::warning(this, QStringLiteral("New profile"), error);
        return;
    }
    emit m_state->profilesChanged();
    m_table->clearSelection();
    reload();
    for (int r = 0; r < m_table->rowCount(); ++r) {
        if (m_table->item(r, 0)->text() == dialog.text())
            m_table->selectRow(r);
    }
}

void ProfilesPage::exportSelected() {
    const QString name = selected();
    if (name.isEmpty())
        return;
    const FlagsFile flags = loadFlags(flagsPath(name));
    if (!flags.error.isEmpty()) {
        QMessageBox::warning(this, QStringLiteral("Export profile"),
                             QStringLiteral("The flags of %1 cannot be read: %2").arg(name, flags.error));
        return;
    }
    const QString path = QFileDialog::getSaveFileName(
        this, QStringLiteral("Export profile"),
        QDir::home().filePath(name + QStringLiteral(".intoxicated-profile.json")),
        QStringLiteral("Intoxicated profiles (*.json)"));
    if (path.isEmpty())
        return;
    QSaveFile out(path);
    if (!out.open(QIODevice::WriteOnly) || out.write(profileDocument(name, flags.values)) < 0 || !out.commit()) {
        QMessageBox::warning(this, QStringLiteral("Export profile"),
                             QStringLiteral("Could not write %1: %2").arg(path, out.errorString()));
        return;
    }
    QMessageBox::information(this, QStringLiteral("Export profile"),
                             QStringLiteral("Saved %1's name and %2 flags to %3. The sign-in is not in the file.")
                                 .arg(name)
                                 .arg(flags.values.size())
                                 .arg(path));
}

void ProfilesPage::importFile() {
    const QString path = QFileDialog::getOpenFileName(this, QStringLiteral("Import profile"), QDir::homePath(),
                                                      QStringLiteral("Intoxicated profiles (*.json)"));
    if (path.isEmpty())
        return;
    QFile in(path);
    if (!in.open(QIODevice::ReadOnly)) {
        QMessageBox::warning(this, QStringLiteral("Import profile"),
                             QStringLiteral("Could not read %1: %2").arg(path, in.errorString()));
        return;
    }
    ProfileFile file;
    QString error;
    if (!parseProfileDocument(in.readAll(), &file, &error)) {
        QMessageBox::warning(this, QStringLiteral("Import profile"), error);
        return;
    }
    NameDialog dialog(this, QStringLiteral("Import profile"),
                      QStringLiteral("A name for the imported profile (%1 flags). It starts signed out.")
                          .arg(file.flags.size()),
                      freeProfileName(file.name), [](const QString &name) {
                          if (!isValidProfileName(name))
                              return QStringLiteral("Use 1 to 32 letters, digits, - and _.");
                          if (QFileInfo::exists(profileDir(name)))
                              return QStringLiteral("There is already a profile called %1.").arg(name);
                          return QString();
                      });
    if (dialog.exec() != QDialog::Accepted)
        return;
    if (!importProfile(dialog.text(), file.flags, &error)) {
        QMessageBox::warning(this, QStringLiteral("Import profile"), error);
        return;
    }
    if (!file.skipped.isEmpty())
        QMessageBox::information(this, QStringLiteral("Import profile"),
                                 QStringLiteral("Left out %1 flag(s) whose name or value is not valid: %2")
                                     .arg(file.skipped.size())
                                     .arg(file.skipped.join(QStringLiteral(", "))));
    emit m_state->profilesChanged();
    m_table->clearSelection();
    reload();
    for (int r = 0; r < m_table->rowCount(); ++r)
        if (m_table->item(r, 0)->text() == dialog.text())
            m_table->selectRow(r);
}

void ProfilesPage::setDefault() {
    const QString name = selected();
    if (name.isEmpty())
        return;
    QString error;
    if (!editSetting(QStringLiteral("INTOXICATED_PROFILE"),
                     name == kDefaultProfile ? std::nullopt : std::optional<QString>(name), &error))
        QMessageBox::warning(this, QStringLiteral("Profiles"), error);
    reload();
    emit m_state->profilesChanged();
}

void ProfilesPage::deleteSelected() {
    const QString name = selected();
    if (name.isEmpty() || name == kDefaultProfile)
        return;
    const QString dir = profileDir(name);
    if (profileInUse(dir)) {
        QMessageBox::warning(this, QStringLiteral("Delete profile"),
                             QStringLiteral("%1 is open in a client. Close that client first.").arg(name));
        return;
    }
    QMessageBox ask(QMessageBox::Warning, QStringLiteral("Delete profile"),
                    QStringLiteral("Delete the profile %1?").arg(name), QMessageBox::NoButton, this);
    ask.setInformativeText(QStringLiteral("This removes its sign-in, Roblox's storage and that profile's flags: "
                                          "everything in %1. It cannot be undone.")
                               .arg(QDir::toNativeSeparators(dir)));
    QPushButton *remove = ask.addButton(QStringLiteral("Delete"), QMessageBox::DestructiveRole);
    ask.addButton(QMessageBox::Cancel);
    ask.setDefaultButton(QMessageBox::Cancel);
    ask.exec();
    if (ask.clickedButton() != remove)
        return;
    QString error;
    if (!deleteProfile(name, &error)) {
        QMessageBox::warning(this, QStringLiteral("Delete profile"), error);
    } else if (name == m_default) {
        editSetting(QStringLiteral("INTOXICATED_PROFILE"), std::nullopt, &error);
    }
    m_table->clearSelection();
    reload();
    emit m_state->profilesChanged();
}

} // namespace intox
