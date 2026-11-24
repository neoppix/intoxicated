// Intoxicated Settings: the Shaders page. The client's screen-space
// reflections: the switch, the named looks and a fine-tune row per knob, each
// written to settings.conf as it changes. The knobs and the looks come from
// `intoxicated --ssr-looks`, so nothing here keeps a copy of their values.

#include "app.h"
#include "style.h"

#include <QGroupBox>
#include <QGridLayout>
#include <QHBoxLayout>
#include <QLabel>
#include <QLocale>
#include <QMessageBox>
#include <QPushButton>
#include <QSlider>
#include <QVBoxLayout>

#include <cmath>

namespace intox {

namespace {

struct KnobText {
    const char *name;
    const char *title;
    const char *description;
};

// What each knob is called on the page. A knob the client adds later gets
// its name as a title until it is listed here.
const KnobText kKnobText[] = {
    {"intensity", "Intensity", "How much of the reflection is blended in"},
    {"roughness", "Roughness", "Blur of the reflection, more the further away; 0 is still slightly soft"},
    {"fresnel", "Fresnel", "0 the same at every angle like a mirror, 1 strong only at grazing angles"},
    {"up_only", "Floors only", "Only surfaces that face up reflect; off for walls too"},
    {"up_threshold", "Floor threshold", "How flat a surface must be to count as facing up"},
    {"max_distance", "Ray length", "The longest ray, in multiples of the surface's distance; 0 for no limit"},
    {"edge_fade", "Edge fade", "Fade towards the screen edges, as a fraction of the smaller side"},
    {"darken", "Darken", "How much darker wet surfaces are"},
    {"tint", "Tint", "The colour wet surfaces are multiplied by (red, green, blue)"},
    {"steps", "Steps", "Steps along each ray: more is finer and costs more GPU time"},
    {"thickness", "Thickness", "How far behind a surface a ray still hits it, relative to its distance"},
    {"fov", "Field of view", "The camera's vertical field of view the reflections assume; Roblox's is 70"},
    {"keep_colour", "Keep colour", "0 the colours it reflects, 1 their brightness in the surface's own hue"},
};

KnobText knobText(const QString &name) {
    for (const KnobText &t : kKnobText) {
        if (name == QLatin1String(t.name))
            return t;
    }
    return {nullptr, nullptr, nullptr};
}

QString knobTitle(const QString &name) {
    const KnobText t = knobText(name);
    return t.title ? QString::fromLatin1(t.title) : ssrLookTitle(name);
}

// The slider's step: whole numbers for a whole knob, else a power of ten
// that gives about 100 to 200 positions over the range (1 for a range over
// 200, so a long ray can still be set to a few multiples).
double sliderStep(const SsrKnob &knob) {
    if (knob.kind == SsrKind::Whole)
        return 1.0;
    const double range = knob.max - knob.min;
    if (range > 200)
        return 1.0;
    return std::pow(10.0, std::ceil(std::log10(range / 200.0)));
}

int decimalsOf(double step) { return step >= 1.0 ? 0 : int(std::lround(-std::log10(step))); }

// V rounded to STEP's decimals, so 0.3 rather than 0.30000000000000004.
double onStep(double v, double step) { return QString::number(v, 'f', decimalsOf(step)).toDouble(); }

QGroupBox *group(QVBoxLayout *page, const QString &title, QVBoxLayout **rows) {
    Section s = section(title);
    auto *v = new QVBoxLayout(s.body);
    v->setContentsMargins(8, 2, 8, 4);
    v->setSpacing(0);
    *rows = v;
    page->addWidget(s.box);
    page->addSpacing(8);
    return s.box;
}

} // namespace

ShadersPage::ShadersPage(AppState *state) : m_state(state) {
    QVBoxLayout *b = pageLayout(this, QStringLiteral("Shaders"),
                                QStringLiteral("Effects the client draws into the game itself, kept in settings.conf "
                                               "for every profile. Changes apply the next time Intoxicated starts."));
    QString error;
    m_table = readSsrTable(&error);

    QVBoxLayout *rows = nullptr;
    group(b, QStringLiteral("Reflections (SSR)"), &rows);
    m_on = new Switch;
    rows->addWidget(settingRow(QStringLiteral("Screen-space reflections"),
                               QStringLiteral("Real reflections from the game's own depth, for about 0.5 ms of GPU "
                                              "time a frame. Only what is on screen can be reflected."),
                               m_on));
    connect(m_on, &QCheckBox::clicked, this, [this](bool on) {
        edit([on](SettingsConf &conf) {
            setSsrOn(conf, on);
            return true;
        });
    });
    m_tableError = new QLabel;
    m_tableError->setObjectName(QStringLiteral("error"));
    m_tableError->setWordWrap(true);
    m_tableError->setContentsMargins(6, 0, 6, 6);
    setProblem(m_tableError, m_table ? QString()
                                     : QStringLiteral("The looks could not be read, so only the switch is offered: %1")
                                           .arg(error));
    rows->addWidget(m_tableError);

    // The look: one segment per named look, as the Quality presets.
    Section look = section(QStringLiteral("Look"));
    m_lookBox = look.box;
    auto *lv = new QVBoxLayout(look.body);
    lv->setContentsMargins(12, 10, 12, 10);
    lv->setSpacing(8);
    auto *row = new QHBoxLayout;
    row->setSpacing(6);
    if (m_table) {
        for (int i = 0; i < m_table->looks.size(); ++i) {
            auto *btn = new QPushButton(ssrLookTitle(m_table->looks[i].name));
            btn->setObjectName(QStringLiteral("segment"));
            btn->setCheckable(true);
            btn->setCursor(Qt::PointingHandCursor);
            btn->setFocusPolicy(Qt::NoFocus);
            btn->setMinimumHeight(34);
            row->addWidget(btn, 1);
            m_lookButtons.append(btn);
            connect(btn, &QPushButton::clicked, this, [this, i] { lookChosen(i); });
        }
    }
    lv->addLayout(row);
    m_lookDesc = new QLabel;
    m_lookDesc->setObjectName(QStringLiteral("note"));
    m_lookDesc->setWordWrap(true);
    lv->addWidget(m_lookDesc);
    b->addWidget(look.box);
    b->addSpacing(8);

    m_tuneBox = group(b, QStringLiteral("Fine tune"), &rows);
    auto *tuneNote = new QLabel(QStringLiteral("A change here overrides the look for that one setting; Reset puts "
                                               "the look's value back. Picking a look clears them all."));
    tuneNote->setObjectName(QStringLiteral("note"));
    tuneNote->setWordWrap(true);
    tuneNote->setContentsMargins(6, 8, 6, 4);
    rows->addWidget(tuneNote);
    m_problems = new QLabel;
    m_problems->setObjectName(QStringLiteral("error"));
    m_problems->setWordWrap(true);
    m_problems->setContentsMargins(6, 0, 6, 4);
    m_problems->hide();
    rows->addWidget(m_problems);
    if (m_table)
        buildFineTune(rows);
    b->addStretch(1);

    connect(state, &AppState::settingsChanged, this, &ShadersPage::reload);
}

void ShadersPage::buildFineTune(QVBoxLayout *rows) {
    for (int r = 0; r < m_table->knobs.size(); ++r) {
        const SsrKnob &knob = m_table->knobs[r];
        KnobRow kr;
        kr.name = knob.name;
        kr.step = sliderStep(knob);

        auto *control = new QWidget;
        auto *h = new QHBoxLayout(control);
        h->setContentsMargins(0, 0, 0, 0);
        h->setSpacing(10);
        if (knob.kind == SsrKind::Switch) {
            kr.toggle = new Switch;
            h->addWidget(kr.toggle, 0, Qt::AlignVCenter);
            kr.shown = new QLabel;
            kr.shown->setObjectName(QStringLiteral("knobValue"));
            kr.shown->setFixedWidth(52);
            kr.shown->setAlignment(Qt::AlignRight | Qt::AlignVCenter);
            h->insertWidget(0, kr.shown);
            h->insertStretch(0, 1);
            connect(kr.toggle, &QCheckBox::clicked, this, [this, r] { knobMoved(r); });
        } else {
            auto *grid = new QGridLayout;
            grid->setContentsMargins(0, 0, 0, 0);
            grid->setHorizontalSpacing(8);
            grid->setVerticalSpacing(2);
            const int parts = knob.kind == SsrKind::Rgb ? 3 : 1;
            static const char *channels[] = {"R", "G", "B"};
            for (int i = 0; i < parts; ++i) {
                auto *slider = new QSlider(Qt::Horizontal);
                slider->setCursor(Qt::PointingHandCursor);
                slider->setFocusPolicy(Qt::NoFocus);
                slider->setFixedWidth(parts == 3 ? 180 : 200);
                slider->setRange(int(std::ceil(knob.min / kr.step - 1e-9)), int(std::floor(knob.max / kr.step + 1e-9)));
                slider->setPageStep(qMax(1, (slider->maximum() - slider->minimum()) / 10));
                auto *value = new QLabel;
                value->setObjectName(QStringLiteral("knobValue"));
                value->setFixedWidth(52);
                value->setAlignment(Qt::AlignRight | Qt::AlignVCenter);
                if (parts == 3) {
                    auto *channel = new QLabel(QString::fromLatin1(channels[i]));
                    channel->setObjectName(QStringLiteral("note"));
                    grid->addWidget(channel, i, 0);
                }
                grid->addWidget(value, i, 1);
                grid->addWidget(slider, i, 2);
                kr.sliders.append(slider);
                kr.values.append(value);
                const double step = kr.step;
                connect(slider, &QSlider::valueChanged, this, [this, r, slider, value, step](int pos) {
                    value->setText(shortestFloat(float(onStep(pos * step, step))));
                    // A drag writes once, when it is let go; a click on the
                    // groove writes at once.
                    if (!m_filling && !slider->isSliderDown())
                        knobMoved(r);
                });
                connect(slider, &QSlider::sliderReleased, this, [this, r] { knobMoved(r); });
            }
            h->addLayout(grid);
        }
        kr.reset = new QPushButton(QStringLiteral("Reset"));
        kr.reset->setObjectName(QStringLiteral("resetKnob"));
        kr.reset->setCursor(Qt::PointingHandCursor);
        kr.reset->setFocusPolicy(Qt::NoFocus);
        h->addWidget(kr.reset, 0, Qt::AlignVCenter);
        const QString name = knob.name;
        connect(kr.reset, &QPushButton::clicked, this, [this, name] {
            edit([this, name](SettingsConf &conf) {
                resetSsrKnob(conf, *m_table->knob(name));
                return true;
            });
        });

        const KnobText text = knobText(knob.name);
        QWidget *line = settingRow(knobTitle(knob.name), text.description ? QString::fromLatin1(text.description) : QString(),
                                   control);
        line->setToolTip(knob.env);
        if (r > 0)
            rows->addWidget(separator());
        rows->addWidget(line);
        m_rows.append(kr);
    }
}

void ShadersPage::showEvent(QShowEvent *event) {
    QWidget::showEvent(event);
    reload();
}

void ShadersPage::showSliders(const KnobRow &row, const SsrValue &value) {
    for (int i = 0; i < row.sliders.size() && i < value.size(); ++i) {
        row.sliders[i]->setValue(int(std::lround(value[i] / row.step)));
        // The value itself, which a hand-written one may put between two
        // slider positions.
        row.values[i]->setText(shortestFloat(float(value[i])));
    }
}

void ShadersPage::reload() {
    m_filling = true;
    SettingsConf conf;
    QString error;
    if (!conf.load(settingsConfPath(), &error))
        QMessageBox::warning(this, QStringLiteral("Shaders"), error);
    const bool on = ssrOn(conf);
    m_on->setChecked(on);
    m_lookBox->setEnabled(on && m_table);
    m_tuneBox->setEnabled(on && m_table);
    if (!m_table) {
        m_lookDesc->setText(QStringLiteral("Not available until the looks can be read."));
        m_filling = false;
        return;
    }

    QString unknown;
    const QString current = ssrLook(*m_table, conf, &unknown);
    for (int i = 0; i < m_lookButtons.size(); ++i)
        m_lookButtons[i]->setChecked(unknown.isEmpty() && m_table->looks[i].name == current);
    if (!unknown.isEmpty()) {
        m_lookDesc->setText(QStringLiteral("%1=%2 names no look, so the client starts from %3. Pick one to fix it.")
                                .arg(kSsrLookSetting, unknown, ssrLookTitle(m_table->defaultLook)));
    } else {
        const QStringList tuned = ssrOverrides(*m_table, conf);
        QString text = m_table->look(current)->description;
        if (!tuned.isEmpty())
            text += QStringLiteral(" Fine tuned: %1.").arg(tuned.size() == 1 ? QStringLiteral("1 setting")
                                                                            : QStringLiteral("%1 settings").arg(tuned.size()));
        m_lookDesc->setText(text);
    }

    QStringList problems;
    for (const KnobRow &row : m_rows) {
        const SsrKnob &knob = *m_table->knob(row.name);
        const auto set = ssrOverride(conf, knob);
        const bool usable = set && parseSsrValue(knob, *set);
        const SsrValue value = effectiveSsrValue(*m_table, conf, knob);
        if (row.toggle) {
            row.toggle->setChecked(value.value(0) != 0.0);
            row.shown->setText(value.value(0) != 0.0 ? QStringLiteral("On") : QStringLiteral("Off"));
        } else {
            showSliders(row, value);
        }
        QList<QLabel *> labels = row.values;
        if (row.shown)
            labels.append(row.shown);
        for (QLabel *label : labels) {
            label->setProperty("set", usable);
            label->style()->unpolish(label);
            label->style()->polish(label);
        }
        row.reset->setEnabled(set.has_value());
        row.reset->setToolTip(set ? QStringLiteral("Remove %1=%2").arg(knob.env, *set)
                                  : QStringLiteral("At the look's value"));
        if (set && !usable)
            problems.append(QStringLiteral("%1=%2 is not a value the client takes; it uses %3.")
                                .arg(knob.env, *set, formatSsrValue(knob, value)));
    }
    m_problems->setText(problems.join(QLatin1Char('\n')));
    m_problems->setVisible(!problems.isEmpty());
    m_filling = false;
}

void ShadersPage::edit(const std::function<bool(SettingsConf &)> &change) {
    if (m_filling)
        return;
    SettingsConf conf;
    QString error;
    bool ok = conf.load(settingsConfPath(), &error);
    if (ok && !change(conf)) {
        ok = false;
        error = QStringLiteral("That value cannot be written.");
    }
    if (ok)
        ok = conf.save(&error);
    if (!ok)
        QMessageBox::warning(this, QStringLiteral("Shaders"), error);
    reload();
}

void ShadersPage::lookChosen(int index) {
    const SsrLook look = m_table->looks.at(index);
    SettingsConf conf;
    conf.load(settingsConfPath());
    const QStringList tuned = ssrOverrides(*m_table, conf);
    if (!tuned.isEmpty()) {
        QStringList titles;
        for (const QString &name : tuned)
            titles.append(knobTitle(name));
        const auto answer = QMessageBox::question(
            this, ssrLookTitle(look.name),
            QStringLiteral("Use the %1 look? Your fine tuning of %2 is removed.")
                .arg(ssrLookTitle(look.name), titles.join(QStringLiteral(", "))),
            QMessageBox::Yes | QMessageBox::Cancel, QMessageBox::Cancel);
        if (answer != QMessageBox::Yes) {
            reload();
            return;
        }
    }
    edit([this, look](SettingsConf &c) { return applySsrLook(*m_table, c, look.name); });
}

void ShadersPage::knobMoved(int row) {
    if (m_filling)
        return;
    const KnobRow &kr = m_rows.at(row);
    const SsrKnob &knob = *m_table->knob(kr.name);
    SsrValue value;
    if (kr.toggle) {
        value.append(kr.toggle->isChecked() ? 1.0 : 0.0);
    } else {
        for (QSlider *s : kr.sliders)
            value.append(qBound(knob.min, s->value() * kr.step, knob.max));
    }
    for (double &v : value)
        v = onStep(v, kr.step);
    // A handle grabbed and let go where it was is no change.
    SettingsConf conf;
    conf.load(settingsConfPath());
    if (!ssrOverride(conf, knob) &&
        formatSsrValue(knob, value) == formatSsrValue(knob, effectiveSsrValue(*m_table, conf, knob)))
        return;
    edit([&knob, value](SettingsConf &conf) { return setSsrKnob(conf, knob, value); });
}

} // namespace intox
