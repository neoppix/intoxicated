// Intoxicated Settings: the look (Fusion, a dark palette, the red accent) and
// the small widgets every page is built from.

#pragma once

#include <QCheckBox>
#include <QColor>
#include <QComboBox>
#include <QPixmap>
#include <QString>

class QGroupBox;
class QLabel;
class QFrame;
class QVBoxLayout;
class QWidget;
class QApplication;

namespace intox {

inline const QColor kAccent(0xE9, 0x1F, 0x26);
inline const QColor kAccentDark(0xA5, 0x07, 0x1A);

// Fusion, the palette, the font and the style sheet.
void applyLook(QApplication &app);

// The logo, compiled in, at SIDE logical pixels for WIDGET's screen.
QPixmap logo(int side, const QWidget *widget);

// A QCheckBox painted as a pill-shaped switch.
class Switch : public QCheckBox {
public:
    explicit Switch(QWidget *parent = nullptr);
    QSize sizeHint() const override { return {42, 22}; }
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
