// Intoxicated Settings: the look (Fusion, an Adwaita-like dark palette, a
// solid red accent, Phosphor icons) and the small widgets every page is built
// from.

#pragma once

#include <QCheckBox>
#include <QColor>
#include <QComboBox>
#include <QIcon>
#include <QList>
#include <QPixmap>
#include <QString>

class QGroupBox;
class QHBoxLayout;
class QPushButton;
class QLabel;
class QFrame;
class QVBoxLayout;
class QWidget;
class QApplication;

namespace intox {

inline const QColor kAccent(0xE0, 0x1B, 0x24);
inline const QColor kAccentDark(0xC0, 0x1C, 0x28);

// Fusion, the palette, the font and the style sheet.
void applyLook(QApplication &app);

// The logo, compiled in, at SIDE logical pixels for WIDGET's screen.
QPixmap logo(int side, const QWidget *widget);

// A compiled-in icon (settings/icons/NAME.png), tinted light grey, white
// when selected and dim when disabled.
QIcon icon(const QString &name);

// Put icon NAME in front of BUTTON's text, a little apart from it.
void setButtonIcon(QPushButton *button, const QString &name);

// Join the segment buttons in ROW into one linked group, as GTK does: no gap,
// rounded only at the two ends.
void linkSegments(QHBoxLayout *row, const QList<QPushButton *> &buttons);

// A QCheckBox painted as a pill-shaped switch.
class Switch : public QCheckBox {
public:
    explicit Switch(QWidget *parent = nullptr);
    QSize sizeHint() const override { return {44, 24}; }
    QSize minimumSizeHint() const override { return sizeHint(); }

protected:
    bool hitButton(const QPoint &pos) const override;
    void paintEvent(QPaintEvent *) override;
};

// A combo box that paints its own chevron (the style sheet hides the arrow).
class Combo : public QComboBox {
public:
    explicit Combo(QWidget *parent = nullptr);

protected:
    void paintEvent(QPaintEvent *e) override;
};

// A rounded group with a bold title; BODY gets the normal font.
struct Section {
    QGroupBox *box;
    QWidget *body;
};
Section section(const QString &title);

// One setting: a title and a description on the left, CONTROL on the right.
// DESCRIPTION may be empty. The description label is returned through DESC.
QWidget *settingRow(const QString &title, const QString &description, QWidget *control, QLabel **desc = nullptr);
QFrame *separator();
// Set a message label's text, showing it only while there is one.
void setProblem(QLabel *label, const QString &text);

// A page: heading, subtitle, then whatever is added to the returned layout,
// all in a scroll area. The subtitle label is returned through SUBTITLE.
QVBoxLayout *pageLayout(QWidget *page, const QString &heading, const QString &subtitle, QLabel **subtitleLabel = nullptr);

} // namespace intox
