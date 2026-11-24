// Intoxicated Settings: the window, its pages and the state they share.

#pragma once

#include "core.h"

#include <QDialog>
#include <QStyledItemDelegate>
#include <QWidget>

#include <memory>

class QFrame;
class QGroupBox;
class QLabel;
class QLineEdit;
class QListWidget;
class QPlainTextEdit;
class QProcess;
class QPushButton;
class QSlider;
class QStackedWidget;
class QTableWidget;
class QTemporaryDir;
class QTimer;
class QVBoxLayout;

namespace intox {

class Combo;
class Switch;

// What the pages share: the profile chosen in the bottom bar, the client's
// built-in flags, and launching.
class AppState : public QObject {
    Q_OBJECT
public:
    explicit AppState(QObject *parent = nullptr);

    const QString &profile() const { return m_profile; }
    void setProfile(const QString &name);

    // From `intoxicated --default-flags`, read once at start.
    const FlagMap &defaults() const { return m_defaults; }
    const QString &defaultsError() const { return m_defaultsError; }

    // Start `intoxicated [LINK]` detached on the chosen profile.
    bool launch(const QString &link);
    // Whether an update runs (the window asks before closing then).
    bool busy = false;

signals:
    void profileChanged(const QString &name);
    // A profile was created or deleted, or the default one changed.
    void profilesChanged();
    // settings.conf changed under a page (reset, video memory).
    void settingsChanged();
    // A line for the bottom bar.
    void status(const QString &message);

private:
    QString m_profile;
    FlagMap m_defaults;
    QString m_defaultsError;
};

// --- Pages -----------------------------------------------------------------

class PlayPage : public QWidget {
    Q_OBJECT
public:
    explicit PlayPage(AppState *state);
    ~PlayPage() override;

private:
    void join();
    void runUpdate(const QString &what, const QStringList &args, bool install);
    void finished(int code, bool crashed);
    void append(const QString &text);

    AppState *m_state;
    QLineEdit *m_link;
    QPushButton *m_updateApk;
    QPushButton *m_updateApp;
    QPlainTextEdit *m_log;
    QLabel *m_result;
    QProcess *m_process = nullptr;
    std::unique_ptr<QTemporaryDir> m_askpassDir;
    QString m_running;
};

class FlagsPage : public QWidget {
    Q_OBJECT
public:
    explicit FlagsPage(AppState *state);
    void reload();

protected:
    void showEvent(QShowEvent *event) override;

private:
    bool edit(const std::function<void(FlagMap &)> &change);
    void fillPresets();
    void fillTable();
    void fpsChosen(int index);
    void videoMemoryChosen(int index);
    void cellEdited(int row, int column);
    void addFlag();
    void importJson();
    void exportJson();
    void resetFlags();
    void backUp();
    // Why TEXT cannot go in ROW's COLUMN, or an empty string.
    QString cellProblem(int row, int column, const QString &text) const;

    AppState *m_state;
    QLabel *m_subtitle;
    QFrame *m_banner;
    QLabel *m_bannerText;
    QGroupBox *m_presets;
    QGroupBox *m_quality = nullptr;
    QGroupBox *m_custom;
    Combo *m_fps;
    Combo *m_videoMemory;
    QList<QPushButton *> m_qualityButtons;
    QLabel *m_qualityDesc = nullptr;
    QLabel *m_qualityWarning = nullptr;
    QList<Switch *> m_switches;
    QList<QLabel *> m_switchDescs;
    QLabel *m_defaultsNote;
    QTableWidget *m_table;
    QLabel *m_empty;
    QLabel *m_tableError;
    QPushButton *m_export;
    QPushButton *m_reset;
    FlagMap m_flags;
    bool m_broken = false;
    bool m_filling = false;

    friend class FlagDelegate;
};

class ClientPage : public QWidget {
    Q_OBJECT
public:
    explicit ClientPage(AppState *state);
    void reload();

protected:
    void showEvent(QShowEvent *event) override;

private:
    void write(const QString &key, const std::optional<QString> &value);
    void writeWheel();
    void resolutionChosen(int index);
    void reset();

