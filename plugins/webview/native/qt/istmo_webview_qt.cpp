// Qt WebEngine shim for istmo-webview: a small C ABI over QtWebEngine
// Widgets, driven from Rust (`src/desktop/linux/qt.rs`), which loads it
// with dlopen so the app binary never links Qt.
//
// No Q_OBJECT anywhere (no moc step): behaviour comes from virtual
// overrides and functor connections. Everything runs on the thread that
// called `istmo_qt_init`, which becomes Qt's GUI thread; only
// `istmo_qt_post` may be called from other threads.
//
// Requests to custom schemes and to the `istmo-bridge:` scheme carry no
// page, so every page installs a request interceptor that stamps them
// with `X-Istmo-View: <id>:<secret>`; the scheme handler only trusts a
// stamp whose secret matches the view.

#include <QApplication>
#include <QBuffer>
#include <QByteArray>
#include <QCloseEvent>
#include <QColor>
#include <QDateTime>
#include <QHash>
#include <QIODevice>
#include <QMetaObject>
#include <QMutex>
#include <QMutexLocker>
#include <QNetworkCookie>
#include <QPixmap>
#include <QPointer>
#include <QRandomGenerator>
#include <QString>
#include <QTimeZone>
#include <QTimer>
#include <QUrl>
#include <QWebEngineCookieStore>
#include <QWebEngineFindTextResult>
#include <QWebEngineHistory>
#include <QWebEngineLoadingInfo>
#include <QWebEngineHttpRequest>
#include <QWebEngineNewWindowRequest>
#include <QWebEnginePage>
#include <QWebEnginePermission>
#include <QWebEngineProfile>
#include <QWebEngineScript>
#include <QWebEngineScriptCollection>
#include <QWebEngineSettings>
#include <QWebEngineUrlRequestInfo>
#include <QWebEngineUrlRequestInterceptor>
#include <QWebEngineUrlRequestJob>
#include <QWebEngineUrlScheme>
#include <QWebEngineUrlSchemeHandler>
#include <QWebEngineView>
#include <QWindow>

#include <cstdint>
#include <cstdlib>
#include <cstring>

// Built with -fvisibility=hidden: only the C ABI is exported.
#define ISTMO_QT_API extern "C" __attribute__((visibility("default")))

extern "C" {

typedef void (*istmo_qt_task)(void *data);
typedef void (*istmo_qt_history_item)(void *out, const char *url, const char *title);

// Engine -> Rust. Strings are borrowed for the duration of the call.
typedef struct istmo_qt_callbacks {
    // Main-frame navigation; return 1 to let it proceed.
    int (*navigation)(uint64_t view, const char *url, int user_gesture);
    void (*load_started)(uint64_t view, const char *url);
    void (*committed)(uint64_t view, const char *url);
    // status: 0 succeeded, 1 failed, 2 stopped. `error_domain` is
    // `QWebEngineLoadingInfo::ErrorDomain`.
    void (*load_finished)(uint64_t view, const char *url, int status, int error_domain, int error_code,
                          const char *error, int back, int forward);
    void (*title)(uint64_t view, const char *title);
    void (*progress)(uint64_t view, int percent);
    void (*close_requested)(uint64_t view);
    // status: `QWebEnginePage::RenderProcessTerminationStatus`.
    void (*terminated)(uint64_t view, int status, int exit_code);
    // `istmo-bridge://<kind>/<channel>` request; answer with the
    // `istmo_qt_response_*` calls on `token`.
    void (*bridge)(uint64_t view, uint64_t token, const char *initiator, const char *kind,
                   const char *channel, const uint8_t *data, size_t len);
    // Custom scheme request; answer with the `istmo_qt_response_*` calls.
    void (*scheme)(uint64_t view, uint64_t token, const char *url, const char *method,
                   const char *initiator, int has_body);
    // Answer with `istmo_qt_permission_reply`. Kinds: 0 camera,
    // 1 microphone, 2 camera + microphone, 3 geolocation, 4 notifications,
    // 5 clipboard, 7 other.
    void (*permission)(uint64_t view, uint64_t token, const char *origin, int kind);
    // status: 0 error (text), 1 value (JSON), 2 still pending.
    void (*eval_done)(uint64_t token, int status, const char *text);
    void (*find_done)(uint64_t token, int matches, int active);
    // A page-opened window; returns the id of a view created (with
    // profile kind 3 and `opener`) inside the call, or 0 to refuse.
    uint64_t (*popup)(uint64_t opener, const char *url, int user_gesture);
} istmo_qt_callbacks;

typedef struct istmo_qt_script {
    const char *source;
    // 0 at document creation, 1 when the DOM is ready.
    int at_end;
    int subframes;
    int isolated;
} istmo_qt_script;

typedef struct istmo_qt_view_spec {
    uint64_t id;
    // 0 shared, 1 ephemeral, 2 persistent (`profile_name`, `data_dir`),
    // 3 the opener's profile (`opener`).
    int profile_kind;
    const char *profile_name;
    const char *data_dir;
    uint64_t opener;
    // 0 window, 1 embedded in `parent_xid`.
    int embedded;
    uint64_t parent_xid;
    const char *title;
    int width;
    int height;
    int resizable;
    const char *user_agent;
    // Appended to Qt's default user agent when `user_agent` is unset.
    const char *user_agent_suffix;
    const char *accept_language;
    int javascript;
    int local_storage;
    int autoplay;
    int third_party_cookies;
    int context_menu;
    uint32_t background_rgba;
} istmo_qt_view_spec;

}  // extern "C"

