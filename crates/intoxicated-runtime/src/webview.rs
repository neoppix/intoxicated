//! The engine's web view, which this client does not have: whatever the
//! engine asks to show in one is handed to the user's browser.
//!
//! Three parts:
//!
//! - [`user_agent`]: the User-Agent `setWebviewUserAgent` is told. It is read
//!   back from the native builder (`intoxicated_build_user_agent` in
//!   `native/jni_values.cpp`) that also fills `InitParams.userAgent`, so the
//!   web view and the engine's HTTP client cannot disagree. Its exact shape
//!   per device identity is pinned by the tests below, because roblox.com
//!   decides what to serve from it.
//! - The `WebViewProtocol` vocabulary: message and key names read from the
//!   engine's own getters, never hardcoded.
//! - [`arm`]: the `openWindow` subscription. A message is parsed on the
//!   engine thread that published it and handed to the presenter
//!   [`set_presenter`] installed, which in this client is
//!   `desktop::open_url` (http and https only).
//!
//! The URL in an `openWindow` is never printed, only its scheme and host: its
//! query can carry a one-time sign-in ticket.
//!
//! Measured on 2.748: the subscription is live, and an `openWindow` published
//! on the engine's bus (`INTOXICATED_WEBVIEW_TEST`) arrives, parses and reaches
//! the hand-off. That a real click in the app shell publishes one in this
//! shape is INFERRED; none was captured.

use std::ffi::{c_char, c_void};
use std::fmt;
use std::sync::OnceLock;

use intoxicated_linker_sys as linker;

use crate::bus;

pub const PROTOCOL_CLASS: &str = "com/roblox/protocols/webview/WebViewProtocol";
const GETTER_PREFIX: &str = "Java_com_roblox_protocols_webview_WebViewProtocol_";
const INITIALIZE: &str = "Java_com_roblox_protocols_webview_WebViewProtocol_initializeAndroidWebViewProtocol";

/// Set to a URL: once `openWindow` is subscribed, publish a synthetic
/// `openWindow` for it on the engine's bus, so the subscription, the parse and
/// the hand-off run exactly as for a real one. A `file:` URL shows the whole
/// path without opening anything.
pub const TEST_VARIABLE: &str = "INTOXICATED_WEBVIEW_TEST";

/// Label and getter, in the order they are read and reported.
const GETTERS: [(&str, &str); 16] = [
    ("protocol", "getProtocolName"),
    ("open-window", "getOpenWindowId"),
    ("close-window", "getCloseWindowId"),
    ("mutate-window", "getMutateWindowId"),
    ("handle-window-close", "getHandleWindowCloseId"),
    ("is-available", "getIsAvailableId"),
    ("key:url", "getUrlKey"),
    ("key:title", "getTitleKey"),
    ("key:window-type", "getWindowTypeKey"),
    ("key:search-params", "getSearchParamsKey"),
    ("key:search-type", "getSearchTypeKey"),
    ("key:is-visible", "getIsVisibleKey"),
    ("key:hide-header", "getHideHeaderKey"),
    ("key:back-button-visible", "getBackButtonVisibleKey"),
    ("key:show-domain-as-title", "getShowDomainAsTitleKey"),
    ("key:available", "getAvailableKey"),
];

/// The keys an `openWindow` is parsed with.
const OPEN_WINDOW_KEYS: [&str; 5] =
    ["key:url", "key:title", "key:hide-header", "key:back-button-visible", "key:show-domain-as-title"];

// ------------------------------------------------------------ User-Agent

unsafe extern "C" {
    fn intoxicated_build_user_agent(buf: *mut c_char, n: usize) -> usize;
}

/// Room for the read-back. A longer User-Agent is treated as unreadable.
const USER_AGENT_ROOM: usize = 2048;

/// The User-Agent the native layer put in `InitParams.userAgent`, byte for
/// byte, or `None` when it cannot be read back whole: empty, too long for
/// the buffer, or not UTF-8.
pub fn user_agent() -> Option<String> {
    let mut buf = vec![0u8; USER_AGENT_ROOM];
    // SAFETY: the C side writes at most `buf.len()` bytes, NUL included, and
    // returns the full length.
    let len = unsafe { intoxicated_build_user_agent(buf.as_mut_ptr().cast(), buf.len()) };
    if len == 0 || len >= buf.len() {
        return None;
    }
    buf.truncate(len);
    String::from_utf8(buf).ok()
}

