// Intoxicated Settings: the Shaders page. The effects the client draws into
// the game before the HUD (screen-space reflections, depth of field, bloom,
// lens, sharpen): at the top the whole-page modes and the switch for the F8
// hotkey that turns them all off and back on in game, then one group each:
// the switch, the named looks and a fine-tune row per knob, each written to
// settings.conf as it changes; and, last, the experimental ray-traced
// reflections, a switch behind a confirmation, shown only when the client
// was built with them. The effects, their knobs, their looks and the modes
// come from `intoxicated --shader-looks`, so nothing here keeps a copy of
// their values.

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
    const char *effect;
    const char *name;
    const char *title;
    const char *description;
};

// What each knob is called on the page. A knob the client adds later gets
// its name as a title until it is listed here.
const KnobText kKnobText[] = {
    {"ssr", "intensity", "Intensity", "How much of the reflection is blended in"},
    {"ssr", "roughness", "Roughness", "Blur of the reflection, more the further away; 0 is still slightly soft"},
    {"ssr", "fresnel", "Fresnel", "0 the same at every angle like a mirror, 1 strong only at grazing angles"},
    {"ssr", "up_only", "Floors only", "Only surfaces that face up reflect; off for walls too"},
    {"ssr", "up_threshold", "Floor threshold", "How flat a surface must be to count as facing up"},
    {"ssr", "max_distance", "Ray length", "The longest ray, in multiples of the surface's distance; 0 for no limit"},
    {"ssr", "edge_fade", "Edge fade", "Fade towards the screen edges, as a fraction of the smaller side"},
    {"ssr", "darken", "Darken", "How much darker wet surfaces are"},
    {"ssr", "tint", "Tint", "The colour wet surfaces are multiplied by (red, green, blue)"},
    {"ssr", "steps", "Steps", "Steps along each ray: more is finer and costs more GPU time"},
    {"ssr", "thickness", "Thickness", "How far behind a surface a ray still hits it, relative to its distance"},
    {"ssr", "fov", "Field of view", "The camera's vertical field of view the reflections assume; Roblox's is 70"},
    {"ssr", "keep_colour", "Keep colour", "0 the colours it reflects, 1 their brightness in the surface's own hue"},
    {"dof", "strength", "Strength", "The largest blur, as a radius in pixels at 1080p"},
    {"dof", "focus_mode", "Focus", "Where the focus is: a fixed distance, your character at the screen centre, or what lies past it"},
    {"dof", "focus_distance", "Focus distance", "Where the focus is with a fixed distance, in studs"},
    {"dof", "focus_range", "Near range", "How far in front of the focus stays sharp with near blur, relative to its distance"},
    {"dof", "far_range", "Far range", "How far behind the focus stays sharp, relative to its distance"},
    {"dof", "near_blur", "Near blur", "Things nearer than the focus blur too"},
    {"dof", "focus_time", "Focus time", "Seconds the autofocus takes to settle on a new distance"},
    {"dof", "samples", "Samples", "Taps of the blur: more is smoother and costs more GPU time"},
    {"bloom", "intensity", "Intensity", "How strong the glow is"},
    {"bloom", "threshold", "Threshold", "How bright a pixel must be to glow, 0 to 1"},
    {"bloom", "knee", "Softness", "How gently the glow fades in around the threshold"},
    {"bloom", "radius", "Radius", "0 a tight glow around each light, 1 a wide haze"},
    {"bloom", "tint", "Tint", "The colour of the glow (red, green, blue)"},
    {"lens", "aberration", "Colour split", "How far red and blue part in the corners, in pixels at 1080p; none at the centre"},
    {"lens", "vignette", "Vignette", "How much darker the corners are"},
    {"lens", "halo", "Rainbow halo", "A rainbow fringe round the brightest areas, from the bloom's glow"},
    {"sharpen", "strength", "Strength", "How strongly texture detail is raised"},
    {"sharpen", "limit", "Limit", "The most any pixel may change, 0 to 1"},
    {"sharpen", "depth_aware", "Depth aware", "Not across depth edges, so outlines get no halo"},
};