namespace {

istmo_qt_callbacks g_cb;
QApplication *g_app = nullptr;
uint64_t g_next_token = 1;
const QByteArray kBridgeScheme = QByteArrayLiteral("istmo-bridge");
const QByteArray kStamp = QByteArrayLiteral("X-Istmo-View");

uint64_t next_token() { return g_next_token++; }

QByteArray full(const QUrl &url) { return url.toString(QUrl::FullyEncoded).toUtf8(); }

QByteArray origin_of(const QUrl &url) {
    if (!url.isValid() || url.host().isEmpty()) {
        return QByteArray();
    }
    return url.toString(QUrl::RemovePath | QUrl::RemoveQuery | QUrl::RemoveFragment | QUrl::RemoveUserInfo |
                        QUrl::StripTrailingSlash)
        .toUtf8();
}

// Body of a streamed response: chunks pushed on the GUI thread, read by
// Qt WebEngine on its IO thread (hence the mutex).
class ChunkDevice : public QIODevice {
public:
    explicit ChunkDevice(QObject *parent) : QIODevice(parent) { open(QIODevice::ReadOnly); }

    bool isSequential() const override { return true; }

    qint64 bytesAvailable() const override {
        QMutexLocker lock(&mutex_);
        return (buffer_.size() - offset_) + QIODevice::bytesAvailable();
    }

    bool atEnd() const override {
        QMutexLocker lock(&mutex_);
        return finished_ && offset_ == buffer_.size();
    }

    void push(const char *data, qsizetype len) {
        {
            QMutexLocker lock(&mutex_);
            if (offset_ > 0 && offset_ == buffer_.size()) {
                buffer_.clear();
                offset_ = 0;
            }
            buffer_.append(data, len);
        }
        emit readyRead();
    }

    void finish() {
        {
            QMutexLocker lock(&mutex_);
            finished_ = true;
        }
        emit readyRead();
        emit readChannelFinished();
    }

protected:
    qint64 readData(char *out, qint64 max) override {
        QMutexLocker lock(&mutex_);
        const qsizetype left = buffer_.size() - offset_;
        if (left == 0) {
            return finished_ ? -1 : 0;
        }
        const qsizetype n = qMin<qsizetype>(left, static_cast<qsizetype>(max));
        std::memcpy(out, buffer_.constData() + offset_, static_cast<size_t>(n));
        offset_ += n;
        // Compact once the consumed prefix dominates, not on every read.
        if (offset_ > (1 << 16) && offset_ * 2 > buffer_.size()) {
            buffer_.remove(0, offset_);
            offset_ = 0;
        }
        return n;
    }

    qint64 writeData(const char *, qint64) override { return -1; }

private:
    mutable QMutex mutex_;
    QByteArray buffer_;
    qsizetype offset_ = 0;
    bool finished_ = false;
};

struct Response {
    QPointer<QWebEngineUrlRequestJob> job;
    QPointer<ChunkDevice> body;
    bool bridge = false;
};

QHash<uint64_t, Response> g_responses;
QHash<uint64_t, QWebEnginePermission> g_permissions;
QHash<QString, QWebEngineProfile *> g_profiles;
QList<QByteArray> g_schemes;
QString g_base_user_agent;

class Page;

struct ViewState {
    uint64_t id = 0;
    QByteArray secret;
    QWebEngineView *view = nullptr;
    Page *page = nullptr;
    QWebEngineProfile *profile = nullptr;
    bool owns_profile = false;
    bool pending_commit = false;
};

QHash<uint64_t, ViewState *> g_views;

ViewState *find(uint64_t id) { return g_views.value(id); }

class Stamp : public QWebEngineUrlRequestInterceptor {
public:
    Stamp(QByteArray value, QObject *parent) : QWebEngineUrlRequestInterceptor(parent), value_(std::move(value)) {}