// ------------------------------------------------------------ vocabulary

/// What the engine's `WebViewProtocol` getters answered.
pub struct Vocabulary {
    /// Label and answer, for each exported getter; an `Err` is a call that failed.
    answers: Vec<(&'static str, Result<String, String>)>,
    /// Getters this build does not export.
    missing: Vec<&'static str>,
}

impl Vocabulary {
    /// The answer under `label`, when the getter is exported and answered.
    pub fn get(&self, label: &str) -> Option<&str> {
        self.answers.iter().find(|(l, _)| *l == label).and_then(|(_, a)| a.as_deref().ok())
    }
}

/// Reads the sixteen getters through `symbol`, which resolves a name in the
/// loaded engine.
pub fn read_vocabulary(symbol: impl Fn(&str) -> Option<*mut c_void>) -> Vocabulary {
    read_vocabulary_with(|getter| {
        symbol(&format!("{GETTER_PREFIX}{getter}")).map(|f| linker::game_activity::call_static_ret_string(f, PROTOCOL_CLASS))
    })
}

/// `call` is `None` for a getter that is not exported.
fn read_vocabulary_with(call: impl Fn(&str) -> Option<Result<String, String>>) -> Vocabulary {
    let mut vocabulary = Vocabulary { answers: Vec::new(), missing: Vec::new() };
    for (label, getter) in GETTERS {
        match call(getter) {
            None => vocabulary.missing.push(getter),
            Some(answer) => vocabulary.answers.push((label, answer)),
        }
    }
    vocabulary
}

fn report_lines(v: &Vocabulary) -> Vec<String> {
    if v.answers.is_empty() && v.missing.is_empty() {
        return vec!["  webview: WebViewProtocol is not exported by this build".into()];
    }
    let mut out = vec!["  webview: protocol vocabulary, read from the engine".to_string()];
    for (label, answer) in &v.answers {
        let value = match answer {
            Ok(value) => value.clone(),
            Err(e) => format!("<error: {e}>"),
        };
        out.push(format!("    {label:<26} {value}"));
    }
    if !v.missing.is_empty() {
        out.push(format!("    not exported by this build: {}", v.missing.join(", ")));
    }
    out
}

/// The early report, once, before `nativeRetryInit`.
pub fn report(v: &Vocabulary) {
    for line in report_lines(v) {
        println!("{line}");
    }
}

/// The two bus natives the `openWindow` subscription needs.
pub struct BusNatives {
    pub get_message_id: bool,
    pub do_subscribe_raw: bool,
}

pub fn find_bus_natives(symbol: impl Fn(&str) -> Option<*mut c_void>) -> BusNatives {
    BusNatives {
        get_message_id: symbol(bus::GET_MESSAGE_ID).is_some(),
        do_subscribe_raw: symbol(bus::DO_SUBSCRIBE_RAW).is_some(),
    }
}

pub fn report_bus_natives(n: &BusNatives) {
    println!("  webview: MessageBus.getMessageId exported: {}", n.get_message_id);
    println!("  webview: MessageBus.doSubscribeRaw exported: {}", n.do_subscribe_raw);
    if n.get_message_id && n.do_subscribe_raw {
        println!("  webview: both natives are present; openWindow is subscribed later, from arm()");
    }
}

// ---------------------------------------------------------- the message

/// The key names an `openWindow` is parsed with, as this build spells them.
struct Keys {
    url: String,
    title: String,
    hide_header: String,
    back_button_visible: String,
    show_domain_as_title: String,
}

impl Keys {
    /// All five, or the labels of the ones missing.
    fn from(v: &Vocabulary) -> Result<Keys, Vec<&'static str>> {
        let missing: Vec<&'static str> = OPEN_WINDOW_KEYS.into_iter().filter(|l| v.get(l).is_none()).collect();
        if !missing.is_empty() {
            return Err(missing);
        }
        let key = |label: &str| v.get(label).unwrap_or_default().to_string();
        Ok(Keys {
            url: key("key:url"),
            title: key("key:title"),
            hide_header: key("key:hide-header"),
            back_button_visible: key("key:back-button-visible"),
            show_domain_as_title: key("key:show-domain-as-title"),
        })
    }
}

