// Intoxicated Settings: the look. Fusion with a dark palette and the
// Intoxicated red (#E91F26 to #A5071A), as in the approved mockup.

#include "style.h"

#include <QApplication>
#include <QFrame>
#include <QGroupBox>
#include <QHBoxLayout>
#include <QLabel>
#include <QPainter>
#include <QPainterPath>
#include <QPalette>
#include <QScrollArea>
#include <QStyleFactory>
#include <QVBoxLayout>
#include <QWindow>

namespace intox {

static QPalette darkPalette() {
    QPalette pal;
    const QColor window(0x1c, 0x1c, 0x21);
    const QColor text(0xe7, 0xe7, 0xeb);
    const QColor dim(0x80, 0x80, 0x8a);
    pal.setColor(QPalette::Window, window);
    pal.setColor(QPalette::WindowText, text);
    pal.setColor(QPalette::Base, QColor(0x21, 0x21, 0x27));
    pal.setColor(QPalette::AlternateBase, QColor(0x28, 0x28, 0x2f));
    pal.setColor(QPalette::Text, text);
    pal.setColor(QPalette::BrightText, Qt::white);
    pal.setColor(QPalette::Button, QColor(0x2b, 0x2b, 0x32));
    pal.setColor(QPalette::ButtonText, text);
    pal.setColor(QPalette::Light, QColor(0x3c, 0x3c, 0x45));
    pal.setColor(QPalette::Midlight, QColor(0x33, 0x33, 0x3b));
    pal.setColor(QPalette::Mid, QColor(0x26, 0x26, 0x2c));
    pal.setColor(QPalette::Dark, QColor(0x14, 0x14, 0x18));
    pal.setColor(QPalette::Shadow, QColor(0x08, 0x08, 0x0a));
    pal.setColor(QPalette::Highlight, kAccent);
    pal.setColor(QPalette::HighlightedText, Qt::white);
    pal.setColor(QPalette::Inactive, QPalette::Highlight, kAccent);
    pal.setColor(QPalette::Inactive, QPalette::HighlightedText, Qt::white);
    pal.setColor(QPalette::Link, QColor(0xff, 0x5a, 0x60));
    pal.setColor(QPalette::ToolTipBase, QColor(0x2b, 0x2b, 0x32));
    pal.setColor(QPalette::ToolTipText, text);
    pal.setColor(QPalette::PlaceholderText, dim);
    pal.setColor(QPalette::Accent, kAccent);
    pal.setColor(QPalette::Disabled, QPalette::Text, dim);
    pal.setColor(QPalette::Disabled, QPalette::WindowText, dim);
    pal.setColor(QPalette::Disabled, QPalette::ButtonText, dim);
    return pal;
}

static const char *kStyleSheet = R"(
QWidget#root { background: #1c1c21; }
QDialog { background: #1c1c21; }
QDialogButtonBox { dialogbuttonbox-buttons-have-icons: 0; }

QFrame#sidebar {
    background: #141418;
    border: none;
    border-right: 1px solid #2a2a31;
}
QLabel#brand { color: #f3f3f5; font-size: 17px; font-weight: 700; }

QListWidget#nav { background: transparent; border: none; outline: 0; }
QListWidget#nav::item {
    color: #b4b4bd;
    padding-left: 14px;
    border-radius: 7px;
}
QListWidget#nav::item:hover { background: #222228; color: #ececf0; }
QListWidget#nav::item:selected {
    background: qlineargradient(x1:0, y1:0, x2:1, y2:0, stop:0 #E91F26, stop:1 #A5071A);
    color: #ffffff;
}

QScrollArea#page { background: transparent; border: none; }
QWidget#pageBody { background: #1c1c21; }

QLabel#heading { color: #f5f5f7; font-size: 24px; font-weight: 700; }
QLabel#subtitle { color: #9a9aa4; font-size: 13px; }
QLabel#note { color: #9a9aa4; font-size: 12px; }
QLabel#error { color: #ff6b70; font-size: 12px; }
QLabel#status { color: #9a9aa4; font-size: 12px; }
QLabel#aboutName { color: #f5f5f7; font-size: 28px; font-weight: 700; }
QLabel#aboutTag { color: #c8c8cf; font-size: 15px; }

QFrame#banner {
    background: #2a1417;
    border: 1px solid #A5071A;
    border-radius: 10px;
}
QFrame#banner QLabel { color: #f3d6d8; }

QGroupBox {
    background: #212127;
    border: 1px solid #2f2f37;
    border-radius: 10px;
    margin-top: 22px;
    padding: 2px 6px 4px 6px;
}
QGroupBox::title {
    subcontrol-origin: margin;
    subcontrol-position: top left;
    left: 2px;
    top: 0px;
    padding: 0px;
    color: #ececf0;
}