    void interceptRequest(QWebEngineUrlRequestInfo &info) override {
        const QByteArray scheme = info.requestUrl().scheme().toUtf8();
        if (scheme == kBridgeScheme || g_schemes.contains(scheme)) {
            // Overwrites anything the page set under the same name.
            info.setHttpHeader(kStamp, value_);
        }
    }

private:
    QByteArray value_;
};

class Page : public QWebEnginePage {
public:
    Page(QWebEngineProfile *profile, uint64_t id) : QWebEnginePage(profile), id_(id) {}

protected:
    bool acceptNavigationRequest(const QUrl &url, NavigationType type, bool main) override {
        if (!main) {
            return true;
        }
        const QByteArray u = full(url);
        return g_cb.navigation(id_, u.constData(), type == NavigationTypeLinkClicked ? 1 : 0) != 0;
    }

private:
    uint64_t id_;
};

// Closing the window is the app's call: report it and keep the view.
class View : public QWebEngineView {
public:
    explicit View(uint64_t id) : id_(id) {}

protected:
    void closeEvent(QCloseEvent *event) override {
        event->ignore();
        g_cb.close_requested(id_);
    }

private:
    uint64_t id_;
};

ViewState *view_of_job(QWebEngineUrlRequestJob *job) {
    const QByteArray stamp = job->requestHeaders().value(kStamp);
    const qsizetype colon = stamp.indexOf(':');
    if (colon <= 0) {
        return nullptr;
    }
    bool ok = false;
    const uint64_t id = stamp.left(colon).toULongLong(&ok);
    ViewState *view = ok ? find(id) : nullptr;
    if (!view || stamp.mid(colon + 1) != view->secret) {
        return nullptr;
    }
    return view;
}

class SchemeHandler : public QWebEngineUrlSchemeHandler {
public:
    void requestStarted(QWebEngineUrlRequestJob *job) override {
        ViewState *view = view_of_job(job);
        if (!view) {
            job->fail(QWebEngineUrlRequestJob::RequestDenied);
            return;
        }
        const uint64_t token = next_token();
        const QUrl url = job->requestUrl();
        const bool bridge = url.scheme().toUtf8() == kBridgeScheme;
        g_responses.insert(token, Response{job, nullptr, bridge});
        const QByteArray initiator = origin_of(job->initiator());
        if (bridge) {
            QByteArray body;
            if (QIODevice *device = job->requestBody()) {
                if (device->isOpen() || device->open(QIODevice::ReadOnly)) {
                    body = device->readAll();
                }
            }
            const QByteArray kind = url.host().toUtf8();
            const QByteArray channel = QUrl::fromPercentEncoding(url.path(QUrl::FullyEncoded).mid(1).toUtf8()).toUtf8();
            g_cb.bridge(view->id, token, initiator.constData(), kind.constData(), channel.constData(),
                        reinterpret_cast<const uint8_t *>(body.constData()), static_cast<size_t>(body.size()));
            return;
        }
        const QByteArray u = full(url);
        g_cb.scheme(view->id, token, u.constData(), job->requestMethod().constData(), initiator.constData(),
                    job->requestBody() ? 1 : 0);
    }
};

SchemeHandler *g_scheme_handler = nullptr;

void install_handlers(QWebEngineProfile *profile) {
    for (const QByteArray &scheme : std::as_const(g_schemes)) {
        if (!profile->urlSchemeHandler(scheme)) {
            profile->installUrlSchemeHandler(scheme, g_scheme_handler);
        }
    }
    if (!profile->urlSchemeHandler(kBridgeScheme)) {
        profile->installUrlSchemeHandler(kBridgeScheme, g_scheme_handler);
    }
}

QWebEngineProfile *profile_for(const istmo_qt_view_spec &spec, bool *owned) {
    *owned = false;
    QWebEngineProfile *profile = nullptr;
    switch (spec.profile_kind) {
    case 1:
        // Off the record: nothing on disk, shared with no other view.
        profile = new QWebEngineProfile(g_app);
        *owned = true;
        break;
    case 2: {
        const QString key = QString::fromUtf8(spec.profile_name);
        profile = g_profiles.value(key);
        if (!profile) {
            profile = new QWebEngineProfile(key, g_app);
            profile->setPersistentStoragePath(QString::fromUtf8(spec.data_dir) + QStringLiteral("/data"));
            profile->setCachePath(QString::fromUtf8(spec.data_dir) + QStringLiteral("/cache"));
            g_profiles.insert(key, profile);
        }
        break;
    }
    case 3:
        if (ViewState *opener = find(spec.opener)) {
            return opener->profile;
        }
        [[fallthrough]];
    default:
        profile = QWebEngineProfile::defaultProfile();
        break;
    }
    install_handlers(profile);
    // Profile-wide in Qt: the last view created on a profile wins.
    if (spec.user_agent && *spec.user_agent) {
        profile->setHttpUserAgent(QString::fromUtf8(spec.user_agent));
    } else if (spec.user_agent_suffix && *spec.user_agent_suffix) {
        profile->setHttpUserAgent(g_base_user_agent + QLatin1Char(' ') + QString::fromUtf8(spec.user_agent_suffix));
    }
    if (spec.accept_language && *spec.accept_language) {
        profile->setHttpAcceptLanguage(QString::fromUtf8(spec.accept_language));
    }
    if (!spec.third_party_cookies) {
        profile->cookieStore()->setCookieFilter(
            [](const QWebEngineCookieStore::FilterRequest &r) { return !r.thirdParty; });
    }
    return profile;
}

int permission_kind(QWebEnginePermission::PermissionType type) {
    using T = QWebEnginePermission::PermissionType;
    switch (type) {
    case T::MediaVideoCapture: return 0;
    case T::MediaAudioCapture: return 1;
    case T::MediaAudioVideoCapture: return 2;
    case T::Geolocation: return 3;
    case T::Notifications: return 4;
    case T::ClipboardReadWrite: return 5;
    default: return 7;
    }
}

void wire(ViewState *state) {
    Page *page = state->page;
    QWebEngineView *view = state->view;
    const uint64_t id = state->id;
    // Qt has no commit signal: the first URL change of a load is its
    // commit (same-document changes outside a load are not commits); a
    // load that never changed the URL commits when it ends.
    QObject::connect(page, &QWebEnginePage::loadingChanged, view, [state, page, id](const QWebEngineLoadingInfo &info) {
        const QByteArray u = full(info.url());
        switch (info.status()) {
        case QWebEngineLoadingInfo::LoadStartedStatus:
            state->pending_commit = true;
            g_cb.load_started(id, u.constData());
            return;
        case QWebEngineLoadingInfo::LoadSucceededStatus:
        case QWebEngineLoadingInfo::LoadFailedStatus:
        case QWebEngineLoadingInfo::LoadStoppedStatus:
            break;
        }
        if (state->pending_commit && info.status() == QWebEngineLoadingInfo::LoadSucceededStatus) {
            state->pending_commit = false;
            g_cb.committed(id, u.constData());
        }
        state->pending_commit = false;
        const int status = info.status() == QWebEngineLoadingInfo::LoadSucceededStatus ? 0
                           : info.status() == QWebEngineLoadingInfo::LoadFailedStatus  ? 1
                                                                                        : 2;
        const QByteArray error = info.errorString().toUtf8();
        QWebEngineHistory *history = page->history();
        g_cb.load_finished(id, u.constData(), status, static_cast<int>(info.errorDomain()), info.errorCode(),
                           error.constData(), history->canGoBack() ? 1 : 0, history->canGoForward() ? 1 : 0);
    });
    QObject::connect(page, &QWebEnginePage::urlChanged, view, [state, id](const QUrl &url) {
        if (!state->pending_commit) {
            return;
        }
        state->pending_commit = false;
        const QByteArray u = full(url);
        g_cb.committed(id, u.constData());
    });
    QObject::connect(page, &QWebEnginePage::titleChanged, view, [id](const QString &title) {
        const QByteArray t = title.toUtf8();
        g_cb.title(id, t.constData());
    });
    QObject::connect(page, &QWebEnginePage::loadProgress, view, [id](int p) { g_cb.progress(id, p); });
    QObject::connect(page, &QWebEnginePage::windowCloseRequested, view, [id] { g_cb.close_requested(id); });
    QObject::connect(page, &QWebEnginePage::renderProcessTerminated, view,
                     [id](QWebEnginePage::RenderProcessTerminationStatus status, int code) {
                         g_cb.terminated(id, static_cast<int>(status), code);
                     });
    QObject::connect(page, &QWebEnginePage::permissionRequested, view, [id](QWebEnginePermission permission) {
        const uint64_t token = next_token();
        const QByteArray origin = origin_of(permission.origin());
        const int kind = permission_kind(permission.permissionType());
        g_permissions.insert(token, permission);
        g_cb.permission(id, token, origin.constData(), kind);
    });
    QObject::connect(page, &QWebEnginePage::newWindowRequested, view, [id](QWebEngineNewWindowRequest &request) {
        const QByteArray u = full(request.requestedUrl());
        const uint64_t child = g_cb.popup(id, u.constData(), request.isUserInitiated() ? 1 : 0);
        if (ViewState *created = child ? find(child) : nullptr) {
            request.openIn(created->page);
        }
    });
}

QByteArray random_secret() {
    quint32 words[4];
    QRandomGenerator::system()->fillRange(words);
    return QByteArray(reinterpret_cast<const char *>(words), sizeof(words)).toHex();
}

}  // namespace