// The switch row of each effect, and what is offered when the client cannot
// be asked for its effects: only the switches, by their variables.
struct EffectText {
    const char *name;
    const char *title;
    const char *switchTitle;
    const char *switchEnv;
};

const EffectText kEffectText[] = {
    {"ssr", "Reflections (SSR)", "Screen-space reflections", "INTOXICATED_SSR"},
    {"dof", "Depth of field", "Depth of field", "INTOXICATED_DOF"},
    {"bloom", "Bloom", "Bloom", "INTOXICATED_BLOOM"},
    {"lens", "Lens", "Lens effects", "INTOXICATED_LENS"},
    {"sharpen", "Sharpen", "Sharpening", "INTOXICATED_SHARPEN"},
};

KnobText knobText(const QString &effect, const QString &name) {
    for (const KnobText &t : kKnobText) {
        if (effect == QLatin1String(t.effect) && name == QLatin1String(t.name))
            return t;
    }
    return {nullptr, nullptr, nullptr, nullptr};
}

QString knobTitle(const QString &effect, const QString &name) {
    const KnobText t = knobText(effect, name);
    return t.title ? QString::fromLatin1(t.title) : ssrLookTitle(name);
}

QString switchTitle(const SsrTable &table) {
    for (const EffectText &t : kEffectText) {
        if (table.name == QLatin1String(t.name))
            return QString::fromLatin1(t.switchTitle);
    }
    return table.title;
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

QLabel *note(const QString &text) {
    auto *l = new QLabel(text);
    l->setObjectName(QStringLiteral("note"));
    l->setWordWrap(true);
    return l;
}

QLabel *problemLabel() {
    auto *l = new QLabel;
    l->setObjectName(QStringLiteral("error"));
    l->setWordWrap(true);
    l->setContentsMargins(14, 0, 14, 6);
    l->hide();
    return l;
}

} // namespace