QFrame#rowSep { background: #2c2c33; border: none; max-height: 1px; min-height: 1px; }
QLabel#rowTitle { color: #ececf0; font-size: 13px; font-weight: 600; }
QLabel#rowDesc { color: #8e8e98; font-size: 12px; }

QTableWidget {
    background: #212127;
    alternate-background-color: #27272e;
    border: 1px solid #30303a;
    border-radius: 8px;
    color: #e2e2e7;
    gridline-color: #2c2c33;
    selection-background-color: transparent;
}
QTableWidget::item { padding-left: 7px; border: none; }
QTableWidget#profiles { selection-background-color: #4a1d22; selection-color: #ffffff; }
QTableWidget#profiles::item:selected { background: #4a1d22; color: #ffffff; }
QTableWidget QLineEdit { padding: 0px 5px; border-radius: 0px; }
QHeaderView { background: transparent; }
QHeaderView::section {
    background: #2a2a31;
    color: #9a9aa4;
    border: none;
    border-bottom: 1px solid #34343c;
    padding: 6px 10px;
    font-size: 12px;
    font-weight: 600;
}
QHeaderView::section:first { border-top-left-radius: 7px; }
QHeaderView::section:last { border-top-right-radius: 7px; }

QPushButton {
    background: #2b2b32;
    color: #e7e7eb;
    border: 1px solid #3a3a43;
    border-radius: 6px;
    padding: 6px 16px;
}
QPushButton:hover { background: #34343c; border-color: #4a4a54; }
QPushButton:pressed { background: #24242a; }
QPushButton:disabled { background: #232329; color: #63636b; border-color: #2e2e35; }

QPushButton#deleteBtn {
    background: transparent;
    color: #f0585d;
    border: 1px solid #4d2a2e;
    border-radius: 5px;
    padding: 2px 10px;
    font-size: 12px;
}
QPushButton#deleteBtn:hover { background: #3a1a1e; border-color: #A5071A; color: #ff7a7e; }

QPushButton#addBtn { border-color: #A5071A; color: #ffffff; background: #3a1a1e; }
QPushButton#addBtn:hover { background: #4a1d22; border-color: #E91F26; }
QPushButton#addBtn:disabled { background: #232329; color: #63636b; border-color: #2e2e35; }

QPushButton#linkBtn {
    background: transparent;
    border: none;
    color: #ff5a60;
    padding: 2px 0px;
    text-decoration: underline;
}
QPushButton#linkBtn:hover { color: #ff8a8e; }

QFrame#bottomBar {
    background: #17171b;
    border: none;
    border-top: 1px solid #2a2a31;
}

