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
class QSpinBox;
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
    // Whether the warning before starting several clients was accepted in
    // this run of the app.
    bool severalWarned = false;

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

// The Play page's "Run several" group (a switch per profile and Play
// selected, which starts one launcher per checked profile a moment apart)
// and its "Running" group (the clients running now, refreshed every two
// seconds from the profiles' locks, each with a Stop button).
class SeveralClients : public QWidget {
    Q_OBJECT
public:
    explicit SeveralClients(AppState *state);
    void refresh();

protected:
    void showEvent(QShowEvent *event) override;
    void hideEvent(QHideEvent *event) override;

private:
    void playSelected();
    void stopOne(const QString &profile);
    // Asked once per run before more than one client would run; false stops.
    bool acceptWarning();

    AppState *m_state;
    QVBoxLayout *m_profileRows;
    QVBoxLayout *m_runningRows;
    QLabel *m_noneRunning;
    QPushButton *m_playSelected;
    QTimer *m_timer;
    QStringList m_names;
    QMap<QString, Switch *> m_checks;
    QMap<QString, QLabel *> m_descs;
    // Checked and waiting for their turn to start.
    QSet<QString> m_pending;
    QStringList m_shownRunning;
};

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
// reflections, depth of field, bloom, lens and sharpen. At the top the
// whole-page modes (Off, Competitive, Cinematic, and Custom when the effects
// were changed by hand), then one group per effect with its switch, its
// looks and a fine-tune row per knob, and between them the switch for the
// in-game F9 hotkey that turns them all off and back on. Each control writes
// settings.conf as it changes. A client built without the visual effects
// gets a short page saying how to add them instead.
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
        // Named choices, one per value from the knob's minimum.
        QList<QPushButton *> choices;
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
    void modeChosen(int index);
    // The tables of the effects the client listed.
    QList<SsrTable> knownTables() const;
    // Write row ROW's override from its controls.
    void knobMoved(int effect, int row);
    void showSliders(const KnobRow &row, const SsrValue &value);

    AppState *m_state;
    QList<EffectUi> m_effects;
    QList<ShaderMode> m_modes;
    // One per mode, then Custom.
    QList<QPushButton *> m_modeButtons;
    QLabel *m_modeDesc = nullptr;
    // The effects hotkey (INTOXICATED_EFFECTS_HOTKEY).
    Switch *m_hotkey = nullptr;
    QLabel *m_tableError;
    bool m_filling = false;
    // The client was built without the effects: the page only says how to
    // add them.
    bool m_unavailable = false;
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

// In-game macros: a key press plays a recorded sequence of input back through
// the client's own input path, so a macro only fires while the Roblox window
// is focused. The page edits <profile>/macros.json, which the client reads at
// launch; the list on the left is the profile's macros, the editor on the
// right the selected one, and a master switch writes INTOXICATED_MACROS. Every
// change saves the file at once.
class MacrosPage : public QWidget {
    Q_OBJECT
public:
    explicit MacrosPage(AppState *state);
    void reload();

protected:
    void showEvent(QShowEvent *event) override;

private:
    void save();
    void fillList();
    void showEditor();
    int selectedRow() const;
    MacroDef *current();
    void addMacro();
    void duplicateMacro();
    void deleteMacro();
    void nameEdited();
    void triggerChanged();
    void switchesChanged();
    void addStep();
    void editStep();
    void moveStep(int delta);
    void removeStep();
    void fillSteps();
    void setMaster(bool on);
    void updateButtons();
    void setStatus(const QString &text, bool error);

    AppState *m_state;
    QList<MacroDef> m_macros;
    // The file could not be read: it is left untouched rather than written over.
    bool m_broken = false;

    Switch *m_master;
    QListWidget *m_list;
    QPushButton *m_add;
    QPushButton *m_dup;
    QPushButton *m_del;

    QWidget *m_editor;
    QLineEdit *m_name;
    Switch *m_enabled;
    Combo *m_triggerKey;
    Switch *m_tCtrl;
    Switch *m_tShift;
    Switch *m_tAlt;
    Switch *m_swallow;
    Switch *m_repeat;
    QListWidget *m_steps;
    QPushButton *m_stepAdd;
    QPushButton *m_stepEdit;
    QPushButton *m_stepUp;
    QPushButton *m_stepDown;
    QPushButton *m_stepDel;
    QLabel *m_status;
    bool m_filling = false;
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

// Add or edit one macro step. The type picker chooses which fields show: a key
// and its modifiers, a point, a mouse button, a scroll amount, text, or a wait.
class StepDialog : public QDialog {
    Q_OBJECT
public:
    StepDialog(QWidget *parent, const MacroStep &step);
    MacroStep step() const;

private:
    void typeChanged();

    Combo *m_type;
    QWidget *m_keyRow;
    Combo *m_key;
    Switch *m_ctrl;
    Switch *m_shift;
    Switch *m_alt;
    QWidget *m_pointRow;
    QSpinBox *m_x;
    QSpinBox *m_y;
    QWidget *m_buttonRow;
    Combo *m_button;
    QWidget *m_scrollRow;
    QSpinBox *m_detents;
    QWidget *m_textRow;
    QLineEdit *m_text;
    QWidget *m_waitRow;
    QSpinBox *m_wait;
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
    // Asked once per run the first time the Macros page is opened from the
    // sidebar; false sends the user back to the page they came from.
    bool acceptMacrosWarning();

    AppState *m_state;
    QListWidget *m_nav;
    QStackedWidget *m_stack;
    Combo *m_profiles;
    QLabel *m_status;
    QTimer *m_statusTimer;
    int m_lastPage = 0;
    bool m_macrosWarned = false;
};

// `intoxicated-settings --askpass PROMPT`, for sudo -A.
int runAskpass(int argc, char **argv);

} // namespace intox