ShadersPage::ShadersPage(AppState *state) : m_state(state) {
    QVBoxLayout *b = pageLayout(this, QStringLiteral("Shaders"),
                                QStringLiteral("Effects the client draws into the game itself, under the HUD, kept in "
                                               "settings.conf for every profile. Changes apply the next time "
                                               "Intoxicated starts."));
    QString error;
    const auto tables = readShaderTables(&error, &m_modes, &m_unavailable, &m_rtBuilt);
    if (m_unavailable) {
        // A client built without them (install.sh --no-effects): nothing to
        // set, so the page only says how to add them.
        Section s = section(QStringLiteral("Not installed"));
        auto *v = new QVBoxLayout(s.body);
        v->setContentsMargins(14, 10, 14, 10);
        v->setSpacing(6);
        v->addWidget(note(QStringLiteral("This Intoxicated was installed without the visual effects (reflections, "
                                         "depth of field, bloom, lens and sharpening). To add them, run %1 in a "
                                         "terminal, or the installer with --effects. They stay off until you turn "
                                         "them on here; --no-effects takes them out again.")
                              .arg(kAddEffectsCommand)));
        auto *command = new QLabel(kAddEffectsCommand);
        command->setObjectName(QStringLiteral("rowTitle"));
        command->setTextInteractionFlags(Qt::TextSelectableByMouse);
        v->addWidget(command);
        b->addWidget(s.box);
        b->addStretch(1);
        m_tableError = nullptr;
        return;
    }
    m_tableError = problemLabel();
    setProblem(m_tableError, tables ? QString()
                                    : QStringLiteral("The effects could not be read, so only their switches are "
                                                     "offered: %1")
                                          .arg(error));
    b->addWidget(m_tableError);
    if (tables) {
        for (const SsrTable &t : *tables) {
            EffectUi fx;
            fx.table = t;
            fx.known = true;
            m_effects.append(fx);
        }
    } else {
        for (const EffectText &t : kEffectText) {
            EffectUi fx;
            fx.table.name = QString::fromLatin1(t.name);
            fx.table.title = QString::fromLatin1(t.title);
            fx.table.switchEnv = QString::fromLatin1(t.switchEnv);
            fx.table.lookEnv = fx.table.switchEnv + QStringLiteral("_LOOK");
            m_effects.append(fx);
        }
    }
    if (!m_modes.isEmpty()) {
        Section s = section(QStringLiteral("Mode"));
        auto *v = new QVBoxLayout(s.body);
        v->setContentsMargins(14, 10, 14, 10);
        v->setSpacing(6);
        auto *row = new QHBoxLayout;
        row->setSpacing(6);
        for (int i = 0; i <= m_modes.size(); ++i) {
            const bool custom = i == m_modes.size();
            auto *btn = new QPushButton(custom ? QStringLiteral("Custom") : m_modes[i].title);
            btn->setObjectName(QStringLiteral("segment"));
            btn->setCheckable(true);
            btn->setCursor(Qt::PointingHandCursor);
            btn->setFocusPolicy(Qt::NoFocus);
            btn->setMinimumHeight(30);
            btn->setToolTip(custom ? QStringLiteral("The effects below as you set them")
                                   : QStringLiteral("%1=%2").arg(kShaderModeSetting, m_modes[i].name));
            row->addWidget(btn, 1);
            m_modeButtons.append(btn);
            // Custom is where hand changes land; it is not picked.
            if (custom)
                connect(btn, &QPushButton::clicked, this, [this] { reload(); });
            else
                connect(btn, &QPushButton::clicked, this, [this, i] { modeChosen(i); });
        }
        linkSegments(row, m_modeButtons);
        v->addLayout(row);
        m_modeDesc = note(QString());
        v->addWidget(m_modeDesc);
        b->addWidget(s.box);
        b->addSpacing(8);
    }
    {
        // The effects hotkey: its own line, not an effect's, so a mode line
        // is left as it is when it is written.
        Section s = section(QStringLiteral("In game"));
        auto *v = new QVBoxLayout(s.body);
        v->setContentsMargins(0, 0, 0, 0);
        v->setSpacing(0);
        m_hotkey = new Switch;
        m_hotkey->setToolTip(kEffectsHotkeySetting);
        v->addWidget(settingRow(QStringLiteral("F8 turns all effects on and off in game"),
                                QStringLiteral("While this is on, Roblox does not see F8. "
                                               "Typing in chat is not affected. Off by default; on writes "
                                               "INTOXICATED_EFFECTS_HOTKEY=1."),
                                m_hotkey));
        b->addWidget(s.box);
        b->addSpacing(8);
        connect(m_hotkey, &QCheckBox::clicked, this, [this](bool on) {
            if (m_filling)
                return;
            SettingsConf conf;
            QString error;
            bool ok = conf.load(settingsConfPath(), &error);
            if (ok) {
                setEffectsHotkeyOn(conf, on);
                ok = conf.save(&error);
            }
            if (!ok)
                QMessageBox::warning(this, QStringLiteral("Shaders"), error);
            reload();
        });
    }
    for (int i = 0; i < m_effects.size(); ++i)
        buildEffect(b, i);
    buildRayTracing(b);
    b->addStretch(1);

    connect(state, &AppState::settingsChanged, this, &ShadersPage::reload);
}

