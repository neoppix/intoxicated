// Intoxicated Settings: the FastFlags page. Presets and every key of the
// chosen profile's flags.json, edited in place, imported and exported.

#include "app.h"
#include "style.h"

#include <QDialogButtonBox>
#include <QDir>
#include <QFile>
#include <QFileDialog>
#include <QFileInfo>
#include <QFormLayout>
#include <QFrame>
#include <QGroupBox>
#include <QHBoxLayout>
#include <QHeaderView>
#include <QInputDialog>
#include <QLabel>
#include <QLineEdit>
#include <QMessageBox>
#include <QPushButton>
#include <QStyle>
#include <QTableWidget>
#include <QTimer>
#include <QVBoxLayout>

namespace intox {

namespace {

const int kRowHeight = 30;
const int kHeaderHeight = 28;

QFont monoFont() {
    QFont mono(QStringLiteral("Noto Sans Mono"));
    mono.setStyleHint(QFont::Monospace);
    mono.setPixelSize(12);
    return mono;
}

void markInvalid(QWidget *w, bool invalid) {
    w->setProperty("invalid", invalid);
    w->style()->unpolish(w);
    w->style()->polish(w);
}

QString megabytes(const QString &bytes) {
    bool ok = false;
    const qlonglong n = bytes.toLongLong(&ok);
    if (!ok)
        return bytes;
    return QStringLiteral("%1 MB").arg(qRound64(double(n) / (1024.0 * 1024.0)));
}

} // namespace

// --- FlagDelegate ----------------------------------------------------------

FlagDelegate::FlagDelegate(FlagsPage *page, QLabel *problem) : QStyledItemDelegate(page), m_page(page), m_problem(problem) {}

QWidget *FlagDelegate::createEditor(QWidget *parent, const QStyleOptionViewItem &, const QModelIndex &index) const {
    auto *editor = new QLineEdit(parent);
    editor->setFont(monoFont());
    const QPersistentModelIndex at(index);
    connect(editor, &QLineEdit::textChanged, editor, [this, editor, at] {
        const QString why = m_page->cellProblem(at.row(), at.column(), editor->text());
        setProblem(m_problem, why);
        markInvalid(editor, !why.isEmpty());
    });
    setProblem(m_problem, QString());
    return editor;
}

void FlagDelegate::setModelData(QWidget *editor, QAbstractItemModel *model, const QModelIndex &index) const {
    auto *line = static_cast<QLineEdit *>(editor);
    const QString why = m_page->cellProblem(index.row(), index.column(), line->text());
    if (!why.isEmpty()) {
        setProblem(m_problem, why + QStringLiteral(" The change was not kept."));
        return;
    }
    setProblem(m_problem, QString());
    QStyledItemDelegate::setModelData(editor, model, index);
}

// --- AddFlagDialog ---------------------------------------------------------

AddFlagDialog::AddFlagDialog(QWidget *parent, const FlagMap &current) : QDialog(parent), m_current(current) {
    setWindowTitle(QStringLiteral("Add flag"));
    auto *v = new QVBoxLayout(this);
    v->setContentsMargins(20, 18, 20, 16);
    v->setSpacing(10);
    auto *form = new QFormLayout;
    form->setHorizontalSpacing(12);
    form->setVerticalSpacing(8);
    m_name = new QLineEdit;
    m_name->setPlaceholderText(QStringLiteral("FFlagExample"));
    m_name->setMinimumWidth(360);
    m_name->setFont(monoFont());
    m_value = new QLineEdit;
    m_value->setFont(monoFont());
    form->addRow(QStringLiteral("Name"), m_name);
    form->addRow(QStringLiteral("Value"), m_value);
    v->addLayout(form);
    m_problem = new QLabel;
    m_problem->setObjectName(QStringLiteral("error"));
    m_problem->setWordWrap(true);
    v->addWidget(m_problem);
    auto *buttons = new QDialogButtonBox(QDialogButtonBox::Cancel);
    m_ok = buttons->addButton(QStringLiteral("Add"), QDialogButtonBox::AcceptRole);
    m_ok->setDefault(true);
    connect(buttons, &QDialogButtonBox::accepted, this, &QDialog::accept);
    connect(buttons, &QDialogButtonBox::rejected, this, &QDialog::reject);
    v->addWidget(buttons);
    connect(m_name, &QLineEdit::textChanged, this, &AddFlagDialog::check);
    connect(m_value, &QLineEdit::textChanged, this, &AddFlagDialog::check);
    check();
}

void AddFlagDialog::check() {
    const QString name = m_name->text().trimmed();
    switch (flagKind(name)) {
    case FlagKind::Bool:
        m_value->setPlaceholderText(QStringLiteral("True or False"));
        break;
    case FlagKind::Int:
        m_value->setPlaceholderText(QStringLiteral("a whole number"));
        break;
    default:
        m_value->setPlaceholderText(QStringLiteral("any text"));
        break;
    }
    QString why;
    if (!name.isEmpty())
        why = checkFlagName(name);
    markInvalid(m_name, !why.isEmpty());
    bool valueBad = false;
    if (why.isEmpty() && !name.isEmpty() && !m_value->text().isEmpty()) {
        why = checkFlagValue(name, m_value->text(), &m_normalised);
        valueBad = !why.isEmpty();
    }
    markInvalid(m_value, valueBad);
    if (why.isEmpty() && m_current.contains(name))
        setProblem(m_problem, QStringLiteral("Replaces the value it has now, %1.").arg(m_current.value(name)));
    else
        setProblem(m_problem, why);
    const bool ready = !name.isEmpty() && why.isEmpty() &&
                       (!m_value->text().isEmpty() || flagKind(name) == FlagKind::String) &&
                       checkFlagValue(name, m_value->text(), &m_normalised).isEmpty();
    m_ok->setEnabled(ready);
}

QString AddFlagDialog::name() const { return m_name->text().trimmed(); }
QString AddFlagDialog::value() const { return m_normalised; }

// --- ImportDialog ----------------------------------------------------------

ImportDialog::ImportDialog(QWidget *parent, const QString &file, const QList<ImportItem> &items)
    : QDialog(parent), m_items(items) {
    setWindowTitle(QStringLiteral("Import flags"));
    resize(760, 460);
    auto *v = new QVBoxLayout(this);
    v->setContentsMargins(20, 18, 20, 16);
    v->setSpacing(10);
    int added = 0, changed = 0, refused = 0;
    for (const ImportItem &item : items) {
        if (!item.problem.isEmpty())
            ++refused;
        else if (item.current)
            ++changed;
        else
            ++added;
    }
    QString summary = QStringLiteral("From %1: %2 to add and %3 to change.").arg(file).arg(added).arg(changed);
    if (refused)
        summary += QStringLiteral(" %1 cannot be imported, with the reason beside each.").arg(refused);
    summary += QStringLiteral(" Untick what you want to leave out.");
    auto *label = new QLabel(summary);
    label->setWordWrap(true);
    v->addWidget(label);

    m_table = new QTableWidget(int(items.size()), 4);
    m_table->setHorizontalHeaderLabels({QString(), QStringLiteral("Flag"), QStringLiteral("Now"),
                                        QStringLiteral("After import")});
    m_table->verticalHeader()->setVisible(false);
    m_table->setShowGrid(false);
    m_table->setAlternatingRowColors(true);
    m_table->setSelectionMode(QAbstractItemView::NoSelection);
    m_table->setEditTriggers(QAbstractItemView::NoEditTriggers);
    m_table->setFocusPolicy(Qt::NoFocus);
    m_table->verticalHeader()->setDefaultSectionSize(kRowHeight);
    auto *hh = m_table->horizontalHeader();
    hh->setHighlightSections(false);
    hh->setDefaultAlignment(Qt::AlignLeft | Qt::AlignVCenter);
    hh->setSectionResizeMode(0, QHeaderView::Fixed);
    hh->resizeSection(0, 34);
    hh->setSectionResizeMode(1, QHeaderView::Stretch);
    hh->setSectionResizeMode(2, QHeaderView::ResizeToContents);
    hh->setSectionResizeMode(3, QHeaderView::ResizeToContents);
    const QFont mono = monoFont();
    for (int r = 0; r < items.size(); ++r) {
        const ImportItem &item = items[r];
        auto *tick = new QTableWidgetItem;
        auto *name = new QTableWidgetItem(item.name);
        auto *now = new QTableWidgetItem(item.current ? *item.current : QStringLiteral("not set"));
        auto *after = new QTableWidgetItem(item.problem.isEmpty() ? item.value : item.problem);
        name->setFont(mono);
        now->setFont(mono);
        if (!item.current)
            now->setForeground(QColor(0x80, 0x80, 0x8a));
        if (item.problem.isEmpty()) {
            tick->setFlags(Qt::ItemIsEnabled | Qt::ItemIsUserCheckable);
            tick->setCheckState(Qt::Checked);
            after->setFont(mono);
        } else {
            tick->setFlags(Qt::NoItemFlags);
            tick->setCheckState(Qt::Unchecked);
            after->setForeground(QColor(0xff, 0x6b, 0x70));
            after->setToolTip(item.problem);
        }
        m_table->setItem(r, 0, tick);
        m_table->setItem(r, 1, name);
        m_table->setItem(r, 2, now);
        m_table->setItem(r, 3, after);
    }
    v->addWidget(m_table, 1);

    auto *buttons = new QDialogButtonBox(QDialogButtonBox::Cancel);
    QPushButton *import = buttons->addButton(QStringLiteral("Import"), QDialogButtonBox::AcceptRole);
    import->setObjectName(QStringLiteral("addBtn"));
    import->setEnabled(added + changed > 0);
    connect(buttons, &QDialogButtonBox::accepted, this, &QDialog::accept);
    connect(buttons, &QDialogButtonBox::rejected, this, &QDialog::reject);
    v->addWidget(buttons);
}

QSet<QString> ImportDialog::chosen() const {
    QSet<QString> names;
    for (int r = 0; r < m_table->rowCount(); ++r) {
        if (m_table->item(r, 0)->checkState() == Qt::Checked)
            names.insert(m_items[r].name);
    }
    return names;
}

// --- FlagsPage -------------------------------------------------------------

FlagsPage::FlagsPage(AppState *state) : m_state(state) {
    QVBoxLayout *b = pageLayout(this, QStringLiteral("FastFlags"), QString(), &m_subtitle);

    m_banner = new QFrame;
    m_banner->setObjectName(QStringLiteral("banner"));
    auto *bannerLayout = new QHBoxLayout(m_banner);
    bannerLayout->setContentsMargins(14, 10, 14, 10);
    bannerLayout->setSpacing(12);
    m_bannerText = new QLabel;
    m_bannerText->setWordWrap(true);
    bannerLayout->addWidget(m_bannerText, 1);
    auto *backup = new QPushButton(QStringLiteral("Back it up and start empty"));
    backup->setObjectName(QStringLiteral("addBtn"));
    backup->setCursor(Qt::PointingHandCursor);
    connect(backup, &QPushButton::clicked, this, &FlagsPage::backUp);
    bannerLayout->addWidget(backup, 0, Qt::AlignVCenter);
    b->addWidget(m_banner);
    m_banner->hide();

    // Quality: whole-profile presets, one at a time.
    Section quality = section(QStringLiteral("Quality"));
    auto *qv = new QVBoxLayout(quality.body);
    qv->setContentsMargins(12, 10, 12, 10);
    qv->setSpacing(8);
    auto *row = new QHBoxLayout;
    row->setSpacing(6);
    const QList<QualityPreset> qualities = qualityPresets();
    for (int i = 0; i < qualities.size(); ++i) {
        auto *btn = new QPushButton(qualities[i].name);
        btn->setObjectName(QStringLiteral("segment"));
        btn->setCheckable(true);
        btn->setCursor(Qt::PointingHandCursor);
        btn->setMinimumHeight(34);
        row->addWidget(btn, 1);
        m_qualityButtons.append(btn);
        connect(btn, &QPushButton::clicked, this, [this, i] {
            const QualityPreset preset = qualityPresets().at(i);
            if (!preset.warning.isEmpty() && matchingQualityPreset(m_flags) != i) {
                const auto answer = QMessageBox::warning(
                    this, preset.name, preset.warning + QStringLiteral("\n\nUse it anyway?"),
                    QMessageBox::Yes | QMessageBox::Cancel, QMessageBox::Cancel);
                if (answer != QMessageBox::Yes) {
                    fillPresets();
                    return;
                }
            }
            edit([&](FlagMap &flags) { applyQualityPreset(preset, flags); });
        });
    }
    qv->addLayout(row);
    m_qualityDesc = new QLabel;
    m_qualityDesc->setObjectName(QStringLiteral("note"));
    m_qualityDesc->setWordWrap(true);
    qv->addWidget(m_qualityDesc);
    m_qualityWarning = new QLabel;
    m_qualityWarning->setObjectName(QStringLiteral("error"));
    m_qualityWarning->setWordWrap(true);
    m_qualityWarning->hide();
    qv->addWidget(m_qualityWarning);
    m_quality = quality.box;
    b->addWidget(quality.box);
    b->addSpacing(8);

    // Presets.
    Section presets = section(QStringLiteral("Presets"));
    m_presets = presets.box;
    auto *pv = new QVBoxLayout(presets.body);
    pv->setContentsMargins(8, 2, 8, 4);
    pv->setSpacing(0);
    m_fps = new Combo;
    connect(m_fps, &QComboBox::activated, this, &FlagsPage::fpsChosen);
    pv->addWidget(settingRow(QStringLiteral("Frame rate cap"), kFpsFlag, m_fps));
    pv->addWidget(separator());
    m_videoMemory = new Combo;
    connect(m_videoMemory, &QComboBox::activated, this, &FlagsPage::videoMemoryChosen);
    pv->addWidget(settingRow(QStringLiteral("Video memory budget"), kVideoMemoryFlag, m_videoMemory));
    const QList<SwitchPreset> presetList = switchPresets();
    for (int i = 0; i < presetList.size(); ++i) {
        pv->addWidget(separator());
        auto *sw = new Switch;
        QLabel *desc = nullptr;
        pv->addWidget(settingRow(presetList[i].title, presetList[i].description, sw, &desc));
        m_switches.append(sw);
        m_switchDescs.append(desc);
        connect(sw, &QCheckBox::clicked, this, [this, i](bool on) {
            const SwitchPreset preset = switchPresets().at(i);
            const FlagMap defaults = m_state->defaults();
            edit([&](FlagMap &flags) { applyPreset(preset, on, flags, defaults); });
        });
    }
    m_defaultsNote = new QLabel;
    m_defaultsNote->setObjectName(QStringLiteral("error"));
    m_defaultsNote->setWordWrap(true);
    m_defaultsNote->setContentsMargins(6, 4, 6, 4);
    pv->addWidget(m_defaultsNote);
    b->addWidget(m_presets);
    b->addSpacing(8);

    // Custom flags.
    Section custom = section(QStringLiteral("Custom flags"));
    m_custom = custom.box;
    auto *cv = new QVBoxLayout(custom.body);
    cv->setContentsMargins(8, 8, 8, 6);
    cv->setSpacing(8);

    m_table = new QTableWidget(0, 3);
    m_table->setHorizontalHeaderLabels({QStringLiteral("Name"), QStringLiteral("Value"), QString()});
    m_table->verticalHeader()->setVisible(false);
    m_table->setShowGrid(false);
    m_table->setAlternatingRowColors(true);
    m_table->setSelectionMode(QAbstractItemView::NoSelection);
    m_table->setEditTriggers(QAbstractItemView::DoubleClicked | QAbstractItemView::EditKeyPressed);
    m_table->setHorizontalScrollBarPolicy(Qt::ScrollBarAlwaysOff);
    m_table->setVerticalScrollBarPolicy(Qt::ScrollBarAlwaysOff);
    m_table->setToolTip(QStringLiteral("Double-click a name or a value to change it"));
    auto *hh = m_table->horizontalHeader();
    hh->setHighlightSections(false);
    hh->setDefaultAlignment(Qt::AlignLeft | Qt::AlignVCenter);
    hh->setSectionResizeMode(0, QHeaderView::Stretch);
    hh->setSectionResizeMode(1, QHeaderView::Fixed);
    hh->setSectionResizeMode(2, QHeaderView::Fixed);
    hh->resizeSection(1, 190);
    hh->resizeSection(2, 92);
    hh->setFixedHeight(kHeaderHeight);
    m_table->verticalHeader()->setDefaultSectionSize(kRowHeight);
    m_table->verticalHeader()->setSectionResizeMode(QHeaderView::Fixed);
    cv->addWidget(m_table);

    m_empty = new QLabel(QStringLiteral("No flags in this profile's flags.json yet."));
    m_empty->setObjectName(QStringLiteral("note"));
    m_empty->setContentsMargins(4, 6, 4, 6);
    cv->addWidget(m_empty);

    m_tableError = new QLabel;
    m_tableError->setObjectName(QStringLiteral("error"));
    m_tableError->setWordWrap(true);
    cv->addWidget(m_tableError);
    m_table->setItemDelegate(new FlagDelegate(this, m_tableError));
    connect(m_table, &QTableWidget::cellChanged, this, &FlagsPage::cellEdited);

    auto *buttons = new QHBoxLayout;
    buttons->setSpacing(8);
    auto *add = new QPushButton(QStringLiteral("Add flag"));
    add->setObjectName(QStringLiteral("addBtn"));
    auto *import = new QPushButton(QStringLiteral("Import JSON..."));
    m_export = new QPushButton(QStringLiteral("Export JSON..."));
    m_reset = new QPushButton(QStringLiteral("Reset"));
    for (QPushButton *button : {add, import, m_export, m_reset}) {
        button->setCursor(Qt::PointingHandCursor);
        button->setFocusPolicy(Qt::NoFocus);
    }
    buttons->addWidget(add);
    buttons->addWidget(import);
    buttons->addWidget(m_export);
    buttons->addStretch(1);
    buttons->addWidget(m_reset);
    cv->addLayout(buttons);
    connect(add, &QPushButton::clicked, this, &FlagsPage::addFlag);
    connect(import, &QPushButton::clicked, this, &FlagsPage::importJson);
    connect(m_export, &QPushButton::clicked, this, &FlagsPage::exportJson);
    connect(m_reset, &QPushButton::clicked, this, &FlagsPage::resetFlags);
    b->addWidget(m_custom);
    b->addStretch(1);

    connect(state, &AppState::profileChanged, this, &FlagsPage::reload);
    connect(state, &AppState::settingsChanged, this, &FlagsPage::reload);
}

void FlagsPage::showEvent(QShowEvent *event) {
    QWidget::showEvent(event);
    reload();
}

void FlagsPage::reload() {
    const QString profile = m_state->profile();
    const FlagsFile file = loadFlags(flagsPath(profile));
    m_broken = !file.error.isEmpty();
    m_flags = m_broken ? FlagMap() : file.values;
    m_subtitle->setText(QStringLiteral("Flags in the %1 profile's flags.json. Values set here win over the "
                                       "client's defaults.")
                            .arg(profile));
    if (m_broken) {
        m_bannerText->setText(QStringLiteral("%1 cannot be used: %2. The client cannot read it either. It is left "
                                             "as it is until you back it up.")
                                  .arg(QDir::toNativeSeparators(flagsPath(profile)), file.error));
    }
    m_banner->setVisible(m_broken);
    m_presets->setEnabled(!m_broken);
    m_quality->setEnabled(!m_broken);
    m_custom->setEnabled(!m_broken);
    setProblem(m_tableError, QString());
    fillPresets();
    fillTable();
}

bool FlagsPage::edit(const std::function<void(FlagMap &)> &change) {
    QString error;
    const bool ok = editFlags(flagsPath(m_state->profile()), change, &error);
    if (!ok)
        QMessageBox::warning(this, QStringLiteral("FastFlags"), error);
    reload();
    return ok;
}

void FlagsPage::fillPresets() {
    m_filling = true;
    SettingsConf conf;
    conf.load(settingsConfPath());
    const FlagMap &defaults = m_state->defaults();

    const QString fps = effectiveFps(m_flags, defaults, conf);
    m_fps->clear();
    // The engine still stops at 240: measured on 2.748 with the target at 9999,
    // and with each cap flag and the saved FramerateCap tried; none moved it.
    m_fps->addItem(QStringLiteral("Unlimited (the engine stops at 240)"), QStringLiteral("9999"));
    for (const char *n : {"60", "144", "165", "240"})
        m_fps->addItem(QStringLiteral("%1 fps").arg(QLatin1String(n)), QString::fromLatin1(n));
    int at = m_fps->findData(fps);
    if (at < 0) {
        m_fps->addItem(QStringLiteral("Custom: %1 fps").arg(fps), fps);
        at = m_fps->count() - 1;
    }
    m_fps->addItem(QStringLiteral("Custom..."), QStringLiteral("custom"));
    m_fps->setCurrentIndex(at);

    const QString memory = effectiveVideoMemory(m_flags, defaults, conf);
    m_videoMemory->clear();
    m_videoMemory->addItem(QStringLiteral("1.5 GB"), QStringLiteral("1610612736"));
    m_videoMemory->addItem(QStringLiteral("2 GB"), QStringLiteral("2147483647"));
    m_videoMemory->addItem(QStringLiteral("Roblox's default"), QString());
    at = m_videoMemory->findData(memory);
    if (at < 0) {
        m_videoMemory->addItem(QStringLiteral("Custom: %1").arg(megabytes(memory)), memory);
        at = m_videoMemory->count() - 1;
    }
    m_videoMemory->addItem(QStringLiteral("Custom..."), QStringLiteral("custom"));
    m_videoMemory->setCurrentIndex(at);

    const int quality = matchingQualityPreset(m_flags);
    const QList<QualityPreset> qualities = qualityPresets();
    for (int i = 0; i < m_qualityButtons.size(); ++i)
        m_qualityButtons[i]->setChecked(i == quality);
    m_qualityDesc->setText(quality >= 0 ? qualities[quality].description
                                        : QStringLiteral("Custom: this profile's flags match none of the presets. "
                                                         "Picking one replaces the graphics flags it covers."));
    const bool warned = quality >= 0 && !qualities[quality].warning.isEmpty();
    m_qualityWarning->setText(warned ? qualities[quality].warning : QString());
    m_qualityWarning->setVisible(warned);

    const QList<SwitchPreset> presets = switchPresets();
    for (int i = 0; i < presets.size(); ++i) {
        const bool on = presetIsOn(presets[i], m_flags, defaults);
        m_switches[i]->setChecked(on);
        bool fromDefaults = on;
        for (const auto &entry : presets[i].on)
            fromDefaults = fromDefaults && !m_flags.contains(entry.first);
        m_switchDescs[i]->setText(presets[i].description +
                                  (fromDefaults ? QStringLiteral(" (the client's default)") : QString()));
    }
    m_defaultsNote->setText(
        m_state->defaultsError().isEmpty()
            ? QString()
            : QStringLiteral("The client's own flags could not be read (%1), so the switches show only what "
                             "flags.json sets.")
                  .arg(m_state->defaultsError()));
    m_defaultsNote->setVisible(!m_state->defaultsError().isEmpty());
    m_filling = false;
}

void FlagsPage::fillTable() {
    m_filling = true;
    m_table->setRowCount(0);
    m_table->setRowCount(int(m_flags.size()));
    const QFont mono = monoFont();
    int r = 0;
    for (auto it = m_flags.begin(); it != m_flags.end(); ++it, ++r) {
        auto *name = new QTableWidgetItem(it.key());
        name->setData(Qt::UserRole, it.key());
        name->setFont(mono);
        auto *value = new QTableWidgetItem(it.value());
        value->setFont(mono);
        m_table->setItem(r, 0, name);
        m_table->setItem(r, 1, value);

        auto *cell = new QWidget;
        auto *ch = new QHBoxLayout(cell);
        ch->setContentsMargins(0, 0, 10, 0);
        ch->addStretch(1);
        auto *del = new QPushButton(QStringLiteral("Delete"));
        del->setObjectName(QStringLiteral("deleteBtn"));
        del->setCursor(Qt::PointingHandCursor);
        del->setFocusPolicy(Qt::NoFocus);
        ch->addWidget(del, 0, Qt::AlignVCenter);
        m_table->setCellWidget(r, 2, cell);
        const QString key = it.key();
        connect(del, &QPushButton::clicked, this, [this, key] {
            QTimer::singleShot(0, this, [this, key] { edit([&](FlagMap &flags) { flags.remove(key); }); });
        });
    }
    m_table->setFixedHeight(kHeaderHeight + int(m_flags.size()) * kRowHeight + 2);
    m_table->setVisible(!m_flags.isEmpty());
    m_empty->setVisible(m_flags.isEmpty());
    m_reset->setEnabled(!m_flags.isEmpty());
    m_filling = false;
}

QString FlagsPage::cellProblem(int row, int column, const QString &text) const {
    const QTableWidgetItem *nameItem = m_table->item(row, 0);
    if (!nameItem)
        return QString();
    const QString old = nameItem->data(Qt::UserRole).toString();
    QString normalised;
    if (column == 1)
        return checkFlagValue(old, text, &normalised);
    if (text == old)
        return QString();
    QString why = checkFlagName(text);
    if (!why.isEmpty())
        return why;
    if (m_flags.contains(text))
        return QStringLiteral("There is already a flag called %1.").arg(text);
    why = checkFlagValue(text, m_flags.value(old), &normalised);
    if (!why.isEmpty())
        return QStringLiteral("Its value %1 does not fit: %2").arg(m_flags.value(old), why);
    return QString();
}

void FlagsPage::cellEdited(int row, int column) {
    if (m_filling)
        return;
    const QTableWidgetItem *nameItem = m_table->item(row, 0);
    const QTableWidgetItem *valueItem = m_table->item(row, 1);
    if (!nameItem || !valueItem)
        return;
    const QString old = nameItem->data(Qt::UserRole).toString();
    const QString name = nameItem->text();
    const QString value = valueItem->text();
    if ((column == 0 && name == old) || (column == 1 && m_flags.value(old) == value))
        return;
    // Out of the item's own setData before the table is filled again.
    QTimer::singleShot(0, this, [this, old, name, value, column] {
        edit([&](FlagMap &flags) {
            QString normalised;
            if (column == 0) {
                const QString kept = flags.take(old);
                checkFlagValue(name, kept, &normalised);
                flags.insert(name, normalised);
            } else {
                checkFlagValue(old, value, &normalised);
                flags.insert(old, normalised);
            }
        });
    });
}

void FlagsPage::fpsChosen(int index) {
    if (m_filling)
        return;
    QString value = m_fps->itemData(index).toString();
    if (value == QLatin1String("custom")) {
        bool ok = false;
        const int current = effectiveFps(m_flags, m_state->defaults(), SettingsConf()).toInt();
        const int n = QInputDialog::getInt(this, QStringLiteral("Frame rate cap"),
                                           QStringLiteral("Frames per second, 1 to 9999:"),
                                           current > 0 && current < 9999 ? current : 120, 1, 9999, 1, &ok);
        if (!ok) {
            fillPresets();
            return;
        }
        value = QString::number(n);
    }
    edit([&](FlagMap &flags) { flags.insert(kFpsFlag, value); });
}

void FlagsPage::videoMemoryChosen(int index) {
    if (m_filling)
        return;
    QString value = m_videoMemory->itemData(index).toString();
    if (value.isEmpty()) {
        QString error;
        if (!useRobloxVideoMemory(m_state->profile(), &error))
            QMessageBox::warning(this, QStringLiteral("FastFlags"), error);
        emit m_state->settingsChanged();
        return;
    }
    if (value == QLatin1String("custom")) {
        bool ok = false;
        const int mb = QInputDialog::getInt(this, QStringLiteral("Video memory budget"),
                                            QStringLiteral("Megabytes, 64 to 2047:"), 1024, 64, 2047, 64, &ok);
        if (!ok) {
            fillPresets();
            return;
        }
        value = QString::number(qint64(mb) * 1024 * 1024);
    }
    edit([&](FlagMap &flags) { flags.insert(kVideoMemoryFlag, value); });
}

void FlagsPage::addFlag() {
    AddFlagDialog dialog(this, m_flags);
    if (dialog.exec() != QDialog::Accepted)
        return;
    const QString name = dialog.name();
    const QString value = dialog.value();
    edit([&](FlagMap &flags) { flags.insert(name, value); });
}

void FlagsPage::importJson() {
    const QString file = QFileDialog::getOpenFileName(this, QStringLiteral("Import flags"), QDir::homePath(),
                                                      QStringLiteral("JSON files (*.json);;All files (*)"));
    if (file.isEmpty())
        return;
    QFile in(file);
    if (!in.open(QIODevice::ReadOnly)) {
        QMessageBox::warning(this, QStringLiteral("Import flags"), QStringLiteral("%1: %2").arg(file, in.errorString()));
        return;
    }
    FlagMap incoming;
    QStringList skipped;
    QString error;
    if (!parseFlags(in.read(16 * 1024 * 1024), &incoming, &error, &skipped)) {
        QMessageBox::warning(this, QStringLiteral("Import flags"),
                             QStringLiteral("%1 cannot be imported: %2.").arg(QFileInfo(file).fileName(), error));
        return;
    }
    const QList<ImportItem> plan = planImport(m_flags, incoming, skipped);
    if (plan.isEmpty()) {
        QMessageBox::information(this, QStringLiteral("Import flags"),
                                 QStringLiteral("Every flag in %1 already has that value in the %2 profile.")
                                     .arg(QFileInfo(file).fileName(), m_state->profile()));
        return;
    }
    ImportDialog dialog(this, QFileInfo(file).fileName(), plan);
    if (dialog.exec() != QDialog::Accepted)
        return;
    const QSet<QString> chosen = dialog.chosen();
    edit([&](FlagMap &flags) { applyImport(flags, plan, chosen); });
}

void FlagsPage::exportJson() {
    const QString suggested = QDir::homePath() + QStringLiteral("/%1-flags.json").arg(m_state->profile());
    const QString file = QFileDialog::getSaveFileName(this, QStringLiteral("Export flags"), suggested,
                                                      QStringLiteral("JSON files (*.json)"));
    if (file.isEmpty())
        return;
    QString error;
    if (!saveFlags(file, m_flags, &error))
        QMessageBox::warning(this, QStringLiteral("Export flags"), error);
    else
        emit m_state->status(QStringLiteral("Exported %1 flags to %2.").arg(m_flags.size()).arg(file));
}

void FlagsPage::resetFlags() {
    if (QMessageBox::question(this, QStringLiteral("Reset flags"),
                              QStringLiteral("Remove every flag from the %1 profile's flags.json? The launcher adds "
                                             "its frame rate and video memory flags back at the next launch.")
                                  .arg(m_state->profile())) != QMessageBox::Yes)
        return;
    edit([](FlagMap &flags) { flags.clear(); });
}

void FlagsPage::backUp() {
    QString error;
    const QString backup = backupBrokenFlags(flagsPath(m_state->profile()), &error);
    if (backup.isEmpty())
        QMessageBox::warning(this, QStringLiteral("FastFlags"), error);
    else
        emit m_state->status(QStringLiteral("The old file is kept as %1.").arg(QFileInfo(backup).fileName()));
    reload();
}

} // namespace intox