static KEYS: OnceLock<Keys> = OnceLock::new();
/// The bus id `openWindow` was subscribed under.
static BUS_ID: OnceLock<String> = OnceLock::new();

/// A parsed `openWindow`. `Debug` shows the scheme and host, never the URL.
pub struct WindowRequest {
    pub url: String,
    pub title: Option<String>,
    pub hide_header: bool,
    pub back_button_visible: bool,
    pub show_domain_as_title: bool,
}

impl fmt::Debug for WindowRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (scheme, host) = scheme_and_host(&self.url);
        f.debug_struct("WindowRequest")
            .field("scheme", &scheme)
            .field("host", &host)
            .field("hide_header", &self.hide_header)
            .field("back_button_visible", &self.back_button_visible)
            .field("show_domain_as_title", &self.show_domain_as_title)
            .finish_non_exhaustive()
    }
}

fn parse_open_window(text: &str, keys: &Keys) -> Result<WindowRequest, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("openWindow payload is not JSON: {e}"))?;
    let Some(object) = value.as_object() else {
        return Err("openWindow payload is not a JSON object".into());
    };
    let url = match object.get(&keys.url).and_then(|u| u.as_str()) {
        Some(url) if !url.is_empty() => url.to_string(),
        _ => return Err(format!("openWindow payload has no non-empty \"{}\" field", keys.url)),
    };
    let flag = |key: &str| object.get(key) == Some(&serde_json::Value::Bool(true));
    Ok(WindowRequest {
        url,
        title: object.get(&keys.title).and_then(|t| t.as_str()).map(str::to_string),
        hide_header: flag(&keys.hide_header),
        back_button_visible: flag(&keys.back_button_visible),
        show_domain_as_title: flag(&keys.show_domain_as_title),
    })
}

/// `text` safe for one log line: printable ASCII, at most 64 characters.
fn printable(text: &str) -> String {
    let mut out: String =
        text.chars().take(64).map(|c| if ('\x21'..='\x7e').contains(&c) { c } else { '?' }).collect();
    if text.chars().count() > 64 {
        out.push_str("...");
    }
    if out.is_empty() { "none".into() } else { out }
}

/// The only parts of a URL that are printed: the scheme as written, and the
/// authority without any user information (port kept). `none` for both when
/// the URL has no `://`.
fn scheme_and_host(url: &str) -> (String, String) {
    let Some((scheme, rest)) = url.split_once("://") else { return ("none".into(), "none".into()) };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    (printable(scheme), printable(host))
}

type Presenter = Box<dyn Fn(&WindowRequest) + Send + Sync>;
static PRESENTER: OnceLock<Presenter> = OnceLock::new();

/// Installs what an `openWindow` is handed to. Only the first one counts.
pub fn set_presenter(presenter: impl Fn(&WindowRequest) + Send + Sync + 'static) {
    if PRESENTER.set(Box::new(presenter)).is_err() {
        println!("  webview: set_presenter called twice; the first presenter installed is the one in effect");
    }
}

/// The line printed for one payload, and the request when it parsed.
fn receive(payload: Option<&[u8]>, keys: Option<&Keys>) -> (String, Option<WindowRequest>) {
    let Some(bytes) = payload else { return ("[webview] openWindow published with a null payload".into(), None) };
    let n = bytes.len();
    let Ok(text) = std::str::from_utf8(bytes) else {
        return (format!("[webview] openWindow message arrived: {n} bytes, not valid UTF-8"), None);
    };
    let Some(keys) = keys else {
        return (
            format!(
                "[webview] openWindow message arrived: {n} bytes, but this build's key vocabulary was never read \
                 (arm() must run first, and must find getUrlKey and friends)"
            ),
            None,
        );
    };
    match parse_open_window(text, keys) {
        Err(reason) => (format!("[webview] openWindow message arrived: {n} bytes, but could not be parsed: {reason}"), None),
        Ok(request) => {
            let (scheme, host) = scheme_and_host(&request.url);
            (
                format!(
                    "[webview] openWindow message arrived: {n} bytes, host {host} (scheme {scheme}), hideHeader {}, \
                     backButtonVisible {}, showDomainAsTitle {}",
                    request.hide_header, request.back_button_visible, request.show_domain_as_title
                ),
                Some(request),
            )
        }
    }
}