void ShadersPage::buildEffect(QVBoxLayout *page, int index) {
    EffectUi &fx = m_effects[index];
    Section s = section(fx.table.title);
    auto *v = new QVBoxLayout(s.body);
    v->setContentsMargins(0, 0, 0, 0);
    v->setSpacing(0);
    page->addWidget(s.box);
    page->addSpacing(8);

    fx.on = new Switch;
    v->addWidget(settingRow(switchTitle(fx.table), fx.table.summary, fx.on));
    fx.on->setToolTip(fx.table.switchEnv);
    connect(fx.on, &QCheckBox::clicked, this, [this, index](bool on) {
        const QString env = m_effects[index].table.switchEnv;
        edit([env, on](SettingsConf &conf) {
            setEffectOn(env, conf, on);
            return true;
        });
    });
    if (!fx.known)
        return;

    fx.details = new QWidget;
    auto *d = new QVBoxLayout(fx.details);
    d->setContentsMargins(0, 0, 0, 0);
    d->setSpacing(0);
    v->addWidget(fx.details);

    // The look: one segment per named look, as the Quality presets.
    if (fx.table.looks.size() > 1) {
        d->addWidget(separator());
        auto *look = new QWidget;
        auto *lv = new QVBoxLayout(look);
        lv->setContentsMargins(14, 8, 14, 10);
        lv->setSpacing(6);
        auto *title = new QLabel(QStringLiteral("Look"));
        title->setObjectName(QStringLiteral("rowTitle"));
        lv->addWidget(title);
        auto *row = new QHBoxLayout;
        row->setSpacing(6);
        for (int i = 0; i < fx.table.looks.size(); ++i) {
            auto *btn = new QPushButton(ssrLookTitle(fx.table.looks[i].name));
            btn->setObjectName(QStringLiteral("segment"));
            btn->setCheckable(true);
            btn->setCursor(Qt::PointingHandCursor);
            btn->setFocusPolicy(Qt::NoFocus);
            btn->setMinimumHeight(30);
            row->addWidget(btn, 1);
            fx.lookButtons.append(btn);
            connect(btn, &QPushButton::clicked, this, [this, index, i] { lookChosen(index, i); });
        }
        linkSegments(row, fx.lookButtons);
        lv->addLayout(row);
        fx.lookDesc = note(QString());
        lv->addWidget(fx.lookDesc);
        d->addWidget(look);
    }

    d->addWidget(separator());
    fx.tuneButton = new QPushButton;
    fx.tuneButton->setObjectName(QStringLiteral("resetKnob"));
    fx.tuneButton->setCheckable(true);
    fx.tuneButton->setCursor(Qt::PointingHandCursor);
    fx.tuneButton->setFocusPolicy(Qt::NoFocus);
    d->addWidget(settingRow(QStringLiteral("Fine tune"),
                            QStringLiteral("%1 settings. A change overrides the look for that one setting; Reset puts "
                                           "the look's value back. Picking a look clears them all.")
                                .arg(fx.table.knobs.size()),
                            fx.tuneButton));
    fx.problems = problemLabel();
    d->addWidget(fx.problems);
    fx.tune = new QWidget;
    auto *t = new QVBoxLayout(fx.tune);
    t->setContentsMargins(0, 0, 0, 0);
    t->setSpacing(0);
    t->addWidget(separator());
    buildFineTune(t, index);
    d->addWidget(fx.tune);
    connect(fx.tuneButton, &QPushButton::toggled, this, [this, index](bool shown) {
        EffectUi &e = m_effects[index];
        e.tune->setVisible(shown);
        e.tuneButton->setText(shown ? QStringLiteral("Hide") : QStringLiteral("Show"));
    });
    SettingsConf conf;
    conf.load(settingsConfPath());
    const bool tuned = !ssrOverrides(fx.table, conf).isEmpty();
    fx.tuneButton->setChecked(tuned);
    fx.tune->setVisible(tuned);
    fx.tuneButton->setText(tuned ? QStringLiteral("Hide") : QStringLiteral("Show"));
}