// Register schemes and create the application, on the thread that will
// run the event loop. `platform` is a Qt platform plugin name or NULL.
ISTMO_QT_API int istmo_qt_init(const char *const *schemes, size_t count, const char *platform,
                               istmo_qt_callbacks callbacks) {
    // One QApplication per process: an app already running Qt owns it.
    if (g_app || QCoreApplication::instance()) {
        return -1;
    }
    g_cb = callbacks;
    for (size_t i = 0; i < count; ++i) {
        g_schemes.append(QByteArray(schemes[i]));
    }
    QList<QByteArray> all = g_schemes;
    all.append(kBridgeScheme);
    for (const QByteArray &name : std::as_const(all)) {
        QWebEngineUrlScheme scheme(name);
        scheme.setSyntax(QWebEngineUrlScheme::Syntax::Host);
        scheme.setFlags(QWebEngineUrlScheme::SecureScheme | QWebEngineUrlScheme::CorsEnabled |
                        QWebEngineUrlScheme::FetchApiAllowed);
        QWebEngineUrlScheme::registerScheme(scheme);
    }
    QCoreApplication::setAttribute(Qt::AA_ShareOpenGLContexts);
    static QByteArray name = QByteArrayLiteral("istmo-webview");
    static QByteArray flag = QByteArrayLiteral("-platform");
    static QByteArray plugin;
    static int argc = 1;
    static char *argv[4] = {name.data(), nullptr, nullptr, nullptr};
    if (platform && *platform) {
        plugin = QByteArray(platform);
        argv[1] = flag.data();
        argv[2] = plugin.data();
        argc = 3;
    }
    g_app = new QApplication(argc, argv);
    g_app->setQuitOnLastWindowClosed(false);
    g_scheme_handler = new SchemeHandler();
    g_base_user_agent = QWebEngineProfile::defaultProfile()->httpUserAgent();
    return 0;
}

