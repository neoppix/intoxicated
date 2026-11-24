// Intoxicated Settings: the Macros page. A macro is a trigger key and a list
// of steps; the client replays the steps through its own input path when the
// key is pressed with the Roblox window focused. The page edits the profile's
// macros.json and saves it on every change.

#include "app.h"
#include "core.h"
#include "style.h"

#include <QDialogButtonBox>
#include <QGroupBox>
#include <QHBoxLayout>
#include <QLabel>
#include <QLineEdit>
#include <QListWidget>
#include <QMessageBox>
#include <QPushButton>
#include <QSpinBox>
#include <QVBoxLayout>

namespace intox {

namespace {

// The step kinds the dialog offers, in the order it lists them, each with the
// words it shows. The data matches core.h's StepKind.
struct StepChoice {
    StepKind kind;
    const char *label;
};
const StepChoice kStepChoices[] = {
    {StepKind::Tap, "Tap a key"},
    {StepKind::KeyDown, "Hold a key down"},
    {StepKind::KeyUp, "Release a key"},
    {StepKind::Click, "Click"},
    {StepKind::ButtonDown, "Press a mouse button"},
    {StepKind::ButtonUp, "Release a mouse button"},
    {StepKind::Move, "Move the pointer"},
    {StepKind::Scroll, "Scroll"},
    {StepKind::Text, "Type text"},
    {StepKind::Wait, "Wait"},
};

bool isKey(StepKind k) { return k == StepKind::Tap || k == StepKind::KeyDown || k == StepKind::KeyUp; }
bool isButton(StepKind k) {
    return k == StepKind::Click || k == StepKind::ButtonDown || k == StepKind::ButtonUp;
}
bool isPoint(StepKind k) { return isButton(k) || k == StepKind::Move || k == StepKind::Scroll; }

// A key picker filled from the shared table, the evdev code as each item's data.
Combo *keyCombo() {
    auto *c = new Combo;
    for (const KeyName &k : keyNames())
        c->addItem(k.label, k.evdev);
    return c;
}

void selectEvdev(Combo *c, int evdev) {
    const int at = c->findData(evdev);
    c->setCurrentIndex(at >= 0 ? at : 0);
}

QWidget *labelled(const QString &text, QWidget *control) {
    auto *row = new QWidget;
    auto *h = new QHBoxLayout(row);
    h->setContentsMargins(0, 0, 0, 0);
    h->setSpacing(10);
    auto *l = new QLabel(text);
    l->setMinimumWidth(96);
    h->addWidget(l);
    h->addWidget(control, 1);
    return row;
}

} // namespace

// --- StepDialog ------------------------------------------------------------

StepDialog::StepDialog(QWidget *parent, const MacroStep &step) : QDialog(parent) {
    setWindowTitle(QStringLiteral("Step"));
    auto *v = new QVBoxLayout(this);
    v->setContentsMargins(20, 18, 20, 16);
    v->setSpacing(10);

    m_type = new Combo;
    for (const StepChoice &c : kStepChoices)
        m_type->addItem(QString::fromLatin1(c.label), int(c.kind));
    v->addWidget(labelled(QStringLiteral("Do"), m_type));

    // The key and its modifiers.
    m_key = keyCombo();
    m_ctrl = new Switch;
    m_shift = new Switch;
    m_alt = new Switch;
    auto *keyMods = new QWidget;
    auto *km = new QHBoxLayout(keyMods);
    km->setContentsMargins(0, 0, 0, 0);
    km->setSpacing(8);
    km->addWidget(m_key, 1);
    for (const auto &[sw, name] : {std::pair<Switch *, const char *>{m_ctrl, "Ctrl"}, {m_shift, "Shift"}, {m_alt, "Alt"}}) {
        km->addWidget(new QLabel(QString::fromLatin1(name)));
        km->addWidget(sw);
    }
    m_keyRow = labelled(QStringLiteral("Key"), keyMods);
    v->addWidget(m_keyRow);

    // A point.
    m_x = new QSpinBox;
    m_y = new QSpinBox;
    for (QSpinBox *s : {m_x, m_y}) {
        s->setRange(0, 7680);
        s->setSingleStep(10);
    }
    auto *point = new QWidget;
    auto *pp = new QHBoxLayout(point);
    pp->setContentsMargins(0, 0, 0, 0);
    pp->setSpacing(8);
    pp->addWidget(new QLabel(QStringLiteral("x")));
    pp->addWidget(m_x, 1);
    pp->addWidget(new QLabel(QStringLiteral("y")));
    pp->addWidget(m_y, 1);
    m_pointRow = labelled(QStringLiteral("At"), point);
    v->addWidget(m_pointRow);

    // A mouse button.
    m_button = new Combo;
    m_button->addItem(QStringLiteral("Left"), 1);
    m_button->addItem(QStringLiteral("Right"), 2);
    m_button->addItem(QStringLiteral("Middle"), 3);
    m_buttonRow = labelled(QStringLiteral("Button"), m_button);
    v->addWidget(m_buttonRow);

    // A scroll amount.
    m_detents = new QSpinBox;
    m_detents->setRange(-20, 20);
    m_detents->setToolTip(QStringLiteral("Wheel notches; away from you is positive"));
    m_scrollRow = labelled(QStringLiteral("Notches"), m_detents);
    v->addWidget(m_scrollRow);

    // Typed text.
    m_text = new QLineEdit;
    m_text->setPlaceholderText(QStringLiteral("What to type"));
    m_textRow = labelled(QStringLiteral("Text"), m_text);
    v->addWidget(m_textRow);

    // A wait.
    m_wait = new QSpinBox;
    m_wait->setRange(0, 3600000);
    m_wait->setSingleStep(10);
    m_wait->setSuffix(QStringLiteral(" ms"));
    m_waitRow = labelled(QStringLiteral("For"), m_wait);
    v->addWidget(m_waitRow);

    auto *buttons = new QDialogButtonBox(QDialogButtonBox::Ok | QDialogButtonBox::Cancel);
    connect(buttons, &QDialogButtonBox::accepted, this, &QDialog::accept);
    connect(buttons, &QDialogButtonBox::rejected, this, &QDialog::reject);
    v->addWidget(buttons);

    // Fill from the step being edited.
    const int typeAt = m_type->findData(int(step.kind));
    m_type->setCurrentIndex(typeAt >= 0 ? typeAt : 0);
    selectEvdev(m_key, step.evdev ? step.evdev : keyNames().first().evdev);
    m_ctrl->setChecked(step.ctrl);
    m_shift->setChecked(step.shift);
    m_alt->setChecked(step.alt);
    m_x->setValue(int(step.x));
    m_y->setValue(int(step.y));
    m_button->setCurrentIndex(qMax(0, m_button->findData(step.button)));
    m_detents->setValue(int(step.detents ? step.detents : -1));
    m_text->setText(step.text);
    m_wait->setValue(int(qMin<quint64>(step.ms ? step.ms : 50, 3600000)));

    connect(m_type, &QComboBox::currentIndexChanged, this, &StepDialog::typeChanged);
    typeChanged();
}

void StepDialog::typeChanged() {
    const StepKind kind = StepKind(m_type->currentData().toInt());
    m_keyRow->setVisible(isKey(kind));
    m_pointRow->setVisible(isPoint(kind));
    m_buttonRow->setVisible(isButton(kind));
    m_scrollRow->setVisible(kind == StepKind::Scroll);
    m_textRow->setVisible(kind == StepKind::Text);
    m_waitRow->setVisible(kind == StepKind::Wait);
    adjustSize();
}

MacroStep StepDialog::step() const {
    MacroStep s;
    s.kind = StepKind(m_type->currentData().toInt());
    if (isKey(s.kind)) {
        s.evdev = m_key->currentData().toInt();
        s.ctrl = m_ctrl->isChecked();
        s.shift = m_shift->isChecked();
        s.alt = m_alt->isChecked();
    }
    if (isPoint(s.kind)) {
        s.x = m_x->value();
        s.y = m_y->value();
    }
    if (isButton(s.kind))
        s.button = m_button->currentData().toInt();
    if (s.kind == StepKind::Scroll)
        s.detents = m_detents->value();
    if (s.kind == StepKind::Text)
        s.text = m_text->text();
    if (s.kind == StepKind::Wait)
        s.ms = quint64(m_wait->value());
    return s;
}

// --- MacrosPage ------------------------------------------------------------

MacrosPage::MacrosPage(AppState *state) : m_state(state) {
    QVBoxLayout *b = pageLayout(this, QStringLiteral("Macros"),
                                QStringLiteral("A key plays a recorded sequence of input back through the client, so a "
                                               "macro only fires while the Roblox window is focused. Macros belong to "
                                               "the profile below and start at its next launch."));

    // The master switch.
    Section masterBox = section(QStringLiteral("Macros"));
    auto *mv = new QVBoxLayout(masterBox.body);
    mv->setContentsMargins(8, 2, 8, 4);
    mv->setSpacing(0);
    m_master = new Switch;
    mv->addWidget(settingRow(QStringLiteral("Run macros"),
                             QStringLiteral("Off turns every macro off at once (INTOXICATED_MACROS=0)"), m_master));
    b->addWidget(masterBox.box);
    b->addSpacing(8);
    connect(m_master, &QCheckBox::clicked, this, &MacrosPage::setMaster);

    // The list and the editor, side by side.
    Section listBox = section(QStringLiteral("Your macros"));
    auto *split = new QHBoxLayout(listBox.body);
    split->setContentsMargins(12, 12, 12, 12);
    split->setSpacing(16);

    auto *left = new QVBoxLayout;
    left->setSpacing(8);
    m_list = new QListWidget;
    m_list->setMinimumWidth(190);
    left->addWidget(m_list, 1);
    auto *listButtons = new QHBoxLayout;
    listButtons->setSpacing(6);
    m_add = new QPushButton(QStringLiteral("Add"));
    m_add->setObjectName(QStringLiteral("addBtn"));
    m_dup = new QPushButton(QStringLiteral("Duplicate"));
    m_del = new QPushButton(QStringLiteral("Delete"));
    for (QPushButton *btn : {m_add, m_dup, m_del}) {
        btn->setCursor(Qt::PointingHandCursor);
        btn->setFocusPolicy(Qt::NoFocus);
        listButtons->addWidget(btn);
    }
    left->addLayout(listButtons);
    split->addLayout(left, 0);

    // The editor.
    m_editor = new QWidget;
    auto *ev = new QVBoxLayout(m_editor);
    ev->setContentsMargins(0, 0, 0, 0);
    ev->setSpacing(10);
    m_name = new QLineEdit;
    m_name->setPlaceholderText(QStringLiteral("Macro name"));
    ev->addWidget(labelled(QStringLiteral("Name"), m_name));

    m_enabled = new Switch;
    ev->addWidget(labelled(QStringLiteral("On"), m_enabled));

    m_triggerKey = keyCombo();
    m_tCtrl = new Switch;
    m_tShift = new Switch;
    m_tAlt = new Switch;
    auto *trig = new QWidget;
    auto *th = new QHBoxLayout(trig);
    th->setContentsMargins(0, 0, 0, 0);
    th->setSpacing(8);
    th->addWidget(m_triggerKey, 1);
    for (const auto &[sw, name] : {std::pair<Switch *, const char *>{m_tCtrl, "Ctrl"}, {m_tShift, "Shift"}, {m_tAlt, "Alt"}}) {
        th->addWidget(new QLabel(QString::fromLatin1(name)));
        th->addWidget(sw);
    }
    ev->addWidget(labelled(QStringLiteral("Trigger"), trig));

    m_swallow = new Switch;
    ev->addWidget(labelled(QStringLiteral("Hide key"), m_swallow));
    m_swallow->setToolTip(QStringLiteral("Keep the trigger key from the game while the macro runs"));
    m_repeat = new Switch;
    ev->addWidget(labelled(QStringLiteral("Repeat"), m_repeat));
    m_repeat->setToolTip(QStringLiteral("Play the steps over and over while the key is held"));

    auto *stepsLabel = new QLabel(QStringLiteral("Steps"));
    stepsLabel->setObjectName(QStringLiteral("rowTitle"));
    ev->addWidget(stepsLabel);
    m_steps = new QListWidget;
    m_steps->setMinimumHeight(150);
    ev->addWidget(m_steps, 1);
    auto *stepButtons = new QHBoxLayout;
    stepButtons->setSpacing(6);
    m_stepAdd = new QPushButton(QStringLiteral("Add step"));
    m_stepAdd->setObjectName(QStringLiteral("addBtn"));
    m_stepEdit = new QPushButton(QStringLiteral("Edit"));
    m_stepUp = new QPushButton(QStringLiteral("Up"));
    m_stepDown = new QPushButton(QStringLiteral("Down"));
    m_stepDel = new QPushButton(QStringLiteral("Remove"));
    for (QPushButton *btn : {m_stepAdd, m_stepEdit, m_stepUp, m_stepDown, m_stepDel}) {
        btn->setCursor(Qt::PointingHandCursor);
        btn->setFocusPolicy(Qt::NoFocus);
        stepButtons->addWidget(btn);
    }
    ev->addLayout(stepButtons);

    m_status = new QLabel;
    m_status->setObjectName(QStringLiteral("note"));
    m_status->setWordWrap(true);
    m_status->hide();
    ev->addWidget(m_status);
    split->addWidget(m_editor, 1);
    b->addWidget(listBox.box, 1);

    connect(m_list, &QListWidget::currentRowChanged, this, [this](int) {
        showEditor();
        updateButtons();
    });
    connect(m_add, &QPushButton::clicked, this, &MacrosPage::addMacro);
    connect(m_dup, &QPushButton::clicked, this, &MacrosPage::duplicateMacro);
    connect(m_del, &QPushButton::clicked, this, &MacrosPage::deleteMacro);
    connect(m_name, &QLineEdit::textEdited, this, &MacrosPage::nameEdited);
    connect(m_triggerKey, &QComboBox::activated, this, &MacrosPage::triggerChanged);
    for (Switch *sw : {m_enabled, m_tCtrl, m_tShift, m_tAlt, m_swallow, m_repeat})
        connect(sw, &QCheckBox::clicked, this, &MacrosPage::switchesChanged);
    connect(m_steps, &QListWidget::itemDoubleClicked, this, &MacrosPage::editStep);
    connect(m_steps, &QListWidget::currentRowChanged, this, [this](int) { updateButtons(); });
    connect(m_stepAdd, &QPushButton::clicked, this, &MacrosPage::addStep);
    connect(m_stepEdit, &QPushButton::clicked, this, &MacrosPage::editStep);
    connect(m_stepUp, &QPushButton::clicked, this, [this] { moveStep(-1); });
    connect(m_stepDown, &QPushButton::clicked, this, [this] { moveStep(1); });
    connect(m_stepDel, &QPushButton::clicked, this, &MacrosPage::removeStep);
    connect(state, &AppState::profileChanged, this, &MacrosPage::reload);
}

void MacrosPage::showEvent(QShowEvent *event) {
    QWidget::showEvent(event);
    reload();
}

void MacrosPage::reload() {
    m_filling = true;
    SettingsConf conf;
    conf.load(settingsConfPath());
    const auto macrosValue = conf.value(kMacrosSetting);
    // On unless the line says 0: the client's own rule.
    m_master->setChecked(!(macrosValue && (macrosValue->isEmpty() || *macrosValue == QLatin1String("0"))));

    const MacrosFile file = loadMacros(macrosPath(m_state->profile()));
    m_broken = !file.error.isEmpty();
    m_macros = file.macros;
    if (m_broken) {
        setStatus(QStringLiteral("macros.json could not be read (%1); it is left as it is and not written over.")
                      .arg(file.error),
                  true);
    } else if (!file.skipped.isEmpty()) {
        setStatus(QStringLiteral("%1 skipped: %2").arg(file.skipped.size()).arg(file.skipped.join(QStringLiteral("; "))),
                  true);
    } else {
        setStatus(QString(), false);
    }
    fillList();
    if (!m_macros.isEmpty())
        m_list->setCurrentRow(0);
    showEditor();
    updateButtons();
    m_filling = false;
}

void MacrosPage::save() {
    if (m_broken)
        return;
    QString error;
    if (!saveMacros(macrosPath(m_state->profile()), m_macros, &error))
        QMessageBox::warning(this, QStringLiteral("Macros"), error);
}

void MacrosPage::fillList() {
    const int keep = m_list->currentRow();
    const QSignalBlocker block(m_list);
    m_list->clear();
    for (const MacroDef &m : m_macros) {
        QString trigger = keyLabel(m.triggerEvdev);
        if (m.triggerAlt)
            trigger.prepend(QStringLiteral("Alt+"));
        if (m.triggerShift)
            trigger.prepend(QStringLiteral("Shift+"));
        if (m.triggerCtrl)
            trigger.prepend(QStringLiteral("Ctrl+"));
        QString line = QStringLiteral("%1   (%2)").arg(m.name, trigger);
        if (!m.enabled)
            line += QStringLiteral("  off");
        m_list->addItem(line);
    }
    if (keep >= 0 && keep < m_list->count())
        m_list->setCurrentRow(keep);
}

int MacrosPage::selectedRow() const {
    const int row = m_list->currentRow();
    return (row >= 0 && row < m_macros.size()) ? row : -1;
}

MacroDef *MacrosPage::current() {
    const int row = selectedRow();
    return row < 0 ? nullptr : &m_macros[row];
}

void MacrosPage::showEditor() {
    m_filling = true;
    MacroDef *m = current();
    m_editor->setEnabled(m != nullptr && !m_broken);
    if (!m) {
        m_name->clear();
        m_steps->clear();
        m_filling = false;
        return;
    }
    m_name->setText(m->name);
    m_enabled->setChecked(m->enabled);
    selectEvdev(m_triggerKey, m->triggerEvdev ? m->triggerEvdev : keyNames().first().evdev);
    m_tCtrl->setChecked(m->triggerCtrl);
    m_tShift->setChecked(m->triggerShift);
    m_tAlt->setChecked(m->triggerAlt);
    m_swallow->setChecked(m->swallow);
    m_repeat->setChecked(m->repeat);
    fillSteps();
    m_filling = false;
}

void MacrosPage::fillSteps() {
    MacroDef *m = current();
    const QSignalBlocker block(m_steps);
    const int keep = m_steps->currentRow();
    m_steps->clear();
    if (m) {
        for (const MacroStep &s : m->steps)
            m_steps->addItem(describeStep(s));
    }
    if (keep >= 0 && m && keep < m->steps.size())
        m_steps->setCurrentRow(keep);
}

void MacrosPage::addMacro() {
    MacroDef m;
    m.name = QStringLiteral("New macro");
    m.triggerEvdev = keyNames().first().evdev;
    MacroStep s;
    s.kind = StepKind::Tap;
    s.evdev = keyEvdev(QStringLiteral("Space"));
    m.steps.append(s);
    m_macros.append(m);
    save();
    fillList();
    m_list->setCurrentRow(m_macros.size() - 1);
    showEditor();
    updateButtons();
    m_name->setFocus();
    m_name->selectAll();
}

void MacrosPage::duplicateMacro() {
    MacroDef *m = current();
    if (!m)
        return;
    MacroDef copy = *m;
    copy.name = m->name + QStringLiteral(" copy");
    m_macros.insert(selectedRow() + 1, copy);
    save();
    fillList();
    m_list->setCurrentRow(selectedRow() + 1);
    showEditor();
    updateButtons();
}

void MacrosPage::deleteMacro() {
    const int row = selectedRow();
    if (row < 0)
        return;
    if (QMessageBox::question(this, QStringLiteral("Delete macro"),
                              QStringLiteral("Delete \"%1\"?").arg(m_macros[row].name)) != QMessageBox::Yes)
        return;
    m_macros.removeAt(row);
    save();
    fillList();
    m_list->setCurrentRow(qMin(row, m_macros.size() - 1));
    showEditor();
    updateButtons();
}

void MacrosPage::nameEdited() {
    MacroDef *m = current();
    if (m_filling || !m)
        return;
    m->name = m_name->text();
    save();
    fillList();
}

void MacrosPage::triggerChanged() {
    MacroDef *m = current();
    if (m_filling || !m)
        return;
    m->triggerEvdev = m_triggerKey->currentData().toInt();
    save();
    fillList();
}

void MacrosPage::switchesChanged() {
    MacroDef *m = current();
    if (m_filling || !m)
        return;
    m->enabled = m_enabled->isChecked();
    m->triggerCtrl = m_tCtrl->isChecked();
    m->triggerShift = m_tShift->isChecked();
    m->triggerAlt = m_tAlt->isChecked();
    m->swallow = m_swallow->isChecked();
    m->repeat = m_repeat->isChecked();
    save();
    fillList();
}

void MacrosPage::addStep() {
    MacroDef *m = current();
    if (!m)
        return;
    MacroStep seed;
    seed.kind = StepKind::Tap;
    seed.evdev = keyEvdev(QStringLiteral("Space"));
    StepDialog dialog(this, seed);
    if (dialog.exec() != QDialog::Accepted)
        return;
    m->steps.append(dialog.step());
    save();
    fillSteps();
    m_steps->setCurrentRow(m->steps.size() - 1);
    updateButtons();
}

void MacrosPage::editStep() {
    MacroDef *m = current();
    const int row = m_steps->currentRow();
    if (!m || row < 0 || row >= m->steps.size())
        return;
    StepDialog dialog(this, m->steps[row]);
    if (dialog.exec() != QDialog::Accepted)
        return;
    m->steps[row] = dialog.step();
    save();
    fillSteps();
    m_steps->setCurrentRow(row);
}

void MacrosPage::moveStep(int delta) {
    MacroDef *m = current();
    const int row = m_steps->currentRow();
    if (!m || row < 0)
        return;
    const int to = row + delta;
    if (to < 0 || to >= m->steps.size())
        return;
    m->steps.move(row, to);
    save();
    fillSteps();
    m_steps->setCurrentRow(to);
    updateButtons();
}

void MacrosPage::removeStep() {
    MacroDef *m = current();
    const int row = m_steps->currentRow();
    if (!m || row < 0 || row >= m->steps.size())
        return;
    m->steps.removeAt(row);
    save();
    fillSteps();
    m_steps->setCurrentRow(qMin(row, m->steps.size() - 1));
    updateButtons();
}

void MacrosPage::setMaster(bool on) {
    if (m_filling)
        return;
    QString error;
    // On is the default, so it removes the line; off writes 0.
    if (!editSetting(kMacrosSetting, on ? std::nullopt : std::optional<QString>(QStringLiteral("0")), &error))
        QMessageBox::warning(this, QStringLiteral("Macros"), error);
}

void MacrosPage::updateButtons() {
    const bool have = current() != nullptr && !m_broken;
    m_dup->setEnabled(have);
    m_del->setEnabled(have);
    m_add->setEnabled(!m_broken);
    const int row = m_steps->currentRow();
    const MacroDef *m = current();
    const int steps = m ? m->steps.size() : 0;
    m_stepEdit->setEnabled(have && row >= 0);
    m_stepDel->setEnabled(have && row >= 0);
    m_stepUp->setEnabled(have && row > 0);
    m_stepDown->setEnabled(have && row >= 0 && row < steps - 1);
}

void MacrosPage::setStatus(const QString &text, bool error) {
    m_status->setObjectName(error ? QStringLiteral("error") : QStringLiteral("note"));
    m_status->style()->unpolish(m_status);
    m_status->style()->polish(m_status);
    setProblem(m_status, text);
}

} // namespace intox