void ShadersPage::buildRayTracing(QVBoxLayout *page) {
    Section s = section(QStringLiteral("Ray-traced reflections (experimental)"));
    auto *v = new QVBoxLayout(s.body);
    v->setContentsMargins(8, 6, 8, 4);
    v->setSpacing(0);
    page->addWidget(s.box);
    page->addSpacing(8);
    if (!m_rtBuilt) {
        // A client built without it (the installer's ray tracing question
        // answered no): nothing to switch, only how to add it.
        auto *n = note(QStringLiteral("Not installed. This Intoxicated was built without the experimental ray "
                                      "tracing. To add it, run %1 in a terminal (it needs the visual effects); it "
                                      "stays off until you turn it on here.")
                           .arg(kAddRayTracingCommand));
        n->setContentsMargins(6, 8, 6, 8);
        v->addWidget(n);
        return;
    }

    auto *top = new QHBoxLayout;
    top->setContentsMargins(6, 2, 6, 6);
    top->setSpacing(10);
    auto *tag = new QLabel(QStringLiteral("EXPERIMENTAL"));
    tag->setObjectName(QStringLiteral("experimentalTag"));
    top->addWidget(tag, 0, Qt::AlignVCenter);
    top->addWidget(note(QStringLiteral("Opt in, at your own risk. Off by default, and no look or preset turns it on.")),
                   1);
    v->addLayout(top);
    v->addWidget(separator());

    m_rtOn = new Switch;
    m_rtOn->setToolTip(kRtSetting);
    v->addWidget(settingRow(QStringLiteral("Ray-traced reflections"),
                            QStringLiteral("Traces each reflection against the scene's own geometry, so it can show "
                                           "what is off screen or behind the camera."),
                            m_rtOn));
    connect(m_rtOn, &QCheckBox::clicked, this, &ShadersPage::rayTracingClicked);
    v->addWidget(separator());
    QStringList items;
    for (const QString &w : rtWarnings())
        items.append(QStringLiteral("\u2022 ") + w);
    auto *warnings = note(items.join(QLatin1Char('\n')));
    warnings->setContentsMargins(6, 8, 6, 8);
    v->addWidget(warnings);
}

void ShadersPage::rayTracingClicked(bool on) {
    if (on) {
        QStringList items;
        for (const QString &w : rtWarnings())
            items.append(QStringLiteral("\u2022 ") + w);
        QMessageBox box(QMessageBox::Warning, QStringLiteral("Ray-traced reflections"),
                        QStringLiteral("Ray-traced reflections are experimental. Turn them on?"),
                        QMessageBox::Yes | QMessageBox::Cancel, this);
        box.setInformativeText(items.join(QLatin1Char('\n')));
        box.setDefaultButton(QMessageBox::Cancel);
        if (box.exec() != QMessageBox::Yes) {
            reload();
            return;
        }
    }
    edit([on](SettingsConf &conf) {
        setRtOn(conf, on);
        return true;
    });
}