/// Runs on whichever engine thread published. Parses and hands off; never
/// touches the window system.
extern "C" fn on_open_window(payload: *const c_char) {
    // SAFETY: the bus hands a NUL-terminated string valid for the call, or null.
    let (line, request) = receive(unsafe { bus::payload_bytes(payload) }, KEYS.get());
    println!("{line}");
    if let Some(request) = request {
        match PRESENTER.get() {
            Some(present) => present(&request),
            None => println!("[webview] openWindow arrived, but no presenter is installed yet; nothing is opened"),
        }
    }
}

/// Subscribes to `openWindow`, once, just before the looper pump starts.
pub fn arm(symbol: impl Fn(&str) -> Option<*mut c_void>) {
    match symbol(INITIALIZE) {
        None => println!("  webview: initializeAndroidWebViewProtocol is not exported by this build"),
        Some(f) => match linker::game_activity::call_bare_on(f, PROTOCOL_CLASS) {
            Ok(()) => println!("  webview: initializeAndroidWebViewProtocol ok"),
            Err(e) => println!("  webview: initializeAndroidWebViewProtocol failed: {e}"),
        },
    }

    let vocabulary = read_vocabulary(&symbol);
    match Keys::from(&vocabulary) {
        Ok(keys) => {
            if KEYS.set(keys).is_err() {
                println!("  webview: arm() called twice; keeping the first build's key vocabulary");
            }
        }
        Err(missing) => println!(
            "  webview: this build is missing {} of openWindow's key getters ({}); a message will arrive and \
             cannot be parsed until they are",
            missing.len(),
            missing.join(", ")
        ),
    }

    let (Some(protocol), Some(open_window)) = (vocabulary.get("protocol"), vocabulary.get("open-window")) else {
        println!(
            "  webview: cannot subscribe to openWindow without getProtocolName and getOpenWindowId (see the \
             vocabulary reported above)"
        );
        return;
    };
    let Some(get_message_id) = symbol(bus::GET_MESSAGE_ID) else {
        println!("  webview: MessageBus.getMessageId is not exported; not subscribing");
        return;
    };
    let bus_id = match linker::game_activity::call_static_two_strings_ret_string(
        get_message_id,
        bus::MESSAGE_BUS_CLASS,
        protocol,
        open_window,
    ) {
        Ok(id) => id,
        Err(e) => {
            println!("  webview: getMessageId(\"{protocol}\", \"{open_window}\") failed: {e}");
            return;
        }
    };
    let Some(do_subscribe_raw) = symbol(bus::DO_SUBSCRIBE_RAW) else {
        println!("  webview: MessageBus.doSubscribeRaw is not exported; openWindow will do nothing");
        return;
    };
    if bus_id.contains('\0') {
        println!("  webview: the bus id getMessageId returned has a NUL in it: {bus_id:?}");
        return;
    }
    if let Err(e) = bus::subscribe(do_subscribe_raw, &bus_id, Some(on_open_window)) {
        println!("  webview: doSubscribeRaw failed: {e}");
        return;
    }
    match bus::connected(symbol(bus::IS_CONNECTED), &bus_id) {
        Some(true) => {
            println!("  webview: subscribed to {bus_id} ({protocol}.{open_window}); the bus says it is live")
        }
        Some(false) => println!("  webview: subscribed to {bus_id}, but the bus says the connection is not live"),
        None => println!("  webview: subscribed to {bus_id}; nothing here can confirm it (no Connection came back)"),
    }
    let _ = BUS_ID.set(bus_id);
}