QComboBox {
    background: #26262c;
    color: #e7e7eb;
    border: 1px solid #3a3a43;
    border-radius: 6px;
    padding: 7px 12px;
    min-width: 170px;
}
QComboBox:hover { border-color: #4a4a54; }
QComboBox:disabled { color: #63636b; border-color: #2e2e35; }
QComboBox::drop-down { border: none; width: 30px; background: transparent; }
QComboBox::down-arrow { image: none; width: 0px; height: 0px; }
QComboBox QAbstractItemView {
    background: #26262c;
    color: #e7e7eb;
    border: 1px solid #3a3a43;
    selection-background-color: #E91F26;
    outline: 0;
}

QLineEdit {
    background: #26262c;
    color: #e7e7eb;
    border: 1px solid #3a3a43;
    border-radius: 6px;
    padding: 6px 10px;
    selection-background-color: #E91F26;
}
QLineEdit:focus { border-color: #6a6a74; }
QLineEdit[invalid="true"] { border-color: #E91F26; }

QPlainTextEdit#log {
    background: #141418;
    color: #d6d6dc;
    border: 1px solid #2f2f37;
    border-radius: 8px;
    padding: 4px;
    selection-background-color: #A5071A;
}

QPushButton#play {
    background: #E91F26;
    color: #ffffff;
    border: none;
    border-radius: 6px;
    padding: 9px 46px;
    font-size: 17px;
    font-weight: 700;
}
QPushButton#play:hover { background: #F03A40; }
QPushButton#play:pressed { background: #A5071A; }

QPushButton#segment {
    background: #232329;
    color: #d6d6dc;
    border: 1px solid #2f2f37;
    border-radius: 6px;
    padding: 6px 10px;
    font-weight: 600;
}
QPushButton#segment:hover { background: #2b2b32; border-color: #3a3a43; }
QPushButton#segment:checked { background: #E91F26; border-color: #E91F26; color: #ffffff; }
QPushButton#segment:checked:hover { background: #F03A40; border-color: #F03A40; }
QPushButton#segment:disabled { background: #1d1d22; color: #63636b; }

QSlider { min-height: 20px; }
QSlider::groove:horizontal { height: 4px; background: #34343c; border-radius: 2px; }
QSlider::sub-page:horizontal { background: #E91F26; border-radius: 2px; }
QSlider::add-page:horizontal { background: #34343c; border-radius: 2px; }
QSlider::handle:horizontal {
    background: #e7e7eb;
    width: 14px;
    height: 14px;
    margin: -5px 0px;
    border-radius: 7px;
}
QSlider::handle:horizontal:hover { background: #ffffff; }
QSlider::sub-page:horizontal:disabled { background: #44444c; }
QSlider::handle:horizontal:disabled { background: #5a5a62; }

QLabel#knobValue { color: #c8c8cf; font-size: 12px; }
QLabel#knobValue:disabled { color: #63636b; }
QLabel#knobValue[set="true"] { color: #ff5a60; font-weight: 600; }

QPushButton#resetKnob {
    background: transparent;
    color: #b4b4bd;
    border: 1px solid #3a3a43;
    border-radius: 5px;
    padding: 2px 10px;
    font-size: 12px;
}
QPushButton#resetKnob:hover { background: #2b2b32; border-color: #4a4a54; color: #ececf0; }
QPushButton#resetKnob:disabled { color: #4a4a52; border-color: #2a2a30; background: transparent; }

QScrollBar:vertical { background: transparent; width: 10px; margin: 2px; }
QScrollBar::handle:vertical { background: #3a3a43; border-radius: 3px; min-height: 30px; }
QScrollBar::add-line:vertical, QScrollBar::sub-line:vertical { height: 0px; }
QScrollBar::add-page:vertical, QScrollBar::sub-page:vertical { background: transparent; }
QScrollBar:horizontal { background: transparent; height: 10px; margin: 2px; }
QScrollBar::handle:horizontal { background: #3a3a43; border-radius: 3px; min-width: 30px; }
QScrollBar::add-line:horizontal, QScrollBar::sub-line:horizontal { width: 0px; }
QScrollBar::add-page:horizontal, QScrollBar::sub-page:horizontal { background: transparent; }

QToolTip { background: #2b2b32; color: #e7e7eb; border: 1px solid #3a3a43; padding: 4px; }
)";

void applyLook(QApplication &app) {
    app.setStyle(QStyleFactory::create(QStringLiteral("Fusion")));
    app.setPalette(darkPalette());
    QFont font(QStringLiteral("Noto Sans"));
    font.setPixelSize(13);
    app.setFont(font);
    app.setStyleSheet(QString::fromLatin1(kStyleSheet));
}

QPixmap logo(int side, const QWidget *widget) {
    static const QPixmap source(QStringLiteral(":/intoxicated-logo.png"));
    if (source.isNull())
        return QPixmap();
    const qreal dpr = widget ? widget->devicePixelRatioF() : qApp->devicePixelRatio();
    const int px = qRound(side * dpr);
    QPixmap scaled = source.scaled(px, px, Qt::KeepAspectRatio, Qt::SmoothTransformation);
    scaled.setDevicePixelRatio(dpr);
    return scaled;
}

Switch::Switch(QWidget *parent) : QCheckBox(parent) {
    setCursor(Qt::PointingHandCursor);
    setFocusPolicy(Qt::NoFocus);
    setSizePolicy(QSizePolicy::Fixed, QSizePolicy::Fixed);
}

bool Switch::hitButton(const QPoint &pos) const { return rect().contains(pos); }

void Switch::paintEvent(QPaintEvent *) {
    QPainter p(this);
    p.setRenderHint(QPainter::Antialiasing);
    const QRectF track = QRectF(rect()).adjusted(0.5, 0.5, -0.5, -0.5);
    const qreal r = track.height() / 2.0;
    p.setOpacity(isEnabled() ? 1.0 : 0.4);
    if (isChecked()) {
        QLinearGradient g(track.topLeft(), track.topRight());
        g.setColorAt(0.0, kAccentDark);
        g.setColorAt(1.0, kAccent);
        p.setPen(Qt::NoPen);
        p.setBrush(g);
    } else {
        p.setPen(QPen(QColor(0x4a, 0x4a, 0x54), 1.0));
        p.setBrush(QColor(0x2d, 0x2d, 0x34));
    }
    p.drawRoundedRect(track, r, r);

    const qreal pad = 3.0;
    const qreal d = track.height() - 2 * pad;
    const qreal x = isChecked() ? track.right() - pad - d : track.left() + pad;
    const QRectF knob(x, track.top() + pad, d, d);
    p.setPen(Qt::NoPen);
    p.setBrush(QColor(0, 0, 0, 60));
    p.drawEllipse(knob.translated(0, 1));
    p.setBrush(isChecked() ? QColor(Qt::white) : QColor(0xa9, 0xa9, 0xb2));
    p.drawEllipse(knob);
}

Combo::Combo(QWidget *parent) : QComboBox(parent) {
    setFocusPolicy(Qt::NoFocus);
    setCursor(Qt::PointingHandCursor);
    setSizeAdjustPolicy(QComboBox::AdjustToContents);
}

void Combo::paintEvent(QPaintEvent *e) {
    QComboBox::paintEvent(e);
    QPainter p(this);
    p.setRenderHint(QPainter::Antialiasing);
    const QColor colour = isEnabled() ? QColor(0xa9, 0xa9, 0xb2) : QColor(0x55, 0x55, 0x5d);
    p.setPen(QPen(colour, 1.6, Qt::SolidLine, Qt::RoundCap, Qt::RoundJoin));
    const QPointF c(width() - 18.0, height() / 2.0 + 1.0);
    QPainterPath path;
    path.moveTo(c + QPointF(-4.5, -2.5));
    path.lineTo(c + QPointF(0.0, 2.0));
    path.lineTo(c + QPointF(4.5, -2.5));
    p.drawPath(path);
}

Section section(const QString &title) {
    auto *box = new QGroupBox(title);
    QFont f = qApp->font();
    f.setPixelSize(15);
    f.setWeight(QFont::DemiBold);
    box->setFont(f);
    auto *outer = new QVBoxLayout(box);
    outer->setContentsMargins(0, 0, 0, 0);
    auto *body = new QWidget;
    body->setFont(qApp->font());
    outer->addWidget(body);
    return {box, body};
}

QWidget *settingRow(const QString &title, const QString &description, QWidget *control, QLabel **desc) {
    auto *row = new QWidget;
    auto *h = new QHBoxLayout(row);
    h->setContentsMargins(6, 6, 6, 6);
    h->setSpacing(16);

    auto *texts = new QVBoxLayout;
    texts->setSpacing(1);
    auto *t = new QLabel(title);
    t->setObjectName(QStringLiteral("rowTitle"));
    texts->addWidget(t);
    auto *d = new QLabel(description);
    d->setObjectName(QStringLiteral("rowDesc"));
    d->setWordWrap(true);
    d->setVisible(!description.isEmpty());
    texts->addWidget(d);
    if (desc)
        *desc = d;
    h->addLayout(texts, 1);
    if (control) {
        // Combos line up at one width; a longer entry makes its own wider.
        if (qobject_cast<QComboBox *>(control))
            control->setMinimumWidth(240);
        h->addWidget(control, 0, Qt::AlignVCenter);
    }
    return row;
}

void setProblem(QLabel *label, const QString &text) {
    label->setText(text);
    label->setVisible(!text.isEmpty());
}

QFrame *separator() {
    auto *f = new QFrame;
    f->setObjectName(QStringLiteral("rowSep"));
    f->setFrameShape(QFrame::NoFrame);
    f->setFixedHeight(1);
    return f;
}

QVBoxLayout *pageLayout(QWidget *page, const QString &heading, const QString &subtitle, QLabel **subtitleLabel) {
    auto *outer = new QVBoxLayout(page);
    outer->setContentsMargins(0, 0, 0, 0);
    outer->setSpacing(0);

    auto *scroll = new QScrollArea;
    scroll->setObjectName(QStringLiteral("page"));
    scroll->setWidgetResizable(true);
    scroll->setFrameShape(QFrame::NoFrame);
    scroll->setHorizontalScrollBarPolicy(Qt::ScrollBarAlwaysOff);

    // The page is one column, at most kColumnWidth wide and centred, so a
    // window a tiling manager makes very wide does not stretch every row.
    const int kColumnWidth = 1100;
    auto *body = new QWidget;
    body->setObjectName(QStringLiteral("pageBody"));
    auto *row = new QHBoxLayout(body);
    row->setContentsMargins(0, 0, 0, 0);
    row->setSpacing(0);
    auto *column = new QWidget;
    column->setMaximumWidth(kColumnWidth);
    column->setSizePolicy(QSizePolicy::Expanding, QSizePolicy::Preferred);
    row->addStretch(1);
    row->addWidget(column, 1000);
    row->addStretch(1);
    auto *b = new QVBoxLayout(column);
    b->setContentsMargins(28, 16, 28, 16);
    b->setSpacing(0);

    auto *title = new QLabel(heading);
    title->setObjectName(QStringLiteral("heading"));
    b->addWidget(title);
    b->addSpacing(2);
    auto *sub = new QLabel(subtitle);
    sub->setObjectName(QStringLiteral("subtitle"));
    sub->setWordWrap(true);
    // A page that sets its subtitle later shows it then.
    sub->setVisible(!subtitle.isEmpty() || subtitleLabel);
    b->addWidget(sub);
    if (subtitleLabel)
        *subtitleLabel = sub;
    b->addSpacing(8);

    scroll->setWidget(body);
    outer->addWidget(scroll, 1);
    return b;
}

} // namespace intox