void ShadersPage::buildFineTune(QVBoxLayout *rows, int index) {
    EffectUi &fx = m_effects[index];
    for (int r = 0; r < fx.table.knobs.size(); ++r) {
        const SsrKnob &knob = fx.table.knobs[r];
        KnobRow kr;
        kr.name = knob.name;
        kr.step = sliderStep(knob);

        auto *control = new QWidget;
        auto *h = new QHBoxLayout(control);
        h->setContentsMargins(0, 0, 0, 0);
        h->setSpacing(10);
        const QStringList choices = knobChoices(fx.table.name, knob);
        if (!choices.isEmpty()) {
            auto *row = new QHBoxLayout;
            row->setSpacing(6);
            for (int c = 0; c < choices.size(); ++c) {
                auto *btn = new QPushButton(choices[c]);
                btn->setObjectName(QStringLiteral("segment"));
                btn->setCheckable(true);
                btn->setCursor(Qt::PointingHandCursor);
                btn->setFocusPolicy(Qt::NoFocus);
                btn->setMinimumHeight(28);
                row->addWidget(btn);
                kr.choices.append(btn);
                connect(btn, &QPushButton::clicked, this, [this, index, r, c] {
                    EffectUi &e = m_effects[index];
                    for (int k = 0; k < e.rows[r].choices.size(); ++k)
                        e.rows[r].choices[k]->setChecked(k == c);
                    knobMoved(index, r);
                });
            }
            linkSegments(row, kr.choices);
            h->addLayout(row);
        } else if (knob.kind == SsrKind::Switch) {
            kr.toggle = new Switch;
            h->addWidget(kr.toggle, 0, Qt::AlignVCenter);
            kr.shown = new QLabel;
            kr.shown->setObjectName(QStringLiteral("knobValue"));
            kr.shown->setFixedWidth(52);
            kr.shown->setAlignment(Qt::AlignRight | Qt::AlignVCenter);
            h->insertWidget(0, kr.shown);
            h->insertStretch(0, 1);
            connect(kr.toggle, &QCheckBox::clicked, this, [this, index, r] { knobMoved(index, r); });
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
                connect(slider, &QSlider::valueChanged, this, [this, index, r, slider, value, step](int pos) {
                    value->setText(shortestFloat(float(onStep(pos * step, step))));
                    // A drag writes once, when it is let go; a click on the
                    // groove writes at once.
                    if (!m_filling && !slider->isSliderDown())
                        knobMoved(index, r);
                });
                connect(slider, &QSlider::sliderReleased, this, [this, index, r] { knobMoved(index, r); });
            }
            h->addLayout(grid);
        }
        kr.reset = new QPushButton(QStringLiteral("Reset"));
        kr.reset->setObjectName(QStringLiteral("resetKnob"));
        kr.reset->setCursor(Qt::PointingHandCursor);
        kr.reset->setFocusPolicy(Qt::NoFocus);
        h->addWidget(kr.reset, 0, Qt::AlignVCenter);
        const QString name = knob.name;
        connect(kr.reset, &QPushButton::clicked, this, [this, index, name] {
            edit([this, index, name](SettingsConf &conf) {
                resetSsrKnob(conf, *m_effects[index].table.knob(name));
                return true;
            });
        });

        const KnobText text = knobText(fx.table.name, knob.name);
        QWidget *line = settingRow(knobTitle(fx.table.name, knob.name),
                                   text.description ? QString::fromLatin1(text.description) : QString(), control);
        line->setToolTip(knob.env);
        if (r > 0)
            rows->addWidget(separator());
        rows->addWidget(line);
        fx.rows.append(kr);
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
    if (m_unavailable)
        return;
    m_filling = true;
    SettingsConf conf;
    QString error;
    if (!conf.load(settingsConfPath(), &error))
        QMessageBox::warning(this, QStringLiteral("Shaders"), error);
    // What a hand-written mode line asks for, as the effects' own lines.
    const QList<SsrTable> tables = knownTables();
    expandShaderMode(tables, m_modes, conf);
    for (EffectUi &fx : m_effects)
        reloadEffect(conf, fx);
    if (m_rtOn)
        m_rtOn->setChecked(rtOn(conf));
    if (m_hotkey)
        m_hotkey->setChecked(effectsHotkeyOn(conf));
    if (!m_modeButtons.isEmpty()) {
        const QString current = currentShaderMode(tables, m_modes, conf);
        for (int i = 0; i < m_modeButtons.size(); ++i)
            m_modeButtons[i]->setChecked(i < m_modes.size() ? m_modes[i].name == current : current.isEmpty());
        const ShaderMode *m = findShaderMode(m_modes, current);
        m_modeDesc->setText(m ? m->description
                              : QStringLiteral("The effects below are set by hand. Picking a mode sets every "
                                               "effect's switch and look and clears the fine tuning."));
    }
    m_filling = false;
}

QList<SsrTable> ShadersPage::knownTables() const {
    QList<SsrTable> out;
    for (const EffectUi &fx : m_effects) {
        if (fx.known)
            out.append(fx.table);
    }
    return out;
}

