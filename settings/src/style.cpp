// Intoxicated Settings: the look. Fusion with an Adwaita-like dark palette,
// a solid red accent (#E01B24) and Phosphor icons, as in the approved GTK
// preview.

#include "style.h"

#include <QApplication>
#include <QFrame>
#include <QGroupBox>
#include <QHash>
#include <QHBoxLayout>
#include <QLabel>
#include <QPainter>
#include <QPainterPath>
#include <QPalette>
#include <QPushButton>
#include <QScrollArea>
#include <QStyleFactory>
#include <QVBoxLayout>
#include <QWindow>

namespace intox {

static QPalette darkPalette() {
    QPalette pal;
    const QColor window(0x24, 0x24, 0x24);
    const QColor text(0xf2, 0xf2, 0xf2);
    const QColor dim(0x8a, 0x8a, 0x8a);
    pal.setColor(QPalette::Window, window);
    pal.setColor(QPalette::WindowText, text);
    pal.setColor(QPalette::Base, QColor(0x36, 0x36, 0x36));
    pal.setColor(QPalette::AlternateBase, QColor(0x36, 0x36, 0x36));
    pal.setColor(QPalette::Text, text);
    pal.setColor(QPalette::BrightText, Qt::white);
    pal.setColor(QPalette::Button, QColor(0x3a, 0x3a, 0x3a));
    pal.setColor(QPalette::ButtonText, text);
    pal.setColor(QPalette::Light, QColor(0x50, 0x50, 0x50));
    pal.setColor(QPalette::Midlight, QColor(0x46, 0x46, 0x46));
    pal.setColor(QPalette::Mid, QColor(0x30, 0x30, 0x30));
    pal.setColor(QPalette::Dark, QColor(0x1e, 0x1e, 0x1e));
    pal.setColor(QPalette::Shadow, QColor(0x10, 0x10, 0x10));
    pal.setColor(QPalette::Highlight, kAccent);
    pal.setColor(QPalette::HighlightedText, Qt::white);
    pal.setColor(QPalette::Inactive, QPalette::Highlight, kAccent);
    pal.setColor(QPalette::Inactive, QPalette::HighlightedText, Qt::white);
    pal.setColor(QPalette::Link, QColor(0xf6, 0x61, 0x51));
    pal.setColor(QPalette::ToolTipBase, QColor(0x38, 0x38, 0x38));
    pal.setColor(QPalette::ToolTipText, text);
    pal.setColor(QPalette::PlaceholderText, dim);
    pal.setColor(QPalette::Accent, kAccent);
    pal.setColor(QPalette::Disabled, QPalette::Text, dim);
    pal.setColor(QPalette::Disabled, QPalette::WindowText, dim);
    pal.setColor(QPalette::Disabled, QPalette::ButtonText, dim);
    return pal;
}

// Adwaita dark, roughly: #242424 for the content, #2e2e2e for the sidebar and
// the bottom bar, #363636 for the rounded row groups, #3a3a3a for buttons
// (#474747 on a row group), and one solid red, #E01B24, for what is on or
// chosen. No gradients, and no outlines but the thin lines between rows.
static const char *kStyleSheet = R"(
QWidget#root { background: #242424; }
QDialog { background: #2e2e2e; }
QMessageBox { background: #2e2e2e; }
QDialogButtonBox { dialogbuttonbox-buttons-have-icons: 0; }

QFrame#sidebar {
    background: #2e2e2e;
    border: none;
    border-right: 1px solid #1e1e1e;
}
QLabel#brand { color: #ffffff; font-size: 17px; font-weight: 700; }

QListWidget#nav { background: transparent; border: none; outline: 0; padding: 0px; }
QListWidget#nav::item {
    color: #e6e6e6;
    padding: 0px 0px 0px 12px;
    border-radius: 8px;
    border: none;
}
QListWidget#nav::item:hover { background: #3a3a3a; color: #ffffff; }
QListWidget#nav::item:selected { background: #E01B24; color: #ffffff; }
QListWidget#nav::item:selected:hover { background: #E8333B; }

QListWidget {
    background: #2e2e2e;
    color: #f0f0f0;
    border: none;
    border-radius: 8px;
    padding: 4px;
    outline: 0;
}
QListWidget::item { padding: 5px 8px; border-radius: 6px; }
QListWidget::item:hover { background: #3a3a3a; }
QListWidget::item:selected { background: #5a2a2d; color: #ffffff; }

QScrollArea#page { background: transparent; border: none; }
QWidget#pageBody { background: #242424; }

QLabel#heading { color: #ffffff; font-size: 24px; font-weight: 700; }
QLabel#subtitle { color: #a8a8a8; font-size: 13px; }
QLabel#note { color: #a0a0a0; font-size: 12px; }
QLabel#error { color: #ff7b63; font-size: 12px; }
QLabel#status { color: #a0a0a0; font-size: 12px; }
QLabel#aboutName { color: #ffffff; font-size: 28px; font-weight: 700; }
QLabel#aboutTag { color: #d0d0d0; font-size: 15px; }
QLabel#experimentalTag {
    background: qlineargradient(x1:0, y1:0, x2:1, y2:0, stop:0 #E91F26, stop:1 #A5071A);
    color: #ffffff;
    font-size: 11px;
    font-weight: 700;
    border-radius: 4px;
    padding: 2px 7px;
}

QFrame#banner {
    background: #4a2b2d;
    border: none;
    border-radius: 10px;
}
QFrame#banner QLabel { color: #f6e0e1; }

QGroupBox {
    background: #363636;
    border: none;
    border-radius: 10px;
    margin-top: 26px;
    padding: 0px;
}
QGroupBox::title {
    subcontrol-origin: margin;
    subcontrol-position: top left;
    left: 0px;
    top: 0px;
    padding: 0px;
    color: #ffffff;
}
QGroupBox#plain { background: transparent; }

QFrame#rowSep { background: #2a2a2a; border: none; max-height: 1px; min-height: 1px; }
QLabel#rowTitle { color: #f2f2f2; font-size: 13px; }
QLabel#rowDesc { color: #9a9a9a; font-size: 11px; }

QTableWidget {
    background: #363636;
    alternate-background-color: #363636;
    border: none;
    border-radius: 10px;
    color: #f0f0f0;
    gridline-color: #2a2a2a;
    selection-background-color: transparent;
}
QTableWidget::item { padding-left: 7px; border: none; border-bottom: 1px solid #2c2c2c; }
QTableWidget#profiles { selection-background-color: #5a2a2d; selection-color: #ffffff; }
QTableWidget#profiles::item:selected { background: #5a2a2d; color: #ffffff; }
QTableWidget QLineEdit { padding: 0px 5px; border-radius: 0px; background: #2a2a2a; }
QHeaderView { background: transparent; }
QHeaderView::section {
    background: #363636;
    color: #a0a0a0;
    border: none;
    border-bottom: 1px solid #2a2a2a;
    padding: 6px 10px;
    font-size: 12px;
    font-weight: 600;
}
QHeaderView::section:first { border-top-left-radius: 10px; }
QHeaderView::section:last { border-top-right-radius: 10px; }

QPushButton {
    background: #3a3a3a;
    color: #f2f2f2;
    border: none;
    border-radius: 8px;
    padding: 7px 14px;
    font-weight: 600;
}
QPushButton:hover { background: #454545; }
QPushButton:pressed { background: #303030; }
QPushButton:disabled { background: #303030; color: #6e6e6e; }
QGroupBox QPushButton { background: #474747; }
QGroupBox QPushButton:hover { background: #525252; }
QGroupBox QPushButton:pressed { background: #3c3c3c; }
QGroupBox QPushButton:disabled { background: #3d3d3d; color: #707070; }
QGroupBox#plain QPushButton { background: #3a3a3a; }
QGroupBox#plain QPushButton:hover { background: #454545; }
QGroupBox#plain QPushButton:pressed { background: #303030; }
QGroupBox#plain QPushButton:disabled { background: #303030; color: #6e6e6e; }
QDialog QPushButton:default, QMessageBox QPushButton:default { background: #E01B24; color: #ffffff; }
QDialog QPushButton:default:hover, QMessageBox QPushButton:default:hover { background: #E8333B; }
QDialog QPushButton:default:disabled, QMessageBox QPushButton:default:disabled { background: #3a3a3a; color: #6e6e6e; }

QTableWidget QPushButton#deleteBtn {
    background: transparent;
    border: none;
    border-radius: 6px;
    padding: 4px;
}
QTableWidget QPushButton#deleteBtn:hover { background: #474747; }

QPushButton#linkBtn {
    background: transparent;
    border: none;
    color: #f66151;
    padding: 2px 0px;
    font-weight: 400;
    text-decoration: underline;
}
QPushButton#linkBtn:hover { color: #ff8a7d; }

QFrame#bottomBar {
    background: #2e2e2e;
    border: none;
    border-top: 1px solid #1e1e1e;
}

QComboBox {
    background: #3a3a3a;
    color: #f2f2f2;
    border: none;
    border-radius: 8px;
    padding: 7px 12px;
    min-width: 170px;
}
QComboBox:hover { background: #454545; }
QGroupBox QComboBox { background: #474747; }
QGroupBox QComboBox:hover { background: #525252; }
QComboBox:disabled { color: #6e6e6e; }
QFrame#bottomBar QComboBox { font-weight: 600; min-width: 0px; padding-right: 34px; }
QComboBox::drop-down { border: none; width: 30px; background: transparent; }
QComboBox::down-arrow { image: none; width: 0px; height: 0px; }
QComboBox QAbstractItemView {
    background: #383838;
    color: #f2f2f2;
    border: 1px solid #2a2a2a;
    selection-background-color: #E01B24;
    outline: 0;
}

QLineEdit {
    background: #3a3a3a;
    color: #f2f2f2;
    border: 1px solid transparent;
    border-radius: 8px;
    padding: 6px 10px;
    selection-background-color: #E01B24;
}
QGroupBox QLineEdit { background: #474747; }
QLineEdit:focus { border-color: #8a4a4d; }
QLineEdit[invalid="true"] { border-color: #E01B24; }

QPlainTextEdit#log {
    background: #2a2a2a;
    color: #dcdcdc;
    border: none;
    border-radius: 8px;
    padding: 4px;
    selection-background-color: #C01C28;
}

QPushButton#play {
    background: #E01B24;
    color: #ffffff;
    border: none;
    border-radius: 18px;
    min-height: 36px;
    max-height: 36px;
    padding: 0px 72px;
    font-size: 15px;
    font-weight: 700;
}
QPushButton#play:hover { background: #E8333B; }
QPushButton#play:pressed { background: #C01C28; }

QPushButton#segment {
    background: #474747;
    color: #f0f0f0;
    border: none;
    border-radius: 0px;
    padding: 6px 10px;
    font-weight: 600;
}
QPushButton#segment[seg="mid"], QPushButton#segment[seg="last"] { border-left: 1px solid #2e2e2e; }
QPushButton#segment[seg="first"] { border-top-left-radius: 8px; border-bottom-left-radius: 8px; }
QPushButton#segment[seg="last"] { border-top-right-radius: 8px; border-bottom-right-radius: 8px; }
QPushButton#segment[seg="only"] { border-radius: 8px; }
QPushButton#segment:hover { background: #525252; }
QPushButton#segment:checked { background: #E01B24; color: #ffffff; }
QPushButton#segment:checked:hover { background: #E8333B; }
QPushButton#segment:disabled { background: #3e3e3e; color: #747474; }
QPushButton#segment:checked:disabled { background: #5a3335; color: #b8a8a8; }

QSlider { min-height: 20px; }
QSlider::groove:horizontal { height: 4px; background: #545454; border-radius: 2px; }
QSlider::sub-page:horizontal { background: #E01B24; border-radius: 2px; }
QSlider::add-page:horizontal { background: #545454; border-radius: 2px; }
QSlider::handle:horizontal {
    background: #f2f2f2;
    width: 16px;
    height: 16px;
    margin: -6px 0px;
    border-radius: 8px;
}
QSlider::handle:horizontal:hover { background: #ffffff; }
QSlider::sub-page:horizontal:disabled { background: #5a5a5a; }
QSlider::handle:horizontal:disabled { background: #6a6a6a; }

QLabel#knobValue { color: #d0d0d0; font-size: 12px; }
QLabel#knobValue:disabled { color: #6e6e6e; }
QLabel#knobValue[set="true"] { color: #f66151; font-weight: 600; }

QPushButton#resetKnob {
    background: #474747;
    color: #e6e6e6;
    border: none;
    border-radius: 6px;
    padding: 3px 12px;
    font-size: 12px;
}
QPushButton#resetKnob:hover { background: #525252; color: #ffffff; }
QPushButton#resetKnob:checked { background: #474747; }
QPushButton#resetKnob:disabled { color: #6a6a6a; background: #3e3e3e; }

QScrollBar:vertical { background: transparent; width: 10px; margin: 2px; }
QScrollBar::handle:vertical { background: #5a5a5a; border-radius: 3px; min-height: 30px; }
QScrollBar::handle:vertical:hover { background: #6a6a6a; }
QScrollBar::add-line:vertical, QScrollBar::sub-line:vertical { height: 0px; }
QScrollBar::add-page:vertical, QScrollBar::sub-page:vertical { background: transparent; }
QScrollBar:horizontal { background: transparent; height: 10px; margin: 2px; }
QScrollBar::handle:horizontal { background: #5a5a5a; border-radius: 3px; min-width: 30px; }
QScrollBar::add-line:horizontal, QScrollBar::sub-line:horizontal { width: 0px; }
QScrollBar::add-page:horizontal, QScrollBar::sub-page:horizontal { background: transparent; }

QToolTip { background: #383838; color: #f2f2f2; border: 1px solid #2a2a2a; padding: 4px; }
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

// The icon NAME in each mode's colour. With a GAP, every pixmap is a quarter
// wider than tall and the glyph sits at its left, so the text after it starts
// further away than the 4 px Qt leaves.
static QIcon tintedIcon(const QString &name, bool gap) {
    static QHash<QString, QIcon> cache;
    const QString key = name + (gap ? QStringLiteral("+gap") : QString());
    const auto found = cache.constFind(key);
    if (found != cache.constEnd())
        return *found;
    const QImage source = QImage(QStringLiteral(":/icons/%1.png").arg(name))
                              .convertToFormat(QImage::Format_ARGB32_Premultiplied);
    QIcon result;
    if (!source.isNull()) {
        auto tinted = [&source, gap](const QColor &colour, int side) {
            const QImage glyph = source.scaled(side, side, Qt::KeepAspectRatio, Qt::SmoothTransformation);
            QImage img(gap ? side + side / 4 : side, side, QImage::Format_ARGB32_Premultiplied);
            img.fill(Qt::transparent);
            QPainter p(&img);
            p.drawImage(0, 0, glyph);
            p.setCompositionMode(QPainter::CompositionMode_SourceIn);
            p.fillRect(img.rect(), colour);
            p.end();
            return QPixmap::fromImage(img);
        };
        for (int side : {16, 18, 20, 32, 36, 40, 64}) {
            result.addPixmap(tinted(QColor(0xe6, 0xe6, 0xe6), side), QIcon::Normal);
            result.addPixmap(tinted(QColor(0xff, 0xff, 0xff), side), QIcon::Active);
            result.addPixmap(tinted(QColor(0xff, 0xff, 0xff), side), QIcon::Selected);
            result.addPixmap(tinted(QColor(0x70, 0x70, 0x70), side), QIcon::Disabled);
        }
    }
    cache.insert(key, result);
    return result;
}

QIcon icon(const QString &name) { return tintedIcon(name, false); }

void setButtonIcon(QPushButton *button, const QString &name) {
    button->setIcon(tintedIcon(name, true));
    button->setIconSize(QSize(20, 16));
}

void linkSegments(QHBoxLayout *row, const QList<QPushButton *> &buttons) {
    row->setSpacing(0);
    for (int i = 0; i < buttons.size(); ++i) {
        const char *where = buttons.size() == 1       ? "only"
                            : i == 0                  ? "first"
                            : i == buttons.size() - 1 ? "last"
                                                      : "mid";
        buttons[i]->setProperty("seg", QString::fromLatin1(where));
    }
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
    p.setPen(Qt::NoPen);
    p.setBrush(isChecked() ? kAccent : QColor(0x55, 0x55, 0x55));
    p.drawRoundedRect(track, r, r);

    const qreal pad = 3.0;
    const qreal d = track.height() - 2 * pad;
    const qreal x = isChecked() ? track.right() - pad - d : track.left() + pad;
    const QRectF knob(x, track.top() + pad, d, d);
    p.setBrush(isChecked() ? QColor(Qt::white) : QColor(0xd6, 0xd6, 0xd6));
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
    const QColor colour = isEnabled() ? QColor(0xe6, 0xe6, 0xe6) : QColor(0x60, 0x60, 0x60);
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
    f.setPixelSize(14);
    f.setWeight(QFont::Bold);
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
    h->setContentsMargins(14, 8, 14, 8);
    h->setSpacing(16);

    auto *texts = new QVBoxLayout;
    texts->setSpacing(2);
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
