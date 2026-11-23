// Intoxicated Settings: an optional desktop app for Intoxicated's flags,
// client options and profiles, and a place to start it from.
//
//   intoxicated-settings                 the window
//   intoxicated-settings --page <name>   open on a page (play, fastflags,
//                                        client, profiles, mods, about)
//   intoxicated-settings --size WxH      open at that size
//                                        (--page and --size are for tests
//                                        and screenshots)
//   intoxicated-settings --askpass PROMPT
//                                        sudo -A's password dialog
//   intoxicated-settings --self-test     check the load and save code
//                                        against scratch directories

#include "app.h"
#include "core.h"
#include "style.h"

#include <QApplication>
#include <QCoreApplication>

#include <cstdio>
#include <cstring>

namespace {

const char *kUsage = "usage: intoxicated-settings [--page play|fastflags|client|profiles|mods|about] [--size WxH]\n"
                     "       intoxicated-settings --askpass PROMPT\n"
                     "       intoxicated-settings --self-test\n";

} // namespace

int main(int argc, char **argv) {
    for (int i = 1; i < argc; ++i) {
        if (std::strcmp(argv[i], "--self-test") == 0) {
            QCoreApplication app(argc, argv);
            return intox::runSelfTest();
        }
        if (std::strcmp(argv[i], "--askpass") == 0)
            return intox::runAskpass(argc, argv);
    }

    QApplication app(argc, argv);
    QCoreApplication::setApplicationName(QStringLiteral("intoxicated-settings"));
    QGuiApplication::setDesktopFileName(QStringLiteral("intoxicated-settings"));

    int page = 0;
    QSize size;
    const QStringList args = app.arguments();
    for (int i = 1; i < args.size(); ++i) {
        if (args[i] == QLatin1String("--page") && i + 1 < args.size()) {
            const QString wanted = args[++i];
            page = -1;
            const QStringList names = intox::MainWindow::pageNames();
            for (int p = 0; p < names.size(); ++p) {
                if (names[p].compare(wanted, Qt::CaseInsensitive) == 0)
                    page = p;
            }
            if (page < 0) {
                std::fprintf(stderr, "intoxicated-settings: no page called %s\n%s", qPrintable(wanted), kUsage);
                return 2;
            }
        } else if (args[i] == QLatin1String("--size") && i + 1 < args.size()) {
            const QStringList wh = args[++i].split(QLatin1Char('x'));
            size = wh.size() == 2 ? QSize(wh[0].toInt(), wh[1].toInt()) : QSize();
            if (size.width() < 900 || size.height() < 600) {
                std::fprintf(stderr, "intoxicated-settings: --size wants WxH, at least 900x600\n%s", kUsage);
                return 2;
            }
        } else if (args[i] == QLatin1String("-h") || args[i] == QLatin1String("--help")) {
            std::fputs(kUsage, stdout);
            return 0;
        } else {
            std::fprintf(stderr, "intoxicated-settings: unrecognised argument %s\n%s", qPrintable(args[i]), kUsage);
            return 2;
        }
    }

    intox::applyLook(app);
    intox::AppState state;
    intox::MainWindow window(&state, page);
    if (size.isValid())
        window.resize(size);
    window.show();
    return app.exec();
}