void ShadersPage::reloadEffect(const SettingsConf &conf, EffectUi &fx) {
    const bool on = effectOn(fx.table.switchEnv, conf);
    fx.on->setChecked(on);
    if (!fx.known)
        return;
    fx.details->setEnabled(on);

    QString unknown;
    const QString current = ssrLook(fx.table, conf, &unknown);
    for (int i = 0; i < fx.lookButtons.size(); ++i)
        fx.lookButtons[i]->setChecked(unknown.isEmpty() && fx.table.looks[i].name == current);
    if (fx.lookDesc) {
        if (!unknown.isEmpty()) {
            fx.lookDesc->setText(QStringLiteral("%1=%2 names no look, so the client starts from %3. Pick one to fix it.")
                                     .arg(fx.table.lookEnv, unknown, ssrLookTitle(fx.table.defaultLook)));
        } else {
            const QStringList tuned = ssrOverrides(fx.table, conf);
            QString text = fx.table.look(current)->description;
            if (!tuned.isEmpty())
                text += QStringLiteral(" Fine tuned: %1.")
                            .arg(tuned.size() == 1 ? QStringLiteral("1 setting")
                                                   : QStringLiteral("%1 settings").arg(tuned.size()));
            fx.lookDesc->setText(text);
        }
    }

    QStringList problems;
    for (const KnobRow &row : fx.rows) {
        const SsrKnob &knob = *fx.table.knob(row.name);
        const auto set = ssrOverride(conf, knob);
        const bool usable = set && parseSsrValue(knob, *set);
        const SsrValue value = effectiveSsrValue(fx.table, conf, knob);
        if (!row.choices.isEmpty()) {
            for (int c = 0; c < row.choices.size(); ++c) {
                row.choices[c]->setChecked(qRound(value.value(0) - knob.min) == c);
                row.choices[c]->setProperty("set", usable);
            }
        } else if (row.toggle) {
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
    fx.problems->setText(problems.join(QLatin1Char('\n')));
    fx.problems->setVisible(!problems.isEmpty());
}

void ShadersPage::edit(const std::function<bool(SettingsConf &)> &change) {
    if (m_filling)
        return;
    SettingsConf conf;
    QString error;
    bool ok = conf.load(settingsConfPath(), &error);
    // A mode line is written out as the effects' own lines first, so a
    // change to one effect keeps what the mode set for the others.
    if (ok)
        expandShaderMode(knownTables(), m_modes, conf);
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

void ShadersPage::lookChosen(int effect, int index) {
    const SsrTable &table = m_effects.at(effect).table;
    const SsrLook look = table.looks.at(index);
    SettingsConf conf;
    conf.load(settingsConfPath());
    const QStringList tuned = ssrOverrides(table, conf);
    if (!tuned.isEmpty()) {
        QStringList titles;
        for (const QString &name : tuned)
            titles.append(knobTitle(table.name, name));
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
    edit([this, effect, look](SettingsConf &c) { return applySsrLook(m_effects.at(effect).table, c, look.name); });
}

void ShadersPage::modeChosen(int index) {
    const ShaderMode mode = m_modes.at(index);
    SettingsConf conf;
    conf.load(settingsConfPath());
    expandShaderMode(knownTables(), m_modes, conf);
    QStringList tuned;
    for (const EffectUi &fx : m_effects) {
        if (!fx.known)
            continue;
        for (const QString &name : ssrOverrides(fx.table, conf))
            tuned.append(QStringLiteral("%1 %2").arg(fx.table.title, knobTitle(fx.table.name, name).toLower()));
    }
    if (!tuned.isEmpty()) {
        const auto answer = QMessageBox::question(
            this, mode.title,
            QStringLiteral("Use the %1 mode? It sets every effect's switch and look, and your fine tuning of %2 is "
                           "removed.")
                .arg(mode.title, tuned.join(QStringLiteral(", "))),
            QMessageBox::Yes | QMessageBox::Cancel, QMessageBox::Cancel);
        if (answer != QMessageBox::Yes) {
            reload();
            return;
        }
    }
    edit([this, mode](SettingsConf &c) { return applyShaderMode(knownTables(), m_modes, c, mode.name); });
}

void ShadersPage::knobMoved(int effect, int row) {
    if (m_filling)
        return;
    const EffectUi &fx = m_effects.at(effect);
    const KnobRow &kr = fx.rows.at(row);
    const SsrKnob &knob = *fx.table.knob(kr.name);
    SsrValue value;
    if (!kr.choices.isEmpty()) {
        for (int c = 0; c < kr.choices.size(); ++c) {
            if (kr.choices[c]->isChecked())
                value.append(knob.min + c);
        }
        if (value.isEmpty())
            return;
    } else if (kr.toggle) {
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
        formatSsrValue(knob, value) == formatSsrValue(knob, effectiveSsrValue(fx.table, conf, knob)))
        return;
    edit([&knob, value](SettingsConf &conf) { return setSsrKnob(conf, knob, value); });
}

} // namespace intox