ISTMO_QT_API void istmo_qt_run(void) { g_app->exec(); }

// Thread-safe: queue `task(data)` on the GUI thread.
ISTMO_QT_API void istmo_qt_post(istmo_qt_task task, void *data) {
    QMetaObject::invokeMethod(g_app, [task, data] { task(data); }, Qt::QueuedConnection);
}

// GUI thread: run `task(data)` after `ms` milliseconds.
ISTMO_QT_API void istmo_qt_post_after(int ms, istmo_qt_task task, void *data) {
    QTimer::singleShot(ms, g_app, [task, data] { task(data); });
}

ISTMO_QT_API const char *istmo_qt_version(void) { return qVersion(); }

ISTMO_QT_API int istmo_qt_view_new(const istmo_qt_view_spec *spec) {
    if (g_views.contains(spec->id)) {
        return -1;
    }
    auto *state = new ViewState();
    state->id = spec->id;
    state->secret = random_secret();
    state->profile = profile_for(*spec, &state->owns_profile);
    state->page = new Page(state->profile, spec->id);
    state->view = new View(spec->id);
    state->view->setPage(state->page);
    state->page->setUrlRequestInterceptor(
        new Stamp(QByteArray::number(static_cast<qulonglong>(spec->id)) + ':' + state->secret, state->page));
    QWebEngineSettings *s = state->page->settings();
    s->setAttribute(QWebEngineSettings::JavascriptEnabled, spec->javascript != 0);
    s->setAttribute(QWebEngineSettings::LocalStorageEnabled, spec->local_storage != 0);
    s->setAttribute(QWebEngineSettings::PlaybackRequiresUserGesture, spec->autoplay == 0);
    s->setAttribute(QWebEngineSettings::LocalContentCanAccessFileUrls, false);
    s->setAttribute(QWebEngineSettings::LocalContentCanAccessRemoteUrls, false);
    s->setAttribute(QWebEngineSettings::JavascriptCanOpenWindows, false);
    if (!spec->context_menu) {
        state->view->setContextMenuPolicy(Qt::NoContextMenu);
    }
    const uint32_t c = spec->background_rgba;
    state->page->setBackgroundColor(QColor((c >> 24) & 0xff, (c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff));
    g_views.insert(spec->id, state);
    wire(state);
    if (!spec->embedded) {
        state->view->setWindowTitle(QString::fromUtf8(spec->title));
        state->view->resize(spec->width > 0 ? spec->width : 800, spec->height > 0 ? spec->height : 600);
        if (!spec->resizable) {
            state->view->setFixedSize(state->view->size());
        }
        state->view->show();
        return 0;
    }
    // Embedded in the app's X11 window (xcb platform only).
    state->view->setAttribute(Qt::WA_NativeWindow);
    state->view->setWindowFlags(Qt::FramelessWindowHint);
    state->view->winId();
    QWindow *parent = QWindow::fromWinId(static_cast<WId>(spec->parent_xid));
    QWindow *handle = state->view->windowHandle();
    if (!parent || !handle) {
        return -2;
    }
    handle->setParent(parent);
    return 0;
}

ISTMO_QT_API void istmo_qt_view_close(uint64_t id) {
    ViewState *state = g_views.take(id);
    if (!state) {
        return;
    }
    state->page->triggerAction(QWebEnginePage::Stop);
    // Deleted now, not later: a deferred delete can run while the
    // process is already tearing Qt's thread storage down.
    delete state->view;
    delete state->page;
    if (state->owns_profile) {
        state->profile->deleteLater();
    }
    delete state;
}

// Replace the view's scripts (applied from the next document on).
ISTMO_QT_API void istmo_qt_view_set_scripts(uint64_t id, const istmo_qt_script *scripts, size_t count) {
    ViewState *s = find(id);
    if (!s) {
        return;
    }
    QWebEngineScriptCollection &collection = s->page->scripts();
    collection.clear();
    for (size_t i = 0; i < count; ++i) {
        QWebEngineScript script;
        script.setName(QStringLiteral("istmo-%1").arg(i));
        script.setSourceCode(QString::fromUtf8(scripts[i].source));
        script.setInjectionPoint(scripts[i].at_end ? QWebEngineScript::DocumentReady
                                                   : QWebEngineScript::DocumentCreation);
        script.setWorldId(scripts[i].isolated ? QWebEngineScript::ApplicationWorld : QWebEngineScript::MainWorld);
        script.setRunsOnSubFrames(scripts[i].subframes != 0);
        collection.insert(script);
    }
}

ISTMO_QT_API void istmo_qt_view_load(uint64_t id, const char *url, const char *const *header_names,
                                     const char *const *header_values, size_t headers, const uint8_t *body,
                                     size_t body_len) {
    ViewState *s = find(id);
    if (!s) {
        return;
    }
    const QUrl target(QString::fromUtf8(url));
    if (headers == 0 && !body) {
        s->page->load(target);
        return;
    }
    QWebEngineHttpRequest request(target, body ? QWebEngineHttpRequest::Post : QWebEngineHttpRequest::Get);
    for (size_t i = 0; i < headers; ++i) {
        request.setHeader(QByteArray(header_names[i]), QByteArray(header_values[i]));
    }
    if (body) {
        request.setPostData(QByteArray(reinterpret_cast<const char *>(body), static_cast<qsizetype>(body_len)));
    }
    s->page->load(request);
}

ISTMO_QT_API void istmo_qt_view_load_html(uint64_t id, const char *html, const char *base) {
    if (ViewState *s = find(id)) {
        s->page->setHtml(QString::fromUtf8(html), base ? QUrl(QString::fromUtf8(base)) : QUrl());
    }
}

// 0 back, 1 forward, 2 reload, 3 reload bypassing cache, 4 stop,
// 5 history offset `offset`.
ISTMO_QT_API void istmo_qt_view_navigate(uint64_t id, int action, int offset) {
    ViewState *s = find(id);
    if (!s) {
        return;
    }
    switch (action) {
    case 0: s->page->triggerAction(QWebEnginePage::Back); break;
    case 1: s->page->triggerAction(QWebEnginePage::Forward); break;
    case 2: s->page->triggerAction(QWebEnginePage::Reload); break;
    case 3: s->page->triggerAction(QWebEnginePage::ReloadAndBypassCache); break;
    case 4: s->page->triggerAction(QWebEnginePage::Stop); break;
    default: {
        QWebEngineHistory *history = s->page->history();
        const int index = history->currentItemIndex() + offset;
        if (index >= 0 && index < history->count()) {
            history->goToItem(history->itemAt(index));
        }
        break;
    }
    }
}

// Calls `item` for every history entry, oldest first; returns the
// current index (-1 for a closed view).
ISTMO_QT_API int istmo_qt_view_history(uint64_t id, istmo_qt_history_item item, void *out) {
    ViewState *s = find(id);
    if (!s) {
        return -1;
    }
    QWebEngineHistory *history = s->page->history();
    const QList<QWebEngineHistoryItem> items = history->items();
    for (const QWebEngineHistoryItem &entry : items) {
        const QByteArray u = full(entry.url());
        const QByteArray t = entry.title().toUtf8();
        item(out, u.constData(), t.constData());
    }
    return history->currentItemIndex();
}

ISTMO_QT_API void istmo_qt_view_geometry(uint64_t id, int x, int y, int w, int h, int visible) {
    ViewState *s = find(id);
    if (!s) {
        return;
    }
    if (!visible || w <= 0 || h <= 0) {
        s->view->hide();
        return;
    }
    // Physical pixels from the app; Qt widgets use logical ones.
    QWindow *handle = s->view->windowHandle();
    const qreal ratio = handle ? handle->devicePixelRatio() : 1.0;
    s->view->setGeometry(QRect(qRound(x / ratio), qRound(y / ratio), qRound(w / ratio), qRound(h / ratio)));
    s->view->show();
}

ISTMO_QT_API void istmo_qt_view_focus(uint64_t id) {
    if (ViewState *s = find(id)) {
        s->view->setFocus();
    }
}

// The script's value must be `{d: json}` / `{e: message}` / `{p: 1}` (a
// promise still settling); anything else is an error.
ISTMO_QT_API void istmo_qt_view_eval(uint64_t id, const char *script, int isolated, uint64_t token) {
    ViewState *s = find(id);
    if (!s) {
        g_cb.eval_done(token, 0, "view closed");
        return;
    }
    const quint32 world = isolated ? QWebEngineScript::ApplicationWorld : QWebEngineScript::MainWorld;
    s->page->runJavaScript(QString::fromUtf8(script), world, [token](const QVariant &value) {
        const QVariantMap map = value.toMap();
        if (map.contains(QStringLiteral("d"))) {
            const QByteArray d = map.value(QStringLiteral("d")).toString().toUtf8();
            g_cb.eval_done(token, 1, d.constData());
        } else if (map.contains(QStringLiteral("p"))) {
            g_cb.eval_done(token, 2, "");
        } else {
            const QByteArray e = map.value(QStringLiteral("e"), QStringLiteral("script failed")).toString().toUtf8();
            g_cb.eval_done(token, 0, e.constData());
        }
    });
}

ISTMO_QT_API void istmo_qt_view_zoom(uint64_t id, double factor) {
    if (ViewState *s = find(id)) {
        s->page->setZoomFactor(factor);
    }
}

ISTMO_QT_API void istmo_qt_view_find(uint64_t id, const char *text, int backwards, int case_sensitive,
                                     uint64_t token) {
    ViewState *s = find(id);
    if (!s) {
        g_cb.find_done(token, 0, 0);
        return;
    }
    QWebEnginePage::FindFlags flags;
    if (backwards) {
        flags |= QWebEnginePage::FindBackward;
    }
    if (case_sensitive) {
        flags |= QWebEnginePage::FindCaseSensitively;
    }
    s->page->findText(QString::fromUtf8(text), flags, [token](const QWebEngineFindTextResult &r) {
        g_cb.find_done(token, r.numberOfMatches(), r.activeMatch());
    });
}

ISTMO_QT_API void istmo_qt_view_clear_data(uint64_t id, int cookies, int cache) {
    ViewState *s = find(id);
    if (!s) {
        return;
    }
    if (cookies) {
        s->profile->cookieStore()->deleteAllCookies();
    }
    if (cache) {
        s->profile->clearHttpCache();
    }
}

// same_site: 0 none, 1 lax, 2 strict. `expires_unix_ms` < 0: session.
ISTMO_QT_API void istmo_qt_view_set_cookie(uint64_t id, const char *url, const char *name, const char *value,
                                           const char *domain, const char *path, int64_t expires_unix_ms,
                                           int secure, int http_only, int same_site) {
    ViewState *s = find(id);
    if (!s) {
        return;
    }
    QNetworkCookie cookie{QByteArray(name), QByteArray(value)};
    if (domain && *domain) {
        cookie.setDomain(QString::fromUtf8(domain));
    }
    cookie.setPath(QString::fromUtf8(path && *path ? path : "/"));
    if (expires_unix_ms >= 0) {
        cookie.setExpirationDate(QDateTime::fromMSecsSinceEpoch(expires_unix_ms, QTimeZone::UTC));
    }
    cookie.setSecure(secure != 0);
    cookie.setHttpOnly(http_only != 0);
    cookie.setSameSitePolicy(same_site == 2   ? QNetworkCookie::SameSite::Strict
                             : same_site == 1 ? QNetworkCookie::SameSite::Lax
                                              : QNetworkCookie::SameSite::None);
    s->profile->cookieStore()->setCookie(cookie, QUrl(QString::fromUtf8(url)));
}

// PNG of the view's widget, `malloc`ed (free with `istmo_qt_free`), or
// NULL.
ISTMO_QT_API uint8_t *istmo_qt_view_snapshot(uint64_t id, size_t *len, int *width, int *height) {
    ViewState *s = find(id);
    if (!s) {
        return nullptr;
    }
    const QPixmap pixmap = s->view->grab();
    QByteArray png;
    QBuffer buffer(&png);
    buffer.open(QIODevice::WriteOnly);
    if (!pixmap.save(&buffer, "PNG")) {
        return nullptr;
    }
    auto *out = static_cast<uint8_t *>(std::malloc(static_cast<size_t>(png.size())));
    if (!out) {
        return nullptr;
    }
    std::memcpy(out, png.constData(), static_cast<size_t>(png.size()));
    *len = static_cast<size_t>(png.size());
    *width = pixmap.width();
    *height = pixmap.height();
    return out;
}

ISTMO_QT_API void istmo_qt_free(void *ptr) { std::free(ptr); }

ISTMO_QT_API int istmo_qt_view_inspector(uint64_t id, int open) {
    ViewState *s = find(id);
    if (!s) {
        return -1;
    }
    QWebEnginePage *tools = s->page->devToolsPage();
    if (!open) {
        if (tools) {
            if (QWidget *window = QWebEngineView::forPage(tools)) {
                window->close();
            }
        }
        return 0;
    }
    if (!tools) {
        auto *window = new QWebEngineView();
        window->setAttribute(Qt::WA_DeleteOnClose);
        window->setWindowTitle(QStringLiteral("Inspector"));
        s->page->setDevToolsPage(window->page());
        window->resize(900, 600);
        window->show();
    }
    return 0;
}

// Response head for `token` (scheme or bridge request). Headers named
// `access-control-allow-origin` with `*` are dropped.
ISTMO_QT_API void istmo_qt_response_head(uint64_t token, int status, const char *mime,
                                         const char *const *header_names, const char *const *header_values,
                                         size_t headers) {
    auto it = g_responses.find(token);
    if (it == g_responses.end()) {
        return;
    }
    QWebEngineUrlRequestJob *job = it->job;
    if (!job) {
        g_responses.erase(it);
        return;
    }
    // Qt jobs carry no status line: scheme errors become failures; bridge
    // answers keep the body, flagged so the page rejects with it.
    if (status >= 400 && !it->bridge) {
        job->fail(status == 404   ? QWebEngineUrlRequestJob::UrlNotFound
                  : status == 403 ? QWebEngineUrlRequestJob::RequestDenied
                                  : QWebEngineUrlRequestJob::RequestFailed);
        g_responses.erase(it);
        return;
    }
    QMultiMap<QByteArray, QByteArray> extra;
    extra.insert(QByteArrayLiteral("X-Content-Type-Options"), QByteArrayLiteral("nosniff"));
    extra.insert(QByteArrayLiteral("Cache-Control"), QByteArrayLiteral("no-cache"));
    if (it->bridge) {
        const QByteArray initiator = origin_of(job->initiator());
        if (!initiator.isEmpty()) {
            extra.insert(QByteArrayLiteral("Access-Control-Allow-Origin"), initiator);
        }
        extra.insert(QByteArrayLiteral("Access-Control-Expose-Headers"), QByteArrayLiteral("X-Istmo-Error"));
        if (status >= 400) {
            extra.insert(QByteArrayLiteral("X-Istmo-Error"), QByteArrayLiteral("1"));
        }
    }
    for (size_t i = 0; i < headers; ++i) {
        const QByteArray name(header_names[i]);
        const QByteArray value(header_values[i]);
        if (name.compare("access-control-allow-origin", Qt::CaseInsensitive) == 0 && value == "*") {
            continue;
        }
        extra.insert(name, value);
    }
    job->setAdditionalResponseHeaders(extra);
    auto *body = new ChunkDevice(job);
    it->body = body;
    job->reply(QByteArray(mime), body);
}

ISTMO_QT_API void istmo_qt_response_data(uint64_t token, const uint8_t *data, size_t len) {
    auto it = g_responses.find(token);
    if (it == g_responses.end()) {
        return;
    }
    if (ChunkDevice *body = it->body) {
        body->push(reinterpret_cast<const char *>(data), static_cast<qsizetype>(len));
    }
}

// End the body; `abort` fails a job that has no head yet or cuts the
// stream short.
ISTMO_QT_API void istmo_qt_response_end(uint64_t token, int abort) {
    auto it = g_responses.find(token);
    if (it == g_responses.end()) {
        return;
    }
    const Response response = *it;
    g_responses.erase(it);
    if (abort || !response.body) {
        if (response.job) {
            response.job->fail(QWebEngineUrlRequestJob::RequestAborted);
        }
        return;
    }
    response.body->finish();
}

ISTMO_QT_API void istmo_qt_permission_reply(uint64_t token, int allow) {
    auto it = g_permissions.find(token);
    if (it == g_permissions.end()) {
        return;
    }
    const QWebEnginePermission permission = *it;
    g_permissions.erase(it);
    if (allow) {
        permission.grant();
    } else {
        permission.deny();
    }
}

// Thread-safe: close every view and leave the event loop (process exit).
ISTMO_QT_API void istmo_qt_quit(void) {
    QMetaObject::invokeMethod(
        g_app,
        [] {
            const QList<uint64_t> ids = g_views.keys();
            for (const uint64_t id : ids) {
                istmo_qt_view_close(id);
            }
            g_app->quit();
        },
        Qt::QueuedConnection);
}