/// [`TEST_VARIABLE`]: publishes one synthetic `openWindow` for `url` on the
/// engine's bus (title `INTOXICATED_WEBVIEW_TEST`, header shown, back button
/// and domain shown). Prints the URL's length, not the URL.
pub fn publish_test_window(symbol: impl Fn(&str) -> Option<*mut c_void>, url: &str) {
    let (Some(bus_id), Some(keys)) = (BUS_ID.get(), KEYS.get()) else {
        println!("[webview] {TEST_VARIABLE} set, but openWindow is not subscribed; nothing to drive");
        return;
    };
    let Some(publish_raw) = symbol(bus::PUBLISH_RAW) else {
        println!("[webview] {TEST_VARIABLE} set, but MessageBus.publishRaw is not exported; nothing to drive");
        return;
    };
    let mut message = serde_json::Map::new();
    message.insert(keys.url.clone(), url.into());
    message.insert(keys.title.clone(), TEST_VARIABLE.into());
    message.insert(keys.hide_header.clone(), false.into());
    message.insert(keys.back_button_visible.clone(), true.into());
    message.insert(keys.show_domain_as_title.clone(), true.into());
    let payload = serde_json::Value::Object(message).to_string();
    println!("  webview: {TEST_VARIABLE} set; publishing a synthetic openWindow for a {}-byte URL on {bus_id}", url.len());
    match bus::publish(publish_raw, bus_id, &payload) {
        Ok(()) => println!("  webview: {TEST_VARIABLE} published"),
        Err(e) => println!("  webview: {TEST_VARIABLE} could not be published: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measured_keys() -> Keys {
        Keys {
            url: "url".into(),
            title: "title".into(),
            hide_header: "hideHeader".into(),
            back_button_visible: "backButtonVisible".into(),
            show_domain_as_title: "showDomainAsTitle".into(),
        }
    }

    fn parse(text: &str) -> Result<WindowRequest, String> {
        parse_open_window(text, &measured_keys())
    }

    #[test]
    fn an_open_window_is_parsed_with_the_engines_keys() {
        let r = parse(r#"{"url":"https://www.roblox.com/games/mock","hideHeader":false}"#).unwrap();
        assert_eq!(r.url, "https://www.roblox.com/games/mock");
        assert_eq!(r.title, None);
        assert!(!r.hide_header && !r.back_button_visible && !r.show_domain_as_title);
        let r = parse(r#"{"url":"https://x/","hideHeader":true,"backButtonVisible":"yes"}"#).unwrap();
        assert!(r.hide_header);
        assert!(!r.back_button_visible, "only JSON true is true");
        let r = parse(r#"{"url":"https://x/","title":"Marketplace","windowType":2,"isVisible":true}"#).unwrap();
        assert_eq!(r.title.as_deref(), Some("Marketplace"));
        let no_url = "openWindow payload has no non-empty \"url\" field";
        for payload in [r#"{"title":"Marketplace"}"#, r#"{"url":""}"#, r#"{"url":123}"#] {
            assert_eq!(parse(payload).unwrap_err(), no_url);
        }
        assert!(parse("not json").unwrap_err().starts_with("openWindow payload is not JSON: "));
        assert_eq!(parse("[]").unwrap_err(), "openWindow payload is not a JSON object");
        assert_eq!(parse(r#"{"url":"file:///etc/passwd"}"#).unwrap().url, "file:///etc/passwd");
    }

    #[test]
    fn only_the_scheme_and_host_of_a_url_are_shown() {
        let sh = |url: &str| scheme_and_host(url);
        assert_eq!(sh("https://www.roblox.com/home"), ("https".into(), "www.roblox.com".into()));
        assert_eq!(sh("https://u:p@www.roblox.com:443/x?y").1, "www.roblox.com:443");
        assert_eq!(sh("HTTPS://Example.COM"), ("HTTPS".into(), "Example.COM".into()));
        assert_eq!(sh("no-scheme"), ("none".into(), "none".into()));
        assert_eq!(sh("https://a\nb/x").1, "a?b");
        let payload = r#"{"url":"https://www.roblox.com/login?ticket=SECRET","hideHeader":true}"#;
        let (line, request) = receive(Some(payload.as_bytes()), Some(&measured_keys()));
        assert!(request.is_some());
        assert!(!line.contains("SECRET") && !line.contains("/login"), "{line}");
        assert_eq!(
            line,
            format!(
                "[webview] openWindow message arrived: {} bytes, host www.roblox.com (scheme https), hideHeader true, \
                 backButtonVisible false, showDomainAsTitle false",
                payload.len()
            )
        );
        assert!(!format!("{:?}", request.unwrap()).contains("SECRET"));
    }

    #[test]
    fn unreadable_messages_say_why() {
        let keys = measured_keys();
        assert_eq!(receive(None, Some(&keys)).0, "[webview] openWindow published with a null payload");
        assert_eq!(
            receive(Some(&b"\xff"[..]), Some(&keys)).0,
            "[webview] openWindow message arrived: 1 bytes, not valid UTF-8"
        );
        assert!(receive(Some(&b"{}"[..]), None).0.contains("key vocabulary was never read"));
        let (line, request) = receive(Some(&b"[]"[..]), Some(&keys));
        assert_eq!(line, "[webview] openWindow message arrived: 2 bytes, but could not be parsed: openWindow payload is not a JSON object");
        assert!(request.is_none());
    }

    #[test]
    fn the_hand_off_opens_only_web_urls() {
        for url in ["file:///etc/passwd", "javascript:alert(1)", "mailto:a@b", "example.com"] {
            assert!(crate::desktop::open_url(url).is_err(), "{url}");
        }
        assert_eq!(
            crate::desktop::open_url("file:///etc/passwd").unwrap_err(),
            "scheme \"file\" is refused; only http and https may be opened"
        );
    }

    #[test]
    fn the_vocabulary_report_is_aligned_and_names_what_is_missing() {
        let v = read_vocabulary_with(|getter| match getter {
            "getProtocolName" => Some(Ok("WebView".into())),
            "getOpenWindowId" => Some(Ok("openWindow".into())),
            "getUrlKey" => Some(Err("boom".into())),
            _ => None,
        });
        let lines = report_lines(&v);
        assert_eq!(lines[0], "  webview: protocol vocabulary, read from the engine");
        assert_eq!(lines[1], "    protocol                   WebView");
        assert_eq!(lines[2], "    open-window                openWindow");
        assert_eq!(lines[3], "    key:url                    <error: boom>");
        assert!(lines[4].starts_with("    not exported by this build: getCloseWindowId, getMutateWindowId"));
        assert_eq!(v.get("key:url"), None, "a failed getter is not a key");
        assert_eq!(Keys::from(&v).err().unwrap(), OPEN_WINDOW_KEYS);
        let empty = Vocabulary { answers: Vec::new(), missing: Vec::new() };
        assert_eq!(report_lines(&empty), ["  webview: WebViewProtocol is not exported by this build"]);
    }

    // ------------------------------------------------------ User-Agent

    /// The documented format per identity, used only to check the native
    /// builder's output; the client never builds the string in Rust.
    fn documented(identity: &str, ram_mb: u64, w: u32, h: u32, engine: &str) -> String {
        let parts: Vec<&str> = engine.split('.').collect();
        let app = if parts.len() == 4 { format!("{}.{}.{}", parts[0], parts[1], parts[3]) } else { "0.0.0".into() };
        match identity {
            "pc-windows-11" => format!(
                "Mozilla/5.0 ({ram_mb}MB; {w}x{h}; 160x160; {w}x{h}; Intoxicated) AppleWebKit/537.36 (KHTML, like Gecko)  \
                 ROBLOX Windows App {app} Desktop Hybrid()  Intoxicated RobloxApp/{app} (GlobalDist; Intoxicated)"
            ),
            "android-tablet" => format!(
                "Mozilla/5.0 ({ram_mb}MB; {w}x{h}; 160x160; {w}x{h}; Intoxicated; 33) AppleWebKit/537.36 (KHTML, like Gecko)  \
                 ROBLOX Android App {app} Tablet Hybrid()  GooglePlayStore RobloxApp/{app} (GlobalDist; GooglePlayStore)"
            ),
            "roblox-app" => format!("RobloxApp/{app}(GlobalDist; Intoxicated)"),
            other => panic!("no identity {other}"),
        }
    }

    #[test]
    fn the_documented_formats_match_the_measured_strings() {
        assert_eq!(
            documented("pc-windows-11", 32003, 1280, 720, "2.748.0.1527"),
            "Mozilla/5.0 (32003MB; 1280x720; 160x160; 1280x720; Intoxicated) AppleWebKit/537.36 (KHTML, like Gecko)  \
             ROBLOX Windows App 2.748.1527 Desktop Hybrid()  Intoxicated RobloxApp/2.748.1527 (GlobalDist; Intoxicated)"
        );
        assert_eq!(
            documented("android-tablet", 32003, 1280, 720, "2.748.0.1527"),
            "Mozilla/5.0 (32003MB; 1280x720; 160x160; 1280x720; Intoxicated; 33) AppleWebKit/537.36 (KHTML, like Gecko)  \
             ROBLOX Android App 2.748.1527 Tablet Hybrid()  GooglePlayStore RobloxApp/2.748.1527 (GlobalDist; GooglePlayStore)"
        );
        assert_eq!(documented("roblox-app", 32003, 1280, 720, "2.748.0.1527"), "RobloxApp/2.748.1527(GlobalDist; Intoxicated)");
        assert_eq!(documented("roblox-app", 1, 1, 1, "2.748.1527"), "RobloxApp/0.0.0(GlobalDist; Intoxicated)");
    }

    /// Run as a child by `the_native_user_agent_has_the_exact_format_per_identity`:
    /// prints the read-back between markers.
    #[test]
    fn print_native_user_agent_for_parent() {
        if std::env::var_os("INTOXICATED_UA_CHILD").is_none() {
            return;
        }
        println!("UA[{}]UA", user_agent().expect("the native builder answers"));
    }

    fn native_user_agent(env: &[(&str, &str)]) -> String {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "webview::tests::print_native_user_agent_for_parent", "--nocapture", "--test-threads=1"])
            .env("INTOXICATED_UA_CHILD", "1")
            .env_remove("INTOXICATED_UA")
            .env_remove("INTOXICATED_DEVICE_PROFILE")
            .env_remove("INTOXICATED_ENGINE_VERSION");
        for (k, v) in env {
            child.env(k, v);
        }
        let out = child.output().expect("the test binary runs");
        let stdout = String::from_utf8(out.stdout).unwrap();
        let start = stdout.find("UA[").expect("the child printed a User-Agent") + 3;
        let end = stdout.find("]UA").unwrap();
        stdout[start..end].to_string()
    }

    /// The memory figure is this machine's, so it is read off the answer.
    fn ram_in(ua: &str) -> u64 {
        let open = ua.find('(').unwrap() + 1;
        let mb = ua[open..].find("MB").unwrap();
        ua[open..open + mb].parse().unwrap()
    }

    #[test]
    fn the_native_user_agent_has_the_exact_format_per_identity() {
        let version = ("INTOXICATED_ENGINE_VERSION", "2.748.0.1527");
        for (profile, identity) in [
            (None, "pc-windows-11"),
            (Some("windows"), "pc-windows-11"),
            (Some(" Android-Tablet "), "android-tablet"),
            (Some("tablet"), "android-tablet"),
            (Some("roblox-app"), "roblox-app"),
            (Some("app"), "roblox-app"),
            (Some("tablett"), "pc-windows-11"),
        ] {
            let mut env = vec![version];
            if let Some(p) = profile {
                env.push(("INTOXICATED_DEVICE_PROFILE", p));
            }
            let ua = native_user_agent(&env);
            let ram = if identity == "roblox-app" { 0 } else { ram_in(&ua) };
            assert!(identity == "roblox-app" || ram > 0, "a real memory figure: {ua}");
            assert_eq!(ua, documented(identity, ram, 1280, 720, "2.748.0.1527"), "{profile:?}");
        }
        assert_eq!(native_user_agent(&[("INTOXICATED_DEVICE_PROFILE", "roblox"), ("INTOXICATED_ENGINE_VERSION", "2.748.1527")]),
            "RobloxApp/0.0.0(GlobalDist; Intoxicated)");
        assert_eq!(native_user_agent(&[("INTOXICATED_DEVICE_PROFILE", "app")]), "RobloxApp/0.0.0(GlobalDist; Intoxicated)");
        assert_eq!(native_user_agent(&[("INTOXICATED_UA", "Roblox/WinInet"), version]), "Roblox/WinInet");
    }
}