    AppState *m_state;
    Switch *m_fullscreen;
    Combo *m_monitor;
    Combo *m_resolution;
    Combo *m_scale;
    Switch *m_cursor;
    Combo *m_wheel;
    Switch *m_invert;
    Switch *m_noDragLock;
    Combo *m_present;
    Combo *m_renderer;
    Combo *m_audio;
    Switch *m_closeOnLeave;
    bool m_filling = false;
};

// Effects the client draws into the game before the HUD: screen-space
// reflections, depth of field and bloom, one group each with its switch, its
// looks and a fine-tune row per knob. Each control writes settings.conf as it
// changes.
class ShadersPage : public QWidget {
    Q_OBJECT
public:
    explicit ShadersPage(AppState *state);
    void reload();

protected:
    void showEvent(QShowEvent *event) override;

private:
    // One fine-tune row: sliders (three for a colour) or a switch.
    struct KnobRow {
        QString name;
        QList<QSlider *> sliders;
        QList<QLabel *> values;
        double step = 0.01;
        Switch *toggle = nullptr;
        QLabel *shown = nullptr;
        QPushButton *reset = nullptr;
    };
    // One effect's group. Without its table (the client could not be asked)
    // only the switch is offered.
    struct EffectUi {
        SsrTable table;
        bool known = false;
        Switch *on = nullptr;
        QWidget *details = nullptr;
        QList<QPushButton *> lookButtons;
        QLabel *lookDesc = nullptr;
        QLabel *problems = nullptr;
        // The fine-tune rows, shown or hidden by their button; shown at
        // first when the effect has overrides.
        QWidget *tune = nullptr;
        QPushButton *tuneButton = nullptr;
        QList<KnobRow> rows;
    };

    void buildEffect(QVBoxLayout *page, int index);
    void buildFineTune(QVBoxLayout *rows, int index);
    void reloadEffect(const SettingsConf &conf, EffectUi &fx);
    // Load settings.conf, apply CHANGE, write it, show the result.
    void edit(const std::function<bool(SettingsConf &)> &change);
    void lookChosen(int effect, int index);
    // Write row ROW's override from its controls.
    void knobMoved(int effect, int row);
    void showSliders(const KnobRow &row, const SsrValue &value);

    AppState *m_state;
    QList<EffectUi> m_effects;
    QLabel *m_tableError;
    bool m_filling = false;
};

class ProfilesPage : public QWidget {
    Q_OBJECT
public:
    explicit ProfilesPage(AppState *state);
    void reload();

protected:
    void showEvent(QShowEvent *event) override;
    void hideEvent(QHideEvent *event) override;

private:
    QString selected() const;
    void updateButtons();
    void newProfile();
    void setDefault();
    void deleteSelected();
    void importFile();
    void exportSelected();

    AppState *m_state;
    QTableWidget *m_table;
    QPushButton *m_new;
    QPushButton *m_setDefault;
    QPushButton *m_import;
    QPushButton *m_export;
    QPushButton *m_delete;
    QTimer *m_poll;
    QList<ProfileInfo> m_profiles;
    QString m_default;
};

class ModsPage : public QWidget {
    Q_OBJECT
public:
    ModsPage();
};

class AboutPage : public QWidget {
    Q_OBJECT
public:
    AboutPage();

protected:
    void showEvent(QShowEvent *event) override;

private:
    QLabel *m_version;
    QProcess *m_query = nullptr;
};

// --- Dialogs ---------------------------------------------------------------

// One line of input checked as it is typed; OK only when it passes.
class NameDialog : public QDialog {
    Q_OBJECT
public:
    NameDialog(QWidget *parent, const QString &title, const QString &prompt, const QString &placeholder,
               std::function<QString(const QString &)> problem);
    QString text() const;

private:
    QLineEdit *m_edit;
};

class AddFlagDialog : public QDialog {
    Q_OBJECT
public:
    AddFlagDialog(QWidget *parent, const FlagMap &current);
    QString name() const;
    QString value() const;

private:
    void check();

    const FlagMap &m_current;
    QLineEdit *m_name;
    QLineEdit *m_value;
    QLabel *m_problem;
    QPushButton *m_ok;
    QString m_normalised;
};

class ImportDialog : public QDialog {
    Q_OBJECT
public:
    ImportDialog(QWidget *parent, const QString &file, const QList<ImportItem> &items);
    QSet<QString> chosen() const;

private:
    QTableWidget *m_table;
    const QList<ImportItem> &m_items;
};

// The inline editor of the custom flags table: says what is wrong while the
// text is typed and refuses to store a value that does not fit.
class FlagDelegate : public QStyledItemDelegate {
    Q_OBJECT
public:
    FlagDelegate(FlagsPage *page, QLabel *problem);
    QWidget *createEditor(QWidget *parent, const QStyleOptionViewItem &option, const QModelIndex &index) const override;
    void setModelData(QWidget *editor, QAbstractItemModel *model, const QModelIndex &index) const override;

private:
    FlagsPage *m_page;
    QLabel *m_problem;
};

// --- The window ------------------------------------------------------------

class MainWindow : public QWidget {
    Q_OBJECT
public:
    MainWindow(AppState *state, int startPage);
    static QStringList pageNames();
    QSize sizeHint() const override;

protected:
    void closeEvent(QCloseEvent *event) override;

private:
    QWidget *sidebar();
    QWidget *bottomBar();
    void fillProfiles();

    AppState *m_state;
    QListWidget *m_nav;
    QStackedWidget *m_stack;
    Combo *m_profiles;
    QLabel *m_status;
    QTimer *m_statusTimer;
};

// `intoxicated-settings --askpass PROMPT`, for sudo -A.
int runAskpass(int argc, char **argv);

} // namespace intox
